//! Production Forum mode: request and results cross the durable local core IPC.
//! Dora keeps process ownership; transient text inputs never create work here.
use crate::{
    backend,
    durable_queue::{AcknowledgedResult, AttemptKey, DurableQueue, PASSTHROUGH_BACKEND},
    generation_control::GenerationControl,
    DirectionMeta, TranslationTask, TranslationWorkerEvent,
};
use anyhow::{anyhow, Result};
use dora_node_api::{DoraNode, Event, EventStream, Parameter, TryRecvError};
use forum_runtime::{RpcError, RuntimeConfig};
use std::{
    collections::BTreeMap,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn stop_requested(events: &mut EventStream) -> bool {
    for _ in 0..100 {
        match events.try_recv() {
            Ok(Event::Stop(_)) => return true,
            Ok(Event::Error(error)) => {
                tracing::warn!(%error,"Dora reported an event error");
                return true;
            }
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Closed) => return true,
            _ => {} // ASR text is intentionally ignored: core coverage is authoritative.
        }
    }
    false
}

fn retryable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RpcError>()
        .is_some_and(|e| e.retryable)
}

fn notify_committed(node: &mut DoraNode, accepted: AcknowledgedResult) {
    // Legacy private caption surface only. The core
    // outbox/snapshot remains the durable UI source.
    let mut meta: BTreeMap<String, Parameter> = BTreeMap::new();
    meta.insert(
        "session_status".into(),
        Parameter::String("complete".into()),
    );
    meta.insert(
        "commit_id".into(),
        Parameter::Integer(accepted.receipt.store_seq.min(i64::MAX as u64) as i64),
    );
    meta.insert(
        "source_text".into(),
        Parameter::String(accepted.request.input_text.clone()),
    );
    meta.insert(
        "translation_id".into(),
        Parameter::String(accepted.request.translation_id.to_string()),
    );
    meta.insert(
        "translation_revision".into(),
        Parameter::Integer(accepted.request.revision.get() as i64),
    );
    meta.insert(
        "attempt".into(),
        Parameter::Integer(accepted.request.attempt as i64),
    );
    meta.insert(
        "direction_epoch".into(),
        Parameter::Integer(accepted.request.direction_epoch.min(i64::MAX as u64) as i64),
    );
    meta.insert(
        "target_language".into(),
        Parameter::String(accepted.request.target_language.clone()),
    );
    // Failure here never rolls back or loses a core commit.
    if let Err(error) = crate::send_str(node, "translation", &accepted.result.text, meta) {
        tracing::warn!(%error,"Core committed translation; transient caption notification failed");
    }
}

fn model_manifest(path: &std::path::Path, expected: Option<&str>) -> Result<String> {
    let actual = forum_runtime::model_fingerprint(path)?;
    if let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) {
        anyhow::ensure!(
            expected == actual,
            "Translation model fingerprint differs from FORUM_TRANSLATOR_MODEL_MANIFEST_ID; the configured expectation cannot override actual model content"
        );
    }
    Ok(actual)
}

pub fn run(mut node: DoraNode, mut events: EventStream, mut config: RuntimeConfig) -> Result<()> {
    config = config.for_producer("translator")?;
    let model_path = crate::resolve_model_path();
    let expected_manifest = std::env::var("FORUM_TRANSLATOR_MODEL_MANIFEST_ID").ok();
    let model_manifest = model_manifest(&model_path, expected_manifest.as_deref())?;
    let mut queue = DurableQueue::open(config, model_manifest)?;
    let (request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        backend::translation_worker_loop(
            model_path,
            0.0,
            256,
            true,
            ready_tx,
            request_rx,
            response_tx,
        )
    });
    let mut ready = false;
    let mut current: Option<(AttemptKey, i64, GenerationControl)> = None;
    let mut sequence = 0_i64;
    let mut next_poll = Instant::now();
    let mut last_diagnostic = Instant::now() - Duration::from_secs(10);
    let result = (|| -> Result<()> {
        // Model loading is not equivalent to Dora registration. Poll Stop while
        // loading and never acknowledge ready before the backend receipt.
        loop {
            if stop_requested(&mut events) {
                queue.stop()?;
                return Ok(());
            }
            match ready_rx.try_recv() {
                Ok(Ok(())) => break,
                Ok(Err(e)) => return Err(anyhow!("Translation model initialization failed: {e}")),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(anyhow!("Translation model worker exited before ready"))
                }
                Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(20)),
            }
        }
        loop {
            if stop_requested(&mut events) {
                if let Some((_, _, control)) = &current {
                    control.cancel();
                }
                queue.stop()?;
                return Ok(());
            }
            if !ready {
                match queue.mark_ready() {
                    Ok(()) => {
                        ready = true;
                        tracing::info!(
                            "Durable translation model ready; reading only committed coverage"
                        );
                    }
                    Err(error) if retryable(&error) => {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            for _ in 0..100 {
                match response_rx.try_recv() {
                    Ok(TranslationWorkerEvent::Complete(response)) => {
                        if let Some((key, commit_id, _)) = &current {
                            if response.commit_id != *commit_id {
                                continue;
                            }
                            let accepted = queue.complete(*key, response.output)?;
                            current = None;
                            if let Some(accepted) = accepted {
                                notify_committed(&mut node, accepted);
                            }
                        }
                    }
                    Ok(TranslationWorkerEvent::Streaming { .. }) => {} // no uncommitted final substitute
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(anyhow!("Translation model worker disconnected"))
                    }
                }
            }
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + Duration::from_millis(200);
                match queue.poll() {
                    Ok(Some(request)) => {
                        let key = AttemptKey::from(&request);
                        if request.backend == PASSTHROUGH_BACKEND {
                            // The source is durably requested and explicitly
                            // tagged passthrough; no model request is submitted.
                            if let Some(accepted) =
                                queue.complete(key, Ok(request.input_text.clone()))?
                            {
                                notify_committed(&mut node, accepted);
                            }
                        } else if !queue.model_matches(&request) {
                            queue.complete(
                                key,
                                Err(format!("MODEL_UNAVAILABLE: requested {}/{} does not match this worker; do not relabel a changed model as the old request", request.backend,request.model_manifest_id)),
                            )?;
                        } else {
                            sequence = sequence
                                .checked_add(1)
                                .ok_or_else(|| anyhow!("translation sequence exhausted"))?;
                            let control = GenerationControl::new(Duration::from_secs(45), true);
                            request_tx.send(TranslationTask {
                                commit_id: sequence,
                                source_text: request.input_text.clone(),
                                system_prompt: crate::translation_prompt::system(&request.target_language),
                                user_prompt: crate::translation_prompt::user(&request.input_text, &request.context_spans.iter().map(|s| s.quote.as_str()).collect::<Vec<_>>().join(" ")),
                                direction: DirectionMeta {
                                    source_language: "durable-source".into(),
                                    target_language: request.target_language.clone(),
                                    epoch: request.direction_epoch as i64,
                                },
                                control: control.clone(),
                            })?;
                            current = Some((key, sequence, control));
                        }
                    }
                    Ok(None) => {}
                    Err(error) if retryable(&error) => {
                        if last_diagnostic.elapsed() >= Duration::from_secs(5) {
                            tracing::warn!(%error,"Core unavailable; translation coverage/outbox remain durable");
                            last_diagnostic = Instant::now();
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            thread::sleep(Duration::from_millis(15));
        }
    })();
    if let Some((_, _, control)) = &current {
        control.cancel();
    }
    let cleanup = queue.stop();
    drop(request_tx);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if worker.is_finished() {
        let _ = worker.join();
    } else {
        tracing::warn!("Model worker has not returned; owned node process shutdown is the final containment boundary");
    }
    tracing::info!(
        pending_events = queue.pending_count().unwrap_or(usize::MAX),
        "Durable translator stopping"
    );
    result.and(cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_expectation_cannot_override_actual_model_fingerprint() {
        let directory = std::env::temp_dir().join(format!(
            "forum-translator-fingerprint-{}",
            forum_contracts::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("config.json"), b"{}").unwrap();
        std::fs::write(
            directory.join("model.safetensors"),
            b"synthetic model content, never loaded",
        )
        .unwrap();
        let actual = forum_runtime::model_fingerprint(&directory).unwrap();
        assert_eq!(model_manifest(&directory, None).unwrap(), actual);
        assert_eq!(model_manifest(&directory, Some(" ")).unwrap(), actual);
        assert_eq!(model_manifest(&directory, Some(&actual)).unwrap(), actual);
        assert!(model_manifest(&directory, Some("qwen3.5-2b-local")).is_err());
        std::fs::write(
            directory.join("model.safetensors"),
            b"changed synthetic content",
        )
        .unwrap();
        assert!(model_manifest(&directory, Some(&actual)).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
