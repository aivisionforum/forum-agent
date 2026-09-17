//! Durable ASR source branch. Translation availability never gates a final commit.
use anyhow::{anyhow, ensure, Context, Result};
use arrow::array::{Array, Float32Array};
use dora_node_api::{DoraNode, Event, EventStream, IntoArrow, Parameter, TryRecvError};
use forum_contracts::{EventType, ProducerSeal, TranscriptFinal, TranscriptStatus};
use forum_runtime::{
    audio::SegmentMeta, process_io::JsonLineProcess, DurableProducer, RuntimeConfig,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

fn verified_model_fingerprint(path: &std::path::Path) -> Result<String> {
    let actual = forum_runtime::model_fingerprint(path)?;
    if let Ok(expected) = std::env::var("FORUM_ASR_MODEL_MANIFEST_ID") {
        ensure!(
            expected == actual,
            "actual local ASR model fingerprint differs from expected ID"
        );
    }
    Ok(actual)
}

struct Recognition {
    text: String,
    detected_language: Option<String>,
}
enum Recognizer {
    Whisper(JsonLineProcess),
    Fixed(crate::backend::AsrEngine),
}
impl Recognizer {
    fn load() -> Result<(Self, String, String)> {
        if std::env::var("ASR_SOURCE_LANGUAGE").as_deref() == Ok("auto") {
            let absolute = |name: &str| -> Result<PathBuf> {
                let path = PathBuf::from(
                    std::env::var(name).with_context(|| format!("{name} required for auto ASR"))?,
                );
                ensure!(path.is_absolute(), "{name} must be absolute");
                Ok(path)
            };
            let python = absolute("FORUM_ASR_PYTHON")?;
            let script = absolute("FORUM_ASR_SCRIPT")?;
            let model = absolute("FORUM_WHISPER_MODEL_PATH")?;
            ensure!(
                python.is_file()
                    && script.is_file()
                    && model.join("config.json").is_file()
                    && model.join("weights.safetensors").is_file(),
                "Whisper requires existing local runtime/model files"
            );
            let fingerprint = verified_model_fingerprint(&model)?;
            let mut command = Command::new(python);
            command
                .args(["-I", "-B"])
                .arg(script)
                .arg("--model")
                .arg(&model)
                .env_remove("PYTHONPATH")
                .env_remove("PYTHONHOME")
                .env_remove("VIRTUAL_ENV")
                .env_remove("CONDA_PREFIX")
                .env_remove("DYLD_LIBRARY_PATH")
                .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
                .env("HF_HUB_OFFLINE", "1");
            let process = JsonLineProcess::spawn(command, 16 * 1024 * 1024)?;
            let ready = process.receive(Duration::from_secs(90))?;
            ensure!(
                ready["type"] == "ready"
                    && ready["protocol_version"] == 1
                    && ready["backend"] == "mlx-whisper",
                "Whisper adapter did not become ready: {}",
                ready.get("error").unwrap_or(&json!("invalid readiness"))
            );
            Ok((
                Self::Whisper(process),
                "mlx-whisper-python".into(),
                fingerprint,
            ))
        } else {
            let model = crate::resolve_model_path();
            let fingerprint = verified_model_fingerprint(&model)?;
            Ok((
                Self::Fixed(crate::backend::AsrEngine::load(&model)?),
                "qwen3-asr-mlx-fixed".into(),
                fingerprint,
            ))
        }
    }
    fn transcribe(&mut self, samples: &[f32], language: &str) -> Result<Recognition> {
        match self {
            Self::Whisper(process) => {
                ensure!(
                    language == "auto",
                    "Whisper automatic adapter requires configured language auto"
                );
                let id = forum_contracts::Uuid::new_v4();
                let result = process.request(
                    &json!({"id":id,"samples":samples,"sample_rate":16000,"language":"auto"}),
                    Duration::from_secs(60),
                )?;
                ensure!(result["id"] == json!(id), "Whisper response ID mismatch");
                ensure!(
                    result["status"] != "failed",
                    "Whisper failed: {}",
                    result["error"]
                );
                ensure!(
                    matches!(result["status"].as_str(), Some("success" | "empty")),
                    "invalid Whisper result status"
                );
                Ok(Recognition {
                    text: result["text"]
                        .as_str()
                        .context("Whisper result text missing")?
                        .into(),
                    detected_language: result["detected_language"].as_str().map(str::to_owned),
                })
            }
            Self::Fixed(engine) => {
                ensure!(
                    language != "auto",
                    "Qwen fixed language binding cannot implement automatic language detection"
                );
                let text = engine.transcribe(samples, &crate::normalize_language(language))?;
                crate::backend::clear_cache();
                Ok(Recognition {
                    text,
                    detected_language: None,
                })
            }
        }
    }
}

fn ready_to_seal(
    status: &serde_json::Value,
    accepted: &HashSet<(forum_contracts::Uuid, u32)>,
) -> bool {
    let all_terminal = status["registered_segment_ids"]
        .as_array()
        .zip(status["asr_terminal_segment_ids"].as_array())
        .is_some_and(|(registered, terminal)| registered.iter().all(|id| terminal.contains(id)));
    let replay_done = status["capture_dispatch_complete"] == true
        && status["replay_expected_revisions"]
            .as_array()
            .is_some_and(|expected| {
                expected.iter().all(|value| {
                    let id = value["segment_id"].as_str().and_then(|s| s.parse().ok());
                    let revision = value["revision"]
                        .as_u64()
                        .and_then(|r| u32::try_from(r).ok());
                    id.zip(revision)
                        .is_some_and(|item| accepted.contains(&item))
                })
            });
    status["capture_stopped"] == true && all_terminal && replay_done
}

pub fn run(mut node: DoraNode, mut events: EventStream, config: RuntimeConfig) -> Result<()> {
    let config = config.for_producer("asr")?;
    let mut producer = DurableProducer::open(config.clone())?;
    for (_, result) in producer.flush_pending() {
        result?;
    }
    let (mut engine, backend, manifest) = Recognizer::load()?;
    producer.client().call("ready",json!({"session_id":config.session.session_id,"producer_run_id":producer.run_id(),"role":"asr","backend":backend,"model_manifest_id":manifest,"prior_run_ids":producer.prior_run_ids()?,"prior_run_records":producer.prior_run_records()?,"outbox_replayed":true}))?;
    let mut accepted: HashSet<(forum_contracts::Uuid, u32)> = HashSet::new();
    let mut last_retry = Instant::now();
    let mut sealed = false;
    loop {
        if last_retry.elapsed() >= Duration::from_millis(500) {
            for (_, result) in producer.flush_pending() {
                if let Err(error) = result {
                    tracing::warn!(code=%error.code,"ASR final remains in durable outbox");
                }
            }
            if !sealed && producer.pending()?.is_empty() {
                if let Ok(status) = producer
                    .client()
                    .call("status", json!({"session_id":config.session.session_id}))
                {
                    if ready_to_seal(&status, &accepted) {
                        let mut ids: Vec<_> = accepted.iter().map(|(id, _)| *id).collect();
                        ids.sort();
                        ids.dedup();
                        let seal = ProducerSeal {
                            producer_run_id: producer.run_id(),
                            final_seq: producer.last_seq(),
                            segment_ids: ids,
                        };
                        let pending = producer.append(EventType::ProducerSealed, &seal)?;
                        producer.flush_one(pending.message_id)?;
                        sealed = true;
                    }
                }
            }
            last_retry = Instant::now();
        }
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(TryRecvError::Closed) => break,
        };
        match event {
            Event::Input { id, data, metadata } if id.as_str() == "audio" => {
                ensure!(!sealed, "ASR input arrived after its producer was sealed");
                let encoded = metadata
                    .parameters
                    .get("forum_segment")
                    .and_then(|v| {
                        if let Parameter::String(text) = v {
                            Some(text)
                        } else {
                            None
                        }
                    })
                    .context("reliable ASR input has no stable segment metadata")?;
                let segment: SegmentMeta = serde_json::from_str(encoded)?;
                let samples = data
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .context("reliable ASR requires Float32Array")?;
                let pcm: Vec<f32> = samples.values().to_vec();
                segment.validate(pcm.len())?;
                ensure!(
                    segment.session_id == config.session.session_id,
                    "ASR input session mismatch"
                );
                if segment.final_segment
                    && accepted.contains(&(segment.segment_id, segment.revision.get()))
                {
                    continue;
                }
                let result = if pcm.len() < 1600 {
                    Ok(Recognition {
                        text: String::new(),
                        detected_language: None,
                    })
                } else {
                    engine.transcribe(&pcm, &segment.configured_source_language)
                };
                if !segment.final_segment {
                    if let Ok(result) = result {
                        let mut params = BTreeMap::new();
                        params.insert("forum_segment".into(), Parameter::String(encoded.clone()));
                        params.insert(
                            "transcription_mode".into(),
                            Parameter::String("progressive".into()),
                        );
                        let _ = node.send_output(
                            "transcription".into(),
                            params,
                            vec![result.text].into_arrow(),
                        );
                    }
                    continue;
                }
                let (text, detected_language, status, reason) = match result {
                    Ok(value) if value.text.trim().is_empty() => (
                        String::new(),
                        value.detected_language,
                        TranscriptStatus::Empty,
                        Some(
                            if pcm.len() < 1600 {
                                "segment_shorter_than_100ms"
                            } else {
                                "no_speech"
                            }
                            .into(),
                        ),
                    ),
                    Ok(value) => (
                        value.text,
                        value.detected_language,
                        TranscriptStatus::Success,
                        None,
                    ),
                    Err(error) => (
                        String::new(),
                        None,
                        TranscriptStatus::Failed,
                        Some(format!("recognition_failed: {error:#}")),
                    ),
                };
                let final_text = TranscriptFinal {
                    track_id: segment.track_id,
                    segment_id: segment.segment_id,
                    revision: segment.revision,
                    audio: segment.audio.clone(),
                    text,
                    configured_source_language: segment.configured_source_language.clone(),
                    detected_language,
                    target_languages: segment.target_languages.clone(),
                    direction_epoch: segment.direction_epoch,
                    speaker_id: None,
                    status,
                    reason,
                    backend: backend.clone(),
                    model_manifest_id: manifest.clone(),
                };
                final_text.validate()?;
                let pending = producer.append(EventType::TranscriptFinal, &final_text)?;
                // Once append succeeds, this process never regenerates the same
                // segment/revision merely because an ack was lost.
                accepted.insert((segment.segment_id, segment.revision.get()));
                if let Err(error) = producer.flush_one(pending.message_id) {
                    tracing::warn!(code=%error.code,"ASR final saved locally; core ack pending");
                }
            }
            Event::Stop(_) => break,
            Event::Error(error) => return Err(anyhow!("ASR Dora event error: {error}")),
            _ => {}
        }
    }
    for (_, result) in producer.flush_pending() {
        result?;
    }
    if !sealed {
        let mut ids: Vec<_> = accepted.into_iter().map(|(id, _)| id).collect();
        ids.sort();
        ids.dedup();
        let seal = ProducerSeal {
            producer_run_id: producer.run_id(),
            final_seq: producer.last_seq(),
            segment_ids: ids,
        };
        let pending = producer.append(EventType::ProducerSealed, &seal)?;
        producer.flush_one(pending.message_id)?;
    }
    drop(events);
    drop(node);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_seal_waits_for_dispatched_revision_even_when_old_failed_is_terminal() {
        let id = forum_contracts::Uuid::new_v4();
        let mut status = json!({"capture_stopped":true,"registered_segment_ids":[id],"asr_terminal_segment_ids":[id],"capture_dispatch_complete":false,"replay_expected_revisions":[{"segment_id":id,"revision":2}]});
        let mut accepted = HashSet::new();
        accepted.insert((id, 1));
        assert!(!ready_to_seal(&status, &accepted));
        status["capture_dispatch_complete"] = json!(true);
        assert!(!ready_to_seal(&status, &accepted));
        accepted.insert((id, 2));
        assert!(ready_to_seal(&status, &accepted));
    }
    #[test]
    fn empty_queue_never_substitutes_for_registered_final_set() {
        let id = forum_contracts::Uuid::new_v4();
        let status = json!({"capture_stopped":true,"registered_segment_ids":[id],"asr_terminal_segment_ids":[],"capture_dispatch_complete":true,"replay_expected_revisions":[]});
        assert!(!ready_to_seal(&status, &HashSet::new()));
    }
}
