//! File-only F01 smoke probe. Never opens an audio device or downloads weights.
#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use qwen3_asr_mlx::{audio, Qwen3ASR, SamplingConfig};
    use serde::Deserialize;
    use std::{path::PathBuf, time::Instant};

    #[derive(Deserialize)]
    struct Case {
        id: String,
        wav: PathBuf,
        language: String,
    }

    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(args.len() == 2, "usage: forum_asr_probe MODEL_DIR CASES_JSON");
    let model_dir = PathBuf::from(&args[0]);
    anyhow::ensure!(model_dir.is_dir(), "local model directory is required");
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    anyhow::ensure!(!cases.is_empty(), "at least one case is required");
    for case in &cases {
        anyhow::ensure!(case.wav.is_file(), "missing local WAV for {}", case.id);
        anyhow::ensure!(
            matches!(case.language.as_str(), "Chinese" | "English"),
            "this pinned backend requires a language hint; automatic detection is unverified"
        );
    }
    let load_start = Instant::now();
    let mut model = Qwen3ASR::load(&model_dir)?;
    let load_ms = load_start.elapsed().as_millis();
    for case in cases {
        let (samples, sample_rate) = audio::load_wav(&case.wav)?;
        let samples = audio::resample(&samples, sample_rate, 16000)?;
        let start = Instant::now();
        let text = model.transcribe_samples_with_config(
            &samples,
            &case.language,
            &SamplingConfig { temperature: 0.0, max_tokens: 1024 },
        )?;
        println!("{}", serde_json::json!({
            "case_id": case.id, "configured_language": case.language,
            "detected_language": null, "text": text, "load_ms": load_ms,
            "inference_ms": start.elapsed().as_millis(),
            "audio_ms": samples.len() as f64 / 16.0,
            "streaming": false, "model_revision": null,
            "runtime_source_revision": "6aac996db8b71fb7dae7a2409c46b4f2ade93092"
        }));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the F01 ASR probe requires macOS Apple Silicon")
}
