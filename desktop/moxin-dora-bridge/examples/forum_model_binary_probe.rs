//! F01 actual model binaries through the private production controller.
//! Uses an existing synthetic PCM WAV file only; never creates an audio device.
use anyhow::{anyhow, ensure, Context, Result};
use arrow::array::{Float32Array, StringArray};
use dora_node_api::{DoraNode, Event, Parameter};
use moxin_dora_bridge::{BridgeState, DataflowController, DynamicNodeDispatcher};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn pcm(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "expected WAV"
    );
    let mut offset = 12;
    let mut format_ok = false;
    let mut audio = None;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        ensure!(offset + 8 + size <= bytes.len(), "truncated WAV chunk");
        let chunk = &bytes[offset + 8..offset + 8 + size];
        match &bytes[offset..offset + 4] {
            b"fmt " if size >= 16 => {
                format_ok = u16::from_le_bytes(chunk[..2].try_into()?) == 1
                    && u16::from_le_bytes(chunk[2..4].try_into()?) == 1
                    && u32::from_le_bytes(chunk[4..8].try_into()?) == 16000
                    && u16::from_le_bytes(chunk[14..16].try_into()?) == 16;
            }
            b"data" => audio = Some(chunk),
            _ => {}
        }
        offset += 8 + size + size % 2;
    }
    ensure!(format_ok, "requires synthetic PCM16 mono 16 kHz");
    let audio = audio.context("missing PCM")?;
    ensure!(
        !audio.is_empty() && audio.len() % 2 == 0 && audio.len() <= 16000 * 2 * 20,
        "PCM bounds"
    );
    Ok(audio
        .chunks_exact(2)
        .map(|x| i16::from_le_bytes([x[0], x[1]]) as f32 / 32768.0)
        .collect())
}

fn child(role: &str) -> Result<()> {
    let (mut node, mut events) = DoraNode::init_from_env().map_err(|e| anyhow!("{e:#}"))?;
    println!(
        "{}",
        json!({"event":"attached","role":role,"pid":std::process::id(),"ppid":unsafe{libc::getppid()},"pgid":unsafe{libc::getpgrp()},"flow":node.dataflow_id(),"node":node.id(),"explicit_config":std::env::var_os("DORA_NODE_CONFIG").is_some()})
    );
    std::io::stdout().flush()?;
    let audio = if role == "producer" {
        Some(pcm(Path::new(&std::env::var("PROBE_AUDIO")?))?)
    } else {
        None
    };
    let release = std::env::var_os("PROBE_RELEASE").map(PathBuf::from);
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut sent = 0_i64;
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(200)) {
            Some(Event::Input { .. }) => {
                if role == "producer" {
                    if sent > 0 || !release.as_ref().is_some_and(|p| p.exists()) {
                        continue;
                    }
                    let meta: BTreeMap<String, Parameter> = [
                        ("sample_rate".into(), Parameter::Integer(16000)),
                        ("question_id".into(), Parameter::Integer(1)),
                        ("burst_id".into(), Parameter::Integer(1)),
                        ("direction_epoch".into(), Parameter::Integer(0)),
                        ("language".into(), Parameter::String("Chinese".into())),
                        ("source_language".into(), Parameter::String("zh".into())),
                        ("target_language".into(), Parameter::String("en".into())),
                        (
                            "transcription_mode".into(),
                            Parameter::String("final".into()),
                        ),
                        (
                            "segment_reason".into(),
                            Parameter::String("speech_end".into()),
                        ),
                    ]
                    .into_iter()
                    .collect();
                    node.send_output(
                        "audio".into(),
                        meta,
                        Float32Array::from(audio.as_ref().unwrap().clone()),
                    )
                    .map_err(|e| anyhow!("{e:#}"))?;
                    sent = 1;
                    println!(
                        "{}",
                        json!({"event":"synthetic_pcm_sent","samples":audio.as_ref().unwrap().len()})
                    );
                    std::io::stdout().flush()?;
                } else {
                    sent += 1;
                    let meta = [
                        (
                            "session_status".into(),
                            Parameter::String("complete".into()),
                        ),
                        ("commit_id".into(), Parameter::Integer(sent)),
                        (
                            "source_text".into(),
                            Parameter::String(format!("peer-{sent}")),
                        ),
                    ]
                    .into_iter()
                    .collect();
                    node.send_output(
                        "translation".into(),
                        meta,
                        StringArray::from(vec![format!("peer-translation-{sent}")]),
                    )
                    .map_err(|e| anyhow!("{e:#}"))?;
                }
            }
            Some(Event::Stop(_)) => break,
            Some(Event::Error(e)) => return Err(anyhow!("{e}")),
            _ => {}
        }
    }
    println!(
        "{}",
        json!({"event":"child_loop_ended","role":role,"sent":sent})
    );
    std::io::stdout().flush()?;
    drop(events);
    drop(node);
    Ok(())
}

fn gone(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as i32, 0) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

fn run(
    dir: &Path,
    binaries: &Path,
    audio: &Path,
    asr_model: &Path,
    translator_model: &Path,
) -> Result<Value> {
    ensure!(
        asr_model.join("tokenizer.json").is_file(),
        "read-only ASR requires cached tokenizer.json"
    );
    let exe = std::env::current_exe()?.canonicalize()?;
    let release = dir.join("release-audio");
    let graph = json!({"nodes":[
        {"id":"file-producer","path":exe,"inputs":{"tick":"dora/timer/millis/100"},"outputs":["audio"],"env":{"FORUM_PROBE_ROLE":"producer","PROBE_AUDIO":audio,"PROBE_RELEASE":release}},
        {"id":"asr","path":binaries.join("dora-qwen3-asr"),"inputs":{"audio":"file-producer/audio"},"outputs":["transcription","log"],"env":{"QWEN3_ASR_MODEL_PATH":asr_model,"LANGUAGE":"Chinese","TARGET_LANGUAGE":"en","LOG_LEVEL":"INFO"}},
        {"id":"translator","path":binaries.join("dora-qwen35-translator"),"inputs":{"text":"asr/transcription"},"outputs":["source_text","translation","log"],"env":{"QWEN35_TRANSLATOR_MODEL_PATH":translator_model,"SRC_LANG":"zh","TGT_LANG":"en","PASSTHROUGH":"0","TEMPERATURE":"0.0","MAX_TOKENS":"256","TRANSLATOR_WARMUP":"1","LOG_LEVEL":"INFO"}},
        {"id":"moxin-translation-listener","path":"dynamic","inputs":{"source_text":"translator/source_text","translation":"translator/translation","log":"translator/log"}}
    ]});
    let peer_graph = json!({"nodes":[
        {"id":"peer-source","path":exe,"inputs":{"tick":"dora/timer/millis/100"},"outputs":["translation"],"env":{"FORUM_PROBE_ROLE":"peer"}},
        {"id":"moxin-translation-listener","path":"dynamic","inputs":{"translation":"peer-source/translation"}}
    ]});
    fs::write(dir.join("models.yml"), serde_yaml::to_string(&graph)?)?;
    fs::write(dir.join("peer.yml"), serde_yaml::to_string(&peer_graph)?)?;
    let mut peer = DynamicNodeDispatcher::new(DataflowController::new(dir.join("peer.yml"))?);
    let mut model = DynamicNodeDispatcher::new(DataflowController::new(dir.join("models.yml"))?);
    peer.start().context("start peer")?;
    let start = Instant::now();
    model.start().context("start actual model graph")?;
    let model_started_ms = start.elapsed().as_millis();
    let model_runtime = model.controller().read().runtime_directory().to_path_buf();
    let peer_runtime = peer.controller().read().runtime_directory().to_path_buf();
    fs::write(
        dir.join("runtime-paths.json"),
        serde_json::to_vec_pretty(&json!({"models":model_runtime,"peer":peer_runtime}))?,
    )?;
    let model_context = model.controller().read().dynamic_node_context()?;
    let peer_context = peer.controller().read().dynamic_node_context()?;
    ensure!(
        model_context.dataflow_id != peer_context.dataflow_id
            && model_context.daemon_addr != peer_context.daemon_addr,
        "shared identities"
    );
    let model_pids = model.controller().read().owned_process_pids();
    let peer_pids = peer.controller().read().owned_process_pids();
    let node_pids = model.controller().read().owned_node_pids();
    ensure!(node_pids.len() == 3, "missing producer/model Child handles");
    let parents = std::process::Command::new("/bin/ps")
        .args([
            "-o",
            "pid=,ppid=,pgid=,command=",
            "-p",
            &node_pids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ])
        .output()?;
    let parents = String::from_utf8(parents.stdout)?;
    fs::write(dir.join("model-child-processes.txt"), &parents)?;
    // start() confirms registration, not model readiness. Do not submit the
    // sole synthetic segment while either actual binary is still loading.
    let ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let asr_log =
            fs::read_to_string(model_runtime.join("node-1.stdout.log")).unwrap_or_default();
        let translator_log =
            fs::read_to_string(model_runtime.join("node-2.stdout.log")).unwrap_or_default();
        if asr_log.contains("ASR model loaded successfully")
            && translator_log.contains("Buffer merge mode active")
        {
            break;
        }
        model.controller().read().get_status()?;
        ensure!(
            Instant::now() < ready_deadline,
            "models did not become ready"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let model_ready_ms = start.elapsed().as_millis();
    fs::write(&release, b"send existing synthetic file once")?;
    let sent_at = Instant::now();
    let observed = loop {
        model
            .controller()
            .read()
            .get_status()
            .context("model process health")?;
        if let Some(update) = model.shared_state().translation.read() {
            if let Some(entry) = update
                .history
                .iter()
                .find(|e| !e.source_text.is_empty() && !e.translation.is_empty())
            {
                break json!({"source":entry.source_text,"translation":entry.translation,"completed_count":update.completed_count});
            }
        }
        ensure!(
            sent_at.elapsed() < Duration::from_secs(60),
            "actual model transcript did not reach production listener"
        );
        thread::sleep(Duration::from_millis(50));
    };
    let observed_ms = sent_at.elapsed().as_millis();
    let shutdown_started = Instant::now();
    model.stop().context("stop model graph")?;
    let shutdown_ms = shutdown_started.elapsed().as_millis();
    ensure!(
        model_pids.iter().all(|p| gone(*p)),
        "model owned child remains"
    );
    ensure!(
        model
            .get_bridge("moxin-translation-listener")
            .unwrap()
            .state()
            == BridgeState::Disconnected,
        "listener not joined"
    );
    let before_peer = peer
        .shared_state()
        .translation
        .read()
        .map(|s| s.completed_count)
        .unwrap_or(0);
    let peer_deadline = Instant::now() + Duration::from_secs(3);
    let after_peer = loop {
        peer.controller().read().get_status()?;
        let count = peer
            .shared_state()
            .translation
            .read()
            .map(|s| s.completed_count)
            .unwrap_or(0);
        if count >= before_peer + 3 {
            break count;
        }
        ensure!(
            Instant::now() < peer_deadline,
            "independent peer stopped progressing"
        );
        thread::sleep(Duration::from_millis(40));
    };
    peer.stop()?;
    ensure!(peer_pids.iter().all(|p| gone(*p)), "peer child remains");
    Ok(
        json!({"passed":true,"scope":"synthetic file only; actual model binaries via DORA_NODE_CONFIG and production listener; not latency benchmark; concurrent local build possible","audio":audio,"controller_pid":std::process::id(),"models":{"flow":model_context.dataflow_id,"lookup":model_context.daemon_addr,"runtime":model_runtime,"owned_pids":model_pids,"node_pids":node_pids,"process_listing":parents,"startup_ms":model_started_ms,"model_ready_ms":model_ready_ms,"observed_after_release_ms":observed_ms,"observed":observed,"shutdown_ms":shutdown_ms,"shutdown":model.controller().read().last_shutdown_report()},"peer":{"flow":peer_context.dataflow_id,"lookup":peer_context.daemon_addr,"runtime":peer_runtime,"owned_pids":peer_pids,"count_after_model_stop":before_peer,"count_later":after_peer,"shutdown":peer.controller().read().last_shutdown_report()},"all_owned_pids_absent":true,"production_listener_joined":true,"microphone_or_system_capture":false}),
    )
}

fn main() -> Result<()> {
    if let Ok(role) = std::env::var("FORUM_PROBE_ROLE") {
        return child(&role);
    }
    let args: Vec<_> = std::env::args().skip(1).map(PathBuf::from).collect();
    ensure!(args.len()==5,"usage: forum_model_binary_probe EXISTING_ARTIFACT_ROOT BIN_DIR PCM_WAV ASR_MODEL TRANSLATOR_MODEL");
    let paths: Vec<_> = args
        .iter()
        .map(|p| p.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let dir = paths[0].join(uuid::Uuid::new_v4().to_string());
    fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let result = run(&dir, &paths[1], &paths[2], &paths[3], &paths[4]);
    let report = match &result {
        Ok(v) => v.clone(),
        Err(e) => {
            json!({"passed":false,"error":format!("{e:#}"),"cleanup":"owned dispatcher Drop attempted; inspect runtime logs"})
        }
    };
    fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", json!({"artifact_directory":dir,"report":report}));
    result.map(|_| ())
}
