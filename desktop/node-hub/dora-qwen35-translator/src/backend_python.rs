//! macOS Hy-MT2 via a persistent, owned MLX Python child.
use crate::{TranslationResponse, TranslationTask, TranslationWorkerEvent};
use anyhow::{ensure, Context, Result};
use forum_runtime::process_io::JsonLineProcess;
use serde_json::json;
use std::{path::{Path, PathBuf}, process::Command, sync::mpsc, time::{Duration, Instant}};

fn start(model: &Path, control: Option<&crate::generation_control::GenerationControl>) -> Result<JsonLineProcess> {
    let python = PathBuf::from(std::env::var("FORUM_TRANSLATOR_PYTHON").context("translation runtime not configured")?);
    let script = PathBuf::from(std::env::var("FORUM_TRANSLATOR_SCRIPT").context("translation adapter not configured")?);
    ensure!(python.is_absolute() && python.is_file() && script.is_absolute() && script.is_file(), "translation runtime must be existing absolute files");
    let mut command = Command::new(python);
    command.args(["-I", "-B"]).arg(script).arg("--model").arg(model);
    for key in ["PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV", "CONDA_PREFIX", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"] { command.env_remove(key); }
    command.env("HF_HUB_OFFLINE", "1");
    let process = JsonLineProcess::spawn(command, 256 * 1024)?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let ready = loop {
        if let Some(control) = control { control.check()?; }
        ensure!(Instant::now() < deadline, "translation model load deadline");
        if let Some(message) = process.poll(Duration::from_millis(25))? { break message; }
    };
    ensure!(ready["type"] == "ready" && ready["protocol_version"] == 1 && ready["backend"] == "hy-mt2-mlx", "Hy-MT2 adapter failed to load");
    Ok(process)
}

fn generate(process: &mut JsonLineProcess, task: &TranslationTask, temperature: f32, max_tokens: usize, responses: &mpsc::Sender<TranslationWorkerEvent>) -> Result<String> {
    task.control.check()?;
    let user: serde_json::Value = serde_json::from_str(&task.user_prompt).unwrap_or_default();
    let source = user["source_to_translate"].as_str().unwrap_or(&task.source_text);
    let context = user["preceding_context"].as_str().unwrap_or("");
    process.send(&json!({"id":task.commit_id,"source":source,"context":context,"target_language":task.direction.target_language,"max_tokens":max_tokens,"temperature":temperature}), Duration::from_secs(1))?;
    loop {
        task.control.check()?;
        let Some(message) = process.poll(Duration::from_millis(25))? else { continue; };
        task.control.check()?;
        ensure!(message["id"] == task.commit_id, "translation response ID mismatch");
        match message["type"].as_str() {
            Some("partial") => {
                responses.send(TranslationWorkerEvent::Streaming {
                    commit_id: task.commit_id, source_text: task.source_text.clone(),
                    translation: message["text"].as_str().context("invalid translation partial")?.into(),
                    direction: task.direction.clone(),
                })?;
            }
            Some("complete") => {
                task.control.finish(message["saw_eos"].as_bool().context("missing translation completion status")?)?;
                let text = message["text"].as_str().context("invalid translation result")?.trim();
                ensure!(!text.is_empty(), "empty translation result");
                return Ok(text.into());
            }
            _ => anyhow::bail!("TRANSLATOR_INFERENCE_FAILED"),
        }
    }
}

pub(crate) fn translation_worker_loop(model_path: PathBuf, temperature: f32, max_tokens: usize, _warmup_enabled: bool,
    ready_tx: mpsc::Sender<Result<(), String>>, request_rx: mpsc::Receiver<TranslationTask>, response_tx: mpsc::Sender<TranslationWorkerEvent>) {
    let mut process = match start(&model_path, None) {
        Ok(process) => { let _ = ready_tx.send(Ok(())); Some(process) },
        Err(error) => { let _ = ready_tx.send(Err(error.to_string())); return; }
    };
    while let Ok(task) = request_rx.recv() {
        let output = (|| -> Result<String> {
            task.control.check()?;
            if process.is_none() { process = Some(start(&model_path, Some(&task.control))?); }
            generate(process.as_mut().unwrap(), &task, temperature, max_tokens, &response_tx)
        })().map_err(|error| error.to_string());
        // A cancelled/deadline/budget failure cannot leak tokens into the next request.
        if output.is_err() { process.take(); }
        if response_tx.send(TranslationWorkerEvent::Complete(TranslationResponse {
            commit_id: task.commit_id, source_text: task.source_text, output, direction: task.direction,
        })).is_err() { break; }
    }
}
