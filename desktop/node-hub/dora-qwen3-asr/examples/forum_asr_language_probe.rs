//! F01 file-only language-binding and process-lifecycle probe.
//! Literal empty/auto hints are deliberate diagnostics, never claimed as detection.
#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use qwen3_asr_mlx::{audio, Qwen3ASR, SamplingConfig};
    use serde::Deserialize;
    use std::{
        io::Write,
        path::PathBuf,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };
    #[derive(Deserialize)]
    struct Case {
        id: String,
        wav: PathBuf,
        language: String,
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: forum_asr_language_probe MODEL_DIR CASES_JSON OUTPUT_JSONL"
    );
    let model_dir = PathBuf::from(&args[0]);
    ensure!(
        model_dir.is_absolute() && model_dir.is_dir(),
        "absolute local model path required"
    );
    // Upstream otherwise writes tokenizer.json into the shared model directory.
    ensure!(
        model_dir.join("tokenizer.json").is_file(),
        "cached tokenizer required for read-only probe"
    );
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    ensure!(!cases.is_empty(), "at least one case required");
    for case in &cases {
        ensure!(case.wav.is_file(), "missing local WAV: {}", case.id);
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(&args[2])?);
    let origin = Instant::now();
    let mut emit = |event: serde_json::Value| -> anyhow::Result<()> {
        let mut event = event;
        event["unix_ms"] =
            serde_json::json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis());
        event["process_elapsed_ms"] = serde_json::json!(origin.elapsed().as_secs_f64() * 1000.0);
        serde_json::to_writer(&mut out, &event)?;
        writeln!(out)?;
        out.flush()?;
        Ok(())
    };
    emit(serde_json::json!({"event":"loading","pid":std::process::id()}))?;
    let load = Instant::now();
    let mut model = Qwen3ASR::load(&model_dir)?;
    let load_ms = load.elapsed().as_secs_f64() * 1000.0;
    emit(serde_json::json!({"event":"ready","load_ms":load_ms}))?;
    let admitted = Instant::now();
    for (index, case) in cases.into_iter().enumerate() {
        let queued_ms = admitted.elapsed().as_secs_f64() * 1000.0;
        let (samples, rate) = audio::load_wav(&case.wav)?;
        let samples = audio::resample(&samples, rate, 16000)?;
        emit(
            serde_json::json!({"event":"case_started","case_id":case.id,"queue_wait_ms":queued_ms,"queue_scope":"probe admits the file case array at model readiness; sequential in-process loop"}),
        )?;
        let start = Instant::now();
        let text = model.transcribe_samples_with_config(
            &samples,
            &case.language,
            &SamplingConfig {
                temperature: 0.0,
                max_tokens: 1024,
            },
        )?;
        emit(
            serde_json::json!({"event":"case_result","case_id":case.id,"configured_language":case.language,"detected_language":null,"language_binding":"forced assistant language prefix; auto and empty are literal diagnostic values","text":text,"inference_ms":start.elapsed().as_secs_f64()*1000.0,"audio_ms":samples.len() as f64/16.0,"load_ms":load_ms,"first_request_after_load":index==0,"queue_wait_ms":queued_ms,"streaming":false,"build_profile":if cfg!(debug_assertions){"debug"}else{"release"},"max_tokens":1024,"runtime_source_revision":"6aac996db8b71fb7dae7a2409c46b4f2ade93092"}),
        )?;
    }
    emit(serde_json::json!({"event":"finished"}))?;
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("macOS Apple Silicon required")
}
