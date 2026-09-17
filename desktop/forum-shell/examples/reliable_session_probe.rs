//! Full desktop runtime with synthetic WAV recovery. Never opens an audio device.
#[path = "../src/dataflow.rs"]
mod dataflow;
#[path = "../src/identity.rs"]
mod identity;
#[path = "../src/meeting.rs"]
mod meeting;
#[path = "../src/models.rs"]
mod models;
#[path = "../src/runtime.rs"]
mod runtime;
use anyhow::{anyhow, ensure, Context, Result};
use forum_contracts::Uuid;
use forum_runtime::RuntimeClient;
use meeting::{MeetingHost, MeetingOptions};
use moxin_dora_bridge::CaptureSession;
use runtime::{RuntimeEvent, TranslationRuntime};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
fn pcm(path: &Path) -> Result<Vec<f32>> {
    let mut wav = hound::WavReader::open(path)?;
    ensure!(
        wav.spec().channels == 1
            && wav.spec().sample_rate == 16000
            && wav.spec().bits_per_sample == 16,
        "requires synthetic mono16k PCM16"
    );
    Ok(wav
        .samples::<i16>()
        .map(|s| s.map(|s| s as f32 / 32768.))
        .collect::<std::result::Result<_, _>>()?)
}
fn main() -> Result<()> {
    let repo = std::env::current_dir()?.canonicalize()?;
    let root = PathBuf::from("/tmp").join(format!("forum-runtime-probe-{}", Uuid::new_v4()));
    let runtime = TranslationRuntime::new(root.clone());
    let options = MeetingOptions {
            max_segment_ms: 10_000,        source_language: "auto".into(),
        target_language: "bilingual".into(),
        recording_enabled: true,
        system_audio: false,
                dual_audio: false,
    };
    let paths = models::ModelPaths::resolve_current().map_err(|e| anyhow!(e))?;
    let automatic = models::AutomaticAsrPaths::resolve(None).map_err(|e| anyhow!(e))?;
    let mut env = paths.env_vars().map_err(|e| anyhow!(e))?;
    env.extend(automatic.env_vars().map_err(|e| anyhow!(e))?);
    let flow = dataflow::render_translation_dataflow(
        None,
        dataflow::RenderOptions {
            source_language: "auto",
            target_language: "bilingual",
            system_audio: false,
            max_segment_ms: 5000,
            asr_model_path: &paths.asr,
            translator_model_path: &paths.translator,
        },
    )
    .map_err(|e| anyhow!(e))?;
    let cases = [
        "zh_negation",
        "en_negation",
        "mixed_zh_single_voice",
        "mixed_en_stitched",
        "alternating_turns",
        "short_en",
        "silence",
    ];
    let mut reports = Vec::new();
    for round in 0..2 {
        // Seed an interrupted capture journal; intentionally never dispatch ASR.
        let mut host = MeetingHost::create(
            runtime.repository().map_err(|e| anyhow!(e))?.clone(),
            options.clone(),
            None,
        )
        .map_err(|e| anyhow!(e))?;
        let sid = host.id();
        let client = RuntimeClient::new(host.config.endpoint.clone());
        client.call(
            "ready",
            json!({"session_id":sid,"role":"asr","prior_run_records":[],"outbox_replayed":true}),
        )?;
        host.activate_if_ready().map_err(|e| anyhow!(e))?;
        let mut capture = CaptureSession::open(host.capture_context())?;
        let mut source_ids = Vec::new();
        let selected = if round == 0 {
            cases.to_vec()
        } else {
            vec!["short_en"]
        };
        for name in &selected {
            let audio = pcm(&repo.join(format!(
                "artifacts/local/f01-model-feasibility/audio/{name}.wav"
            )))?;
            capture.record(&audio)?;
            let meta = capture.close_segment(&audio)?;
            source_ids.push(json!({"case":name,"segment_id":meta.segment_id}));
        }
        host.interrupt("synthetic file probe: interrupted before any ASR dispatch")
            .map_err(|e| anyhow!(e))?;
        drop(capture);
        drop(host);
        let started = Instant::now();
        runtime
            .start(flow.clone(), env.clone(), options.clone(), Some(sid))
            .map_err(|e| anyhow!(e))?;
        let mut events = Vec::new();
        let receipt = 'receipt: loop {
            for event in runtime.poll_events() {
                eprintln!("round {round}: {event:?}");
                events.push(format!("{event:?}"));
                match event {
                    RuntimeEvent::Started(_) => {
                        return Err(anyhow!(
                            "file recovery must never report hardware capture started"
                        ))
                    }
                    RuntimeEvent::Error(error) | RuntimeEvent::ShutdownFailed(error) => {
                        return Err(anyhow!("runtime failed: {error}"))
                    }
                    RuntimeEvent::Stopped {
                        session_id,
                        incomplete,
                        translation_pending,
                    } => {
                        break 'receipt json!({"session_id":session_id,"incomplete":incomplete,"translation_pending":translation_pending})
                    }
                    _ => {}
                }
            }
            ensure!(
                started.elapsed() < Duration::from_secs(160),
                "runtime recovery timeout"
            );
            thread::sleep(Duration::from_millis(100));
        };
        let export = runtime
            .repository()
            .map_err(|e| anyhow!(e))?
            .core
            .call(move |store| store.export_session(sid))?;
        let page = runtime
            .repository()
            .map_err(|e| anyhow!(e))?
            .page(sid, None, None)
            .map_err(|e| anyhow!(e))?;
        ensure!(
            page["items"]
                .as_array()
                .context("source page missing")?
                .len()
                == selected.len(),
            "recovery lost or duplicated source segments"
        );
        ensure!(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["transcript"].is_object()),
            "source final remains missing"
        );
        fs::write(
            root.join(format!("round-{round}.json")),
            serde_json::to_vec_pretty(
                &json!({"round":round,"cases":source_ids,"receipt":receipt,"events":events,"snapshot":page,"translations":export.translations}),
            )?,
        )?;
        ensure!(
            export.status.transcript_sealed,
            "source transcript was not sealed; diagnostics saved under {}",
            root.display()
        );
        let markdown = runtime
            .repository()
            .map_err(|e| anyhow!(e))?
            .export_markdown(sid)
            .map_err(|e| anyhow!(e))?;
        fs::write(root.join(format!("round-{round}.md")), markdown)?;
        reports.push(json!({"round":round,"cases":source_ids,"elapsed_ms":started.elapsed().as_millis(),"receipt":receipt,"events":events,"snapshot":page,"translations":export.translations}));
    }
    let output = repo
        .join("artifacts/local/f04-desktop-runtime")
        .join(Uuid::new_v4().to_string());
    fs::create_dir_all(&output)?;
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(
            &json!({"scope":"production desktop runtime + synthetic recording recovery; no audio devices","root":root,"rounds":reports}),
        )?,
    )?;
    drop(runtime);
    println!("{}", output.join("report.json").display());
    Ok(())
}
