//! F01 text-only smoke probe using the production MLX worker, without Dora or audio.
//! Reads a JSON case array from a local file (or stdin with `-`). Never downloads weights.
#[cfg(target_os = "macos")]
#[path = "../src/backend_mlx.rs"]
mod backend_mlx;
#[path = "../src/generation_control.rs"]
mod generation_control;

// The binary's private wire types are reproduced here only to call its unchanged backend.
#[derive(Debug)]
struct TranslationTask {
    commit_id: i64,
    source_text: String,
    system_prompt: String,
    user_prompt: String,
    direction: DirectionMeta,
    control: generation_control::GenerationControl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectionMeta {
    source_language: String,
    target_language: String,
    epoch: i64,
}

#[derive(Debug)]
struct TranslationResponse {
    commit_id: i64,
    source_text: String,
    output: Result<String, String>,
    direction: DirectionMeta,
}

#[derive(Debug)]
enum TranslationWorkerEvent {
    Streaming {
        commit_id: i64,
        source_text: String,
        translation: String,
        direction: DirectionMeta,
    },
    Complete(TranslationResponse),
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use anyhow::{anyhow, ensure, Context};
    use serde::Deserialize;
    use std::{
        collections::HashSet,
        io::{Read, Write},
        path::PathBuf,
        sync::mpsc,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Case {
        id: String,
        source_language: String,
        target_language: String,
        text: String,
        #[serde(default)]
        cancel_after_streams: Option<usize>,
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: forum_translation_probe MODEL_DIR CASES_JSON_OR_- OUTPUT_JSONL"
    );
    let model_dir = PathBuf::from(&args[0]);
    ensure!(
        model_dir.is_absolute() && model_dir.is_dir(),
        "absolute local model directory is required"
    );
    for file in ["config.json", "tokenizer.json", "tokenizer_config.json"] {
        ensure!(
            model_dir.join(file).is_file(),
            "missing local model file: {file}"
        );
    }
    ensure!(
        std::fs::read_dir(&model_dir)?
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "safetensors")
            }),
        "local safetensors weights are required"
    );
    let cases_bytes = if args[1] == "-" {
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input)?;
        input
    } else {
        std::fs::read(&args[1])?
    };
    let cases: Vec<Case> = serde_json::from_slice(&cases_bytes)?;
    ensure!(!cases.is_empty(), "at least one case is required");
    let mut case_ids = HashSet::new();
    for case in &cases {
        ensure!(
            !case.id.is_empty() && case_ids.insert(&case.id),
            "case IDs must be nonempty and unique"
        );
        ensure!(
            !case.text.trim().is_empty(),
            "case {} has empty text",
            case.id
        );
        ensure!(
            case.cancel_after_streams != Some(0),
            "cancel_after_streams must be positive"
        );
        ensure!(
            matches!(case.source_language.as_str(), "zh" | "en" | "mixed"),
            "unsupported source language"
        );
        ensure!(
            matches!(case.target_language.as_str(), "zh" | "en"),
            "unsupported target language"
        );
    }
    // A dedicated file keeps dependency diagnostics from corrupting the JSONL stream.
    let mut results = std::io::BufWriter::new(std::fs::File::create(&args[2])?);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    let worker_model_dir = model_dir.clone();
    let load_start = Instant::now();
    let worker = std::thread::spawn(move || {
        // Production defaults, except warm-up is off to measure load separately.
        backend_mlx::translation_worker_loop(
            worker_model_dir,
            0.0,
            256,
            false,
            ready_tx,
            request_rx,
            response_tx,
        );
    });
    ready_rx
        .recv_timeout(Duration::from_secs(120))
        .context("model did not become ready")?
        .map_err(|error| anyhow!(error))?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
    let mut failed = false;
    for (index, case) in cases.into_iter().enumerate() {
        let commit_id = i64::try_from(index + 1)?;
        let direction = DirectionMeta {
            source_language: case.source_language.clone(),
            target_language: case.target_language.clone(),
            epoch: 0,
        };
        let target_display = if case.target_language == "zh" {
            "Chinese"
        } else {
            "English"
        };
        // These are the current main.rs build_system_prompt/build_translation_user_prompt.
        let system_prompt = format!(
            "/no_think You are a translation engine. Translate the source text into {target_display}. Output only the translated text. Do not include source/translation labels, do not explain, do not repeat the source."
        );
        let user_prompt = format!("Source:\n{}", case.text);
        let observed_at_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let started = Instant::now();
        let control = generation_control::GenerationControl::new(Duration::from_secs(45), true);
        request_tx.send(TranslationTask {
            commit_id,
            source_text: case.text.clone(),
            system_prompt,
            user_prompt,
            direction: direction.clone(),
            control: control.clone(),
        })?;
        let mut first_stream_ms = None;
        let mut first_stream_text = None;
        let mut first_nonempty_stream_ms = None;
        let mut stream_count = 0;
        let mut cancellation_requested_ms = None;
        let completion = loop {
            let remaining = Duration::from_secs(60).saturating_sub(started.elapsed());
            let event = response_rx
                .recv_timeout(remaining)
                .context("translation did not finish within 60 seconds")?;
            let event_ms = started.elapsed().as_secs_f64() * 1000.0;
            match event {
                TranslationWorkerEvent::Streaming {
                    commit_id: received_id,
                    source_text,
                    translation,
                    direction: received_direction,
                } => {
                    ensure!(
                        received_id == commit_id
                            && source_text == case.text
                            && received_direction == direction,
                        "stream event attribution mismatch"
                    );
                    stream_count += 1;
                    if case.cancel_after_streams == Some(stream_count) {
                        control.cancel();
                        cancellation_requested_ms = Some(event_ms);
                    }
                    if first_nonempty_stream_ms.is_none() && !translation.trim().is_empty() {
                        first_nonempty_stream_ms = Some(event_ms);
                    }
                    if first_stream_ms.is_none() {
                        first_stream_ms = Some(event_ms);
                        first_stream_text = Some(translation);
                    }
                }
                TranslationWorkerEvent::Complete(response) => {
                    ensure!(
                        response.commit_id == commit_id
                            && response.source_text == case.text
                            && response.direction == direction,
                        "completion event attribution mismatch"
                    );
                    break response.output;
                }
            }
        };
        let final_ms = started.elapsed().as_secs_f64() * 1000.0;
        let cancellation_observed = completion
            .as_ref()
            .err()
            .is_some_and(|message| message.contains("CANCELLED"));
        failed |= if case.cancel_after_streams.is_some() {
            !cancellation_observed
        } else {
            completion.is_err()
        };
        let (translation, error) = match completion {
            Ok(text) => (Some(text), None),
            Err(error) => (None, Some(error)),
        };
        serde_json::to_writer(
            &mut results,
            &serde_json::json!({
                "case_id": case.id,
                "observed_at_unix_ms": observed_at_unix_ms,
                "input_kind": "synthetic_text",
                "expected_cancelled":case.cancel_after_streams.is_some(),
                "cancellation_requested_ms":cancellation_requested_ms,
                "cancellation_observed":cancellation_observed,
                "source_language": case.source_language,
                "target_language": case.target_language,
                "source_text": case.text,
                "translation": translation,
                "error": error,
                "load_ms": load_ms,
                "first_stream_ms": first_stream_ms,
                "first_stream_text": first_stream_text,
                "first_nonempty_stream_ms": first_nonempty_stream_ms,
                "final_ms": final_ms,
                "stream_count": stream_count,
                "streaming_measurement": "worker cumulative callback, batches of up to 5 tokens",
                "warmup_enabled": false,
                "load_measurement": "worker ready after model/tokenizer init; first request may include lazy evaluation cost",
                "first_request_after_load": index == 0,
                "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
                "temperature": 0.0,
                "max_tokens": 256,
                "model_path": model_dir,
                "model_revision": null,
                "runtime_source_revision": "6aac996db8b71fb7dae7a2409c46b4f2ade93092",
                "scope": "direct MLX worker only; no ASR, Dora, UI, audio, concurrency or meeting-quality validation"
            }),
        )?;
        writeln!(results)?;
        results.flush()?;
    }
    drop(request_tx);
    worker
        .join()
        .map_err(|_| anyhow!("translation worker panicked"))?;
    ensure!(!failed, "one or more translation cases failed; see JSONL");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the F01 translation probe requires macOS Apple Silicon")
}
