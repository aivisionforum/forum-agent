//! File-only production capture -> privately owned Dora ASR -> durable core probe.
//! No translator, microphone, speaker, system capture, or download path is used.
use anyhow::{anyhow, ensure, Context, Result};
use dora_node_api::{DoraNode, IntoArrow, Parameter};
use forum_contracts::{
    Event, EventType, Producer, SessionSpec, SessionState, SessionTransition, TrackKind, TrackSpec,
    Uuid, SCHEMA_VERSION,
};
use forum_core::CoreHandle;
use forum_runtime::{DurableProducer, Endpoint, RpcError, RuntimeConfig, UdsServer};
use moxin_dora_bridge::{CaptureContext, CaptureSession, DataflowController};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

fn transition(
    core: &CoreHandle,
    session: &SessionSpec,
    run: Uuid,
    seq: &mut u64,
    from: SessionState,
    to: SessionState,
) -> Result<()> {
    *seq += 1;
    let event = Event {
        schema_version: SCHEMA_VERSION,
        message_id: Uuid::new_v4(),
        event_type: EventType::SessionChanged,
        event_id: session.event_id,
        room_id: session.room_id,
        session_id: session.session_id,
        producer: Producer {
            name: "probe-host".into(),
            run_id: run,
            seq: *seq,
        },
        payload: SessionTransition {
            expected_state: from,
            next_state: to,
            reason: "file-only production ASR validation".into(),
        },
    };
    core.ingest_json(serde_json::to_value(event)?)?;
    Ok(())
}
fn wave(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "not a WAV"
    );
    let mut i = 12;
    let mut valid = false;
    let mut output = None;
    while i + 8 <= bytes.len() {
        let n = u32::from_le_bytes(bytes[i + 4..i + 8].try_into()?) as usize;
        let body = bytes.get(i + 8..i + 8 + n).context("truncated WAV")?;
        if &bytes[i..i + 4] == b"fmt " {
            ensure!(n >= 16, "short WAV fmt");
            valid = u16::from_le_bytes(body[0..2].try_into()?) == 1
                && u16::from_le_bytes(body[2..4].try_into()?) == 1
                && u32::from_le_bytes(body[4..8].try_into()?) == 16000
                && u16::from_le_bytes(body[14..16].try_into()?) == 16;
        }
        if &bytes[i..i + 4] == b"data" {
            output = Some(
                body.chunks_exact(2)
                    .map(|b| i16::from_le_bytes(b.try_into().unwrap()) as f32 / 32768.0)
                    .collect(),
            );
        }
        i += 8 + n + n % 2;
    }
    ensure!(valid, "probe requires mono 16kHz PCM16 WAV");
    output.context("missing WAV data")
}
fn main() -> Result<()> {
    let repo = std::env::current_dir()?.canonicalize()?;
    let run = Uuid::new_v4();
    let root = PathBuf::from("/tmp").join(format!("f03-asr-{run}"));
    fs::DirBuilder::new().mode(0o700).create(&root)?;
    let artifacts = repo
        .join("artifacts/local/f03-durable-asr")
        .join(run.to_string());
    fs::create_dir_all(&artifacts)?;
    let session = SessionSpec {
        session_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        room_id: Uuid::new_v4(),
        owner_device_id: Uuid::new_v4(),
        title: "ASR source independent of translation".into(),
    };
    let track = TrackSpec {
        track_id: Uuid::new_v4(),
        session_id: session.session_id,
        kind: TrackKind::Replay,
        sample_rate: 16000,
    };
    let sid = session.session_id;
    let tid = track.track_id;
    let core = CoreHandle::open(root.join("meeting.sqlite"), 32)?;
    let s = session.clone();
    let t = track.clone();
    core.call(move |store| {
        store.create_session(&s)?;
        store.create_track(&t)
    })?;
    let mut seq = 0;
    transition(
        &core,
        &session,
        run,
        &mut seq,
        SessionState::Created,
        SessionState::Preparing,
    )?;
    transition(
        &core,
        &session,
        run,
        &mut seq,
        SessionState::Preparing,
        SessionState::Ready,
    )?;
    transition(
        &core,
        &session,
        run,
        &mut seq,
        SessionState::Ready,
        SessionState::Recording,
    )?;
    let endpoint = Endpoint::new(root.join("core.sock"));
    let config = RuntimeConfig {
        endpoint: endpoint.clone(),
        session: session.clone(),
        producer_dir: root.join("producers"),
        producer_name: "node".into(),
    };
    let ready = Arc::new(AtomicBool::new(false));
    let readiness = ready.clone();
    let reject_once = Arc::new(AtomicBool::new(true));
    let rejected = reject_once.clone();
    let writer = core.clone();
    let mut server = UdsServer::bind(endpoint, move |req| match req.method.as_str() {
        "ingest" => {
            let event = req.params["event"].clone();
            let lost_ack =
                event["type"] == "transcript.final" && rejected.swap(false, Ordering::AcqRel);
            let receipt = writer
                .ingest_json(event)
                .map_err(|e| RpcError::new(e.code(), e.retryable(), e))?;
            if lost_ack {
                return Err(RpcError::new(
                    "INJECTED_ACK_LOSS",
                    true,
                    "committed final but omitted first receipt",
                ));
            }
            Ok(serde_json::to_value(receipt).unwrap())
        }
        "ready" => {
            if req.params["role"] != "asr" {
                return Err(RpcError::new(
                    "UNEXPECTED_ROLE",
                    false,
                    "translator must be absent",
                ));
            }
            readiness.store(true, Ordering::Release);
            Ok(json!({"accepted":true}))
        }
        "status" => {
            let r = readiness.load(Ordering::Acquire);
            writer.call(move|store|Ok(json!({"ready_roles":if r{vec!["asr"]}else{vec![]},"capture_stopped":store.session_status(sid)?.capture_stopped,"capture_manifest_sha256":store.capture_seal(sid)?.map(|s|s.manifest_sha256),"registered_segment_ids":store.registered_segment_ids(sid,tid)?,"terminal_segment_ids":store.terminal_segment_ids(sid)?,"asr_terminal_segment_ids":store.terminal_segment_ids(sid)?,"capture_dispatch_complete":true,"replay_expected_revisions":[],"recovery_segments":store.recovery_segments(sid,100)?}))).map_err(|e|RpcError::new(e.code(),e.retryable(),e))
        }
        _ => Err(RpcError::new(
            "UNEXPECTED_RPC",
            false,
            "only ASR source methods allowed",
        )),
    })?;
    let asr = PathBuf::from(
        std::env::var("FORUM_PROBE_ASR_BIN")
            .unwrap_or_else(|_| "/tmp/aivf-cargo-target/debug/dora-qwen3-asr".into()),
    );
    ensure!(asr.is_file(), "build ASR binary first");
    let graph = json!({"nodes":[{"id":"moxin-file-capture","path":"dynamic","outputs":["audio_segment"]},{"id":"qwen3-asr","path":asr,"inputs":{"audio":"moxin-file-capture/audio_segment"},"outputs":["transcription","log"]}]});
    let graph_path = root.join("flow.yml");
    fs::write(&graph_path, serde_yaml::to_string(&graph)?)?;
    let mut controller = DataflowController::new(&graph_path)?;
    controller.set_env("FORUM_RUNTIME_CONFIG", serde_json::to_string(&config)?);
    controller.set_env("ASR_SOURCE_LANGUAGE", "auto");
    for key in [
        "FORUM_ASR_PYTHON",
        "FORUM_ASR_SCRIPT",
        "FORUM_WHISPER_MODEL_PATH",
    ] {
        controller.set_env(
            key,
            std::env::var(key).with_context(|| format!("{key} required"))?,
        );
    }
    let mut capture = CaptureSession::open(CaptureContext {
        max_segment_ms: 10000,
        runtime: config.clone(),
        track,
        recording_dir: root.join("recording"),
        recording_enabled: true,
        replay_only: false,
        configured_source_language: "auto".into(),
        target_languages: vec!["zh".into(), "en".into()],
        direction_epoch: 1,
    })?;
    let early = vec![0.2; 480];
    capture.record(&early)?;
    ensure!(
        !capture.models_ready()? && capture.close_segment(&early).is_err(),
        "30ms first frame escaped readiness barrier"
    );
    let start = Instant::now();
    controller.start()?;
    let context = controller.dynamic_node_context()?;
    let owned = controller.owned_node_health()?;
    let node_config = moxin_dora_bridge::dynamic_node_endpoint::request_owned_node_config(
        &context,
        "moxin-file-capture".to_string().into(),
    )?;
    let (mut node, events) = DoraNode::init(node_config).map_err(|e| anyhow!("{e:#}"))?;
    while !capture.models_ready()? {
        ensure!(
            start.elapsed() < Duration::from_secs(100),
            "ASR ready timeout"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let ready_ms = start.elapsed().as_millis();
    let mut cases = vec![("30ms_stop_tail".to_owned(), early)];
    for name in ["zh_negation", "en_negation", "mixed_en_stitched"] {
        cases.push((
            name.into(),
            wave(&repo.join(format!(
                "artifacts/local/f01-model-feasibility/audio/{name}.wav"
            )))?,
        ));
    }
    let mut results = Vec::new();
    for (index, (name, pcm)) in cases.into_iter().enumerate() {
        let began = Instant::now();
        if index > 0 {
            capture.record(&pcm)?;
        }
        let metadata = capture.close_segment(&pcm)?;
        let mut params = BTreeMap::new();
        params.insert(
            "forum_segment".into(),
            Parameter::String(serde_json::to_string(&metadata)?),
        );
        node.send_output("audio_segment".into(), params, pcm.into_arrow())
            .map_err(|e| anyhow!("{e:#}"))?;
        capture.mark_dispatched(metadata.segment_id)?;
        loop {
            let segment_id = metadata.segment_id;
            if let Ok(result) = core.call(move |store| {
                store.transcript_revision(sid, segment_id, forum_contracts::Revision::FIRST)
            }) {
                ensure!(
                    result.payload.status != forum_contracts::TranscriptStatus::Failed,
                    "ASR failed: {:?}",
                    result.payload.reason
                );
                results.push(json!({"case":name,"elapsed_ms":began.elapsed().as_millis(),"transcript":result}));
                break;
            }
            ensure!(
                began.elapsed() < Duration::from_secs(70),
                "ASR final timeout"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
    transition(
        &core,
        &session,
        run,
        &mut seq,
        SessionState::Recording,
        SessionState::Stopping,
    )?;
    capture.seal_after_devices_released()?;
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if core
            .call(move |s| s.unsealed_producer_runs(sid))?
            .is_empty()
        {
            break;
        }
        ensure!(Instant::now() < deadline, "ASR producer seal not durable");
        thread::sleep(Duration::from_millis(100));
    }
    let snapshot = core.call(move |store| store.session_snapshot(sid))?;
    ensure!(
        snapshot
            .incomplete
            .iter()
            .all(|s| s.status == Some(forum_contracts::TranscriptStatus::Empty))
            && snapshot.transcript.len() == 3
            && core.call(move |s| s.terminal_segment_ids(sid))?.len() == 4,
        "missing or duplicate source finals"
    );
    drop(events);
    drop(node);
    controller.stop()?;
    let shutdown = controller.last_shutdown_report();
    ensure!(
        shutdown.as_ref().is_some_and(|s| s.contained),
        "owned processes not contained"
    );
    for child in &owned {
        ensure!(
            unsafe { libc::kill(child.pid as i32, 0) } < 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "ASR process remains"
        );
    }
    let asr_producer = DurableProducer::open(config.for_producer("asr")?)?;
    ensure!(
        asr_producer.pending()?.is_empty(),
        "ASR outbox retained unacked event after drain"
    );
    let report = json!({"scope":"file-only production Rust capture -> private Dora ASR -> SQLite; translator absent","ready_ms":ready_ms,"first_30ms_blocked_before_ready":true,"injected_first_final_ack_loss":!reject_once.load(Ordering::Acquire),"results":results,"shutdown":shutdown,"owned_asr_pids":owned,"all_owned_asr_gone":true,"recording":root.join("recording"),"database":root.join("meeting.sqlite"),"runtime_root":root});
    fs::write(
        artifacts.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    server.stop()?;
    core.shutdown()?;
    println!("{}", artifacts.join("report.json").display());
    Ok(())
}
