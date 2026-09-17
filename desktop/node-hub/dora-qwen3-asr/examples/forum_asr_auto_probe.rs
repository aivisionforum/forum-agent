//! Experimental auto-language probe: built only by probe_forum_models_auto_patch.py.
//! It requires that script's isolated two-change dependency patch, never the production crate.
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use qwen3_asr_mlx::{audio, Qwen3ASR, SamplingConfig};
    use serde::Deserialize;
    use std::{io::Write, path::PathBuf, time::Instant};
    #[derive(Deserialize)]
    struct Case {
        id: String,
        wav: PathBuf,
    }
    ensure!(
        option_env!("FORUM_EXPERIMENTAL_AUTO_PATCH") == Some("1"),
        "build using the isolated F01 auto-patch script only"
    );
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: forum-asr-auto-experiment MODEL CASES OUTPUT"
    );
    let model_path = PathBuf::from(&args[0]);
    ensure!(
        model_path.is_absolute() && model_path.join("tokenizer.json").is_file(),
        "local model and cached tokenizer required"
    );
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let mut output = std::io::BufWriter::new(std::fs::File::create(&args[2])?);
    let started = Instant::now();
    let mut model = Qwen3ASR::load(&model_path)?;
    let load_ms = started.elapsed().as_secs_f64() * 1000.0;
    for case in cases {
        let (samples, rate) = audio::load_wav(&case.wav)?;
        let samples = audio::resample(&samples, rate, 16000)?;
        let started = Instant::now();
        // In the experiment ONLY, empty language omits the forced assistant suffix.
        let raw = model.transcribe_samples_with_config(
            &samples,
            "",
            &SamplingConfig {
                temperature: 0.0,
                max_tokens: 1024,
            },
        )?;
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        let parsed = raw.split_once("<asr_text>").and_then(|(prefix, text)| {
            let prefix = prefix
                .trim()
                .strip_prefix("<|im_start|>")
                .unwrap_or(prefix.trim());
            let language = prefix.strip_prefix("language ").unwrap_or(prefix).trim();
            matches!(
                language,
                "Chinese" | "English" | "Chinese,English" | "English,Chinese" | "None"
            )
            .then_some((language, text.trim()))
        });
        let (language, text) = parsed
            .map(|(language, text)| (Some(language), text))
            .unwrap_or((None, raw.as_str()));
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({"event":"case_result","case_id":case.id,"configured_language":null,"detected_language_raw":language,"raw_output":raw,"text":text,"language_protocol_parsed":parsed.is_some(),"inference_ms":elapsed,"load_ms":load_ms,"audio_ms":samples.len() as f64/16.0,"scope":"experimental isolated prompt/decode patch; not production binding, language is per utterance rather than guaranteed spans"}),
        )?;
        writeln!(output)?;
        output.flush()?;
    }
    Ok(())
}
