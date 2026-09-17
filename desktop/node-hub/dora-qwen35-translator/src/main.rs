//! dora-qwen35-translator: real-time translation Dora node powered by qwen3.5-35B-mlx.
//!
//! The translator treats upstream ASR as a text provider.
//!
//! Pipeline position:
//!   dora-qwen3-asr + mic bridge
//!      → dora-qwen35-translator
//!      → [source_text, translation]
//!
//! # Inputs
//!   text – StringArray (single element: latest ASR text chunk)
//!
//! # Outputs
//!   source_text  – current transcript tail or committed sentence
//!   translation  – translated committed sentence
//!   log          – status / debug messages
//!
//! Internally, the node maintains a continuously growing transcript buffer.
//! Same-burst chunks replace the active burst, new bursts seal the previous
//! one, and only sealed text participates in periodic translation commits.

mod transcript_buffer;

use anyhow::{anyhow, Result};
use arrow::array::{Array, StringArray};
use dora_node_api::{DoraNode, Event, IntoArrow, TryRecvError};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use transcript_buffer::TranscriptBuffer;

// Platform inference backend: MLX on macOS, managed llama-server on Windows.
#[cfg(target_os = "macos")]
#[path = "backend_mlx.rs"]
mod backend;
#[cfg(windows)]
#[path = "backend_llamacpp.rs"]
mod backend;

#[cfg(not(any(target_os = "macos", windows)))]
compile_error!(
    "dora-qwen35-translator currently supports only macOS (MLX) and Windows (llama.cpp)"
);

// ── Dora output helper ───────────────────────────────────────────────────────

fn send_str(
    node: &mut DoraNode,
    output: &str,
    value: &str,
    meta: BTreeMap<String, dora_node_api::Parameter>,
) -> Result<()> {
    let arr = vec![value.to_string()].into_arrow();
    node.send_output(output.into(), meta, arr)
        .map_err(|e| anyhow!("send_output({output}) failed: {e}"))
}

fn send_log(node: &mut DoraNode, msg: &str) -> Result<()> {
    send_str(node, "log", msg, BTreeMap::new())
}

fn build_session_meta(
    status: &str,
    question_id: Option<i64>,
    commit_id: Option<i64>,
    direction: Option<&DirectionMeta>,
) -> BTreeMap<String, dora_node_api::Parameter> {
    let mut meta = BTreeMap::new();
    meta.insert(
        "session_status".into(),
        dora_node_api::Parameter::String(status.into()),
    );
    if let Some(qid) = question_id {
        meta.insert(
            "question_id".to_string(),
            dora_node_api::Parameter::Integer(qid),
        );
    }
    if let Some(commit_id) = commit_id {
        meta.insert(
            "commit_id".to_string(),
            dora_node_api::Parameter::Integer(commit_id),
        );
    }
    if let Some(direction) = direction {
        meta.insert(
            "source_language".to_string(),
            dora_node_api::Parameter::String(direction.source_language.clone()),
        );
        meta.insert(
            "target_language".to_string(),
            dora_node_api::Parameter::String(direction.target_language.clone()),
        );
        meta.insert(
            "direction_epoch".to_string(),
            dora_node_api::Parameter::Integer(direction.epoch),
        );
    }
    meta
}

fn send_source(
    node: &mut DoraNode,
    text: &str,
    status: &str,
    question_id: Option<i64>,
    commit_id: Option<i64>,
    direction: Option<&DirectionMeta>,
) -> Result<()> {
    send_str(
        node,
        "source_text",
        text,
        build_session_meta(status, question_id, commit_id, direction),
    )
}

fn send_translation_chunk(
    node: &mut DoraNode,
    chunk: &str,
    status: &str,
    question_id: Option<i64>,
    commit_id: Option<i64>,
    source_text: Option<&str>,
    direction: Option<&DirectionMeta>,
) -> Result<()> {
    let mut meta = build_session_meta(status, question_id, commit_id, direction);
    if let Some(source_text) = source_text {
        meta.insert(
            "source_text".to_string(),
            dora_node_api::Parameter::String(source_text.to_string()),
        );
    }
    send_str(node, "translation", chunk, meta)
}

// ── Language helpers ─────────────────────────────────────────────────────────

fn normalize_lang(raw: &str) -> &'static str {
    match raw.trim().to_lowercase().as_str() {
        "zh" | "zh-cn" | "chinese" | "cn" => "Chinese",
        "en" | "en-us" | "english" => "English",
        "fr" | "french" => "French",
        "ja" | "jp" | "japanese" => "Japanese",
        "ko" | "korean" => "Korean",
        "de" | "german" => "German",
        "es" | "spanish" => "Spanish",
        "ru" | "russian" => "Russian",
        _ => "English", // safe fallback
    }
}

/// Full language name used in the translation prompt.
fn lang_display(code: &str) -> &'static str {
    normalize_lang(code)
}

fn should_drop_low_info_chunk(chunk: &str, src_lang: &str) -> bool {
    let t = chunk.trim().trim_matches(|c: char| {
        c.is_ascii_punctuation() || c.is_whitespace() || "，。！？；：、“”‘’（）()…".contains(c)
    });
    if t.is_empty() {
        return true;
    }

    // Chinese filler words / non-lexical short utterances that are commonly
    // triggered by breath/noise and should not be translated as standalone text.
    if normalize_lang(src_lang) == "Chinese" {
        const FILLERS: &[&str] = &[
            "嗯", "啊", "呃", "额", "唔", "哦", "噢", "哎", "哈", "嗯嗯", "啊啊", "呃呃",
        ];
        if FILLERS.contains(&t) {
            return true;
        }
    }

    false
}

fn warmup_enabled_from_raw(raw: Option<&str>) -> bool {
    !matches!(
        raw.unwrap_or("1").trim().to_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// Detect chain-of-thought leakage: small non-thinking models occasionally
/// answer a translation request with reasoning about the task instead of the
/// translation itself (observed on Qwen3-1.7B under CPU load, where greedy
/// sampling can flip onto a reasoning path). Callers should retry once.
#[cfg(target_os = "windows")]
pub(crate) fn looks_like_reasoning(output: &str) -> bool {
    const MARKERS: &[&str] = &[
        // zh reasoning preambles / meta-talk
        "好的，我", "好的,我", "我需要", "让我来", "首先，我", "首先,我", "原文", "翻译为", "用户提供的",
        // en reasoning preambles
        "Let me", "I need to", "I will translate", "The user wants", "First, I",
    ];
    MARKERS.iter().any(|m| output.contains(m))
}

// ── Model path resolution ────────────────────────────────────────────────────

fn resolve_model_path() -> PathBuf {
    // 1. Explicit env override
    if let Ok(v) = std::env::var("QWEN35_TRANSLATOR_MODEL_PATH") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    // 2. Platform default (macOS: MLX model dir; Windows: GGUF file)
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    {
        base.join(".OminiX")
            .join("models")
            .join("Qwen3.5-2B-MLX-4bit")
    }
    #[cfg(windows)]
    {
        base.join(".OminiX")
            .join("models")
            .join("Qwen3-1.7B-Q4_K_M.gguf")
    }
}

// ── Translation system prompt ─────────────────────────────────────────────────

const COMMIT_THRESHOLD_CHARS: usize = 10;
const COMMIT_TICK_MS: u64 = 500;
const IDLE_FLUSH_MS_DEFAULT: u64 = 9000;
const EVENT_POLL_SLEEP_MS: u64 = 25;
const STOP_DRAIN_TIMEOUT_MS_DEFAULT: u64 = 3000;

#[derive(Debug)]
struct TranslationTask {
    commit_id: i64,
    source_text: String,
    system_prompt: String,
    user_prompt: String,
    direction: DirectionMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectionMeta {
    source_language: String,
    target_language: String,
    epoch: i64,
}

#[derive(Debug)]
struct AsrChunk {
    burst_id: Option<i64>,
    transcription_mode: String,
    segment_reason: Option<String>,
    text: String,
    direction: DirectionMeta,
}

fn direction_from_parameters(
    parameters: &BTreeMap<String, dora_node_api::Parameter>,
    fallback: &DirectionMeta,
) -> DirectionMeta {
    let string_value = |key: &str| {
        parameters.get(key).and_then(|parameter| match parameter {
            dora_node_api::Parameter::String(value) => Some(value.clone()),
            _ => None,
        })
    };
    DirectionMeta {
        source_language: string_value("source_language")
            .unwrap_or_else(|| fallback.source_language.clone()),
        target_language: string_value("target_language")
            .unwrap_or_else(|| fallback.target_language.clone()),
        epoch: parameters
            .get("direction_epoch")
            .and_then(|parameter| match parameter {
                dora_node_api::Parameter::Integer(value) => Some(*value),
                _ => None,
            })
            .unwrap_or(fallback.epoch),
    }
}

fn apply_asr_chunk(
    node: &mut DoraNode,
    transcript: &mut TranscriptBuffer,
    input: AsrChunk,
    current_burst_id: &mut Option<i64>,
    last_asr_chunk_at: &mut Option<Instant>,
    last_streaming_at: &mut Option<Instant>,
    last_streaming_text: &mut String,
) -> bool {
    if input.burst_id.is_some() {
        *current_burst_id = input.burst_id;
    }
    let chunk_is_usable = !input.text.is_empty()
        && !should_drop_low_info_chunk(&input.text, &input.direction.source_language);

    if chunk_is_usable {
        tracing::info!(
            "Received ASR chunk\nepoch={}\nburst_id={:?}\nmode={}\nsegment_reason={:?}\nchunk=\n{}",
            input.direction.epoch,
            current_burst_id,
            input.transcription_mode,
            input.segment_reason,
            input.text
        );
    } else {
        tracing::info!(
            "Ignoring non-usable ASR chunk\nepoch={}\nburst_id={:?}\nmode={}\nsegment_reason={:?}\nchunk_is_empty={}",
            input.direction.epoch,
            current_burst_id,
            input.transcription_mode,
            input.segment_reason,
            input.text.is_empty()
        );
        return false;
    }

    *last_asr_chunk_at = Some(Instant::now());
    let chunk_for_buffer = if input.segment_reason.as_deref() == Some("max_segment") {
        strip_hard_cut_terminal_punctuation(&input.text)
    } else {
        input.text
    };
    let mut changed = transcript.update_from_chunk(input.burst_id, &chunk_for_buffer);
    let mut schedule_requested = false;
    if seal_final_asr_chunk(transcript, &input.transcription_mode) {
        changed = true;
        schedule_requested = true;
    }

    tracing::info!(
        "Transcript state after chunk (changed={})\n{}",
        changed,
        transcript.debug_snapshot()
    );

    let tail = transcript.uncommitted_tail();
    if !tail.is_empty()
        && should_emit_streaming(
            &tail,
            last_streaming_text,
            *last_streaming_at,
            Duration::from_millis(500),
        )
    {
        let _ = send_source(
            node,
            &tail,
            "streaming",
            *current_burst_id,
            None,
            Some(&input.direction),
        );
        *last_streaming_at = Some(Instant::now());
        *last_streaming_text = tail;
    }
    schedule_requested
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

fn build_system_prompt(tgt_lang: &str) -> String {
    format!(
        "/no_think You are a translation engine. Translate the source text into {tgt}. Output only the translated text. Do not include source/translation labels, do not explain, do not repeat the source.",
        tgt = lang_display(tgt_lang)
    )
}

fn build_translation_user_prompt(source_text: &str) -> String {
    format!("Source:\n{source_text}")
}

fn format_commit_prompt_debug(system_prompt: &str, user_prompt: &str) -> String {
    format!("system_prompt=\n{system_prompt}\n\nuser_prompt=\n{user_prompt}")
}

fn strip_hard_cut_terminal_punctuation(chunk: &str) -> String {
    let trimmed = chunk.trim_end();
    let mut out = trimmed.to_string();
    if matches!(
        out.chars().last(),
        Some('。' | '.' | '！' | '!' | '？' | '?')
    ) {
        out.pop();
    }
    out
}

fn find_commit_boundary_from_tail(text: &str) -> Option<usize> {
    text.char_indices().rev().find_map(|(idx, ch)| match ch {
        '，' | ',' | '。' | '.' | '！' | '!' | '？' | '?' | '；' | ';' => {
            Some(idx + ch.len_utf8())
        }
        _ => None,
    })
}

fn should_emit_streaming(
    new_tail: &str,
    last_tail: &str,
    last_emit_at: Option<Instant>,
    min_interval: Duration,
) -> bool {
    if new_tail == last_tail {
        return false;
    }
    match last_emit_at {
        Some(t) if t.elapsed() < min_interval => false,
        _ => true,
    }
}

fn should_trigger_idle_flush(
    elapsed_since_last_chunk: Option<Duration>,
    has_buffered_text: bool,
    translation_pending: bool,
    flush_requested: bool,
    idle_flush_ms: u64,
) -> bool {
    !translation_pending
        && !flush_requested
        && has_buffered_text
        && elapsed_since_last_chunk
            .map(|elapsed| elapsed >= Duration::from_millis(idle_flush_ms))
            .unwrap_or(false)
}

fn stop_drain_timed_out(stop_started_at: Option<Instant>, stop_drain_timeout_ms: u64) -> bool {
    stop_started_at
        .map(|started_at| started_at.elapsed() >= Duration::from_millis(stop_drain_timeout_ms))
        .unwrap_or(false)
}

fn seal_final_asr_chunk(transcript: &mut TranscriptBuffer, transcription_mode: &str) -> bool {
    transcription_mode == "final" && transcript.seal_active_burst()
}

fn submit_translation_task(
    request_tx: &mpsc::Sender<TranslationTask>,
    commit_id: i64,
    source_text: &str,
    direction: &DirectionMeta,
    transcript: &TranscriptBuffer,
) -> Result<()> {
    let source_text = source_text.to_string();
    let system_prompt = build_system_prompt(&direction.target_language);
    let user_prompt = build_translation_user_prompt(&source_text);

    tracing::info!(
        "Entering commit attempt\n{}\nsource_text=\n{}\nprompt=\n{}",
        transcript.debug_snapshot(),
        source_text,
        format_commit_prompt_debug(&system_prompt, &user_prompt)
    );

    request_tx
        .send(TranslationTask {
            commit_id,
            source_text,
            system_prompt,
            user_prompt,
            direction: direction.clone(),
        })
        .map_err(|e| anyhow!("failed to send translation task to worker: {e}"))?;

    Ok(())
}

/// Passthrough commit: emit the stable source as a finalized sentence with an
/// empty translation. Used when the UI selects "no translation" as target.
/// No LLM is invoked; the listener finalizes immediately because translation
/// events carry the matching `source_text` metadata.
fn commit_passthrough(
    node: &mut DoraNode,
    transcript: &mut TranscriptBuffer,
    next_commit_id: &mut i64,
    source_text: &str,
    direction: &DirectionMeta,
) -> bool {
    if source_text.is_empty() {
        return false;
    }
    match transcript.consume_stable_prefix(source_text) {
        Ok(()) => {
            let commit_id = *next_commit_id;
            *next_commit_id += 1;
            tracing::info!(
                "[passthrough] Committed stable prefix\ncommit_id={}\nsource_text=\n{}",
                commit_id,
                source_text,
            );
            let _ = send_source(
                node,
                source_text,
                "complete",
                None,
                Some(commit_id),
                Some(direction),
            );
            let _ = send_translation_chunk(
                node,
                "",
                "complete",
                None,
                Some(commit_id),
                Some(source_text),
                Some(direction),
            );
            true
        }
        Err(e) => {
            tracing::error!("[passthrough] Failed to consume stable prefix: {e}");
            let _ = send_log(
                node,
                &format!("[passthrough] Failed to consume stable prefix: {e}"),
            );
            false
        }
    }
}

fn handle_translation_response(
    node: &mut DoraNode,
    transcript: &mut TranscriptBuffer,
    response: TranslationResponse,
) -> bool {
    let TranslationResponse {
        commit_id,
        source_text,
        output,
        direction,
    } = response;

    if source_text.is_empty() {
        return false;
    }

    let translation = match output {
        Ok(output) => output,
        Err(e) => {
            tracing::error!("Translation generation failed: {e}");
            let _ = send_log(node, &format!("Translation generation failed: {e}"));
            let _ = send_translation_chunk(
                node,
                "",
                "failed",
                None,
                Some(commit_id),
                Some(&source_text),
                Some(&direction),
            );
            return false;
        }
    };
    if translation.trim().is_empty() {
        let _ = send_translation_chunk(
            node,
            "",
            "failed",
            None,
            Some(commit_id),
            Some(&source_text),
            Some(&direction),
        );
        return false;
    }

    match transcript.consume_stable_prefix(&source_text) {
        Ok(()) => {
            tracing::info!(
                "Committed stable prefix\ncommit_id={}\nsource_text=\n{}\n{}",
                commit_id,
                source_text,
                transcript.debug_snapshot()
            );
            let _ = send_source(
                node,
                &source_text,
                "complete",
                None,
                Some(commit_id),
                Some(&direction),
            );
            let _ = send_translation_chunk(
                node,
                &translation,
                "complete",
                None,
                Some(commit_id),
                Some(&source_text),
                Some(&direction),
            );
            true
        }
        Err(e) => {
            tracing::error!("Failed to consume committed stable prefix: {e}");
            let _ = send_log(
                node,
                &format!("Failed to consume committed stable prefix: {e}"),
            );
            let _ = send_translation_chunk(
                node,
                "",
                "failed",
                None,
                Some(commit_id),
                Some(&source_text),
                Some(&direction),
            );
            false
        }
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("LOG_LEVEL")
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("dora-qwen35-translator starting");

    let (mut node, mut events) =
        DoraNode::init_from_env().map_err(|e| anyhow!("Failed to init Dora node: {e}"))?;

    let src_lang = std::env::var("SRC_LANG").unwrap_or_else(|_| "zh".into());
    let tgt_lang = std::env::var("TGT_LANG").unwrap_or_else(|_| "en".into());
    let passthrough = matches!(
        std::env::var("PASSTHROUGH")
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    ) || tgt_lang.trim().eq_ignore_ascii_case("none");
    let temperature: f32 = std::env::var("TEMPERATURE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let max_tokens: usize = std::env::var("MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let idle_flush_ms: u64 = std::env::var("TRANSLATOR_IDLE_FLUSH_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(IDLE_FLUSH_MS_DEFAULT);
    let stop_drain_timeout_ms: u64 = std::env::var("TRANSLATOR_STOP_DRAIN_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(STOP_DRAIN_TIMEOUT_MS_DEFAULT);
    let warmup_env = std::env::var("TRANSLATOR_WARMUP").ok();
    let warmup_enabled = warmup_enabled_from_raw(warmup_env.as_deref());
    if passthrough {
        tracing::info!(
            "Translator running in passthrough mode (no LLM); src={}",
            src_lang
        );
        let _ = send_log(
            &mut node,
            "Translator passthrough mode — no translation will be generated",
        );
    } else {
        tracing::info!("Translation: {} → {}", src_lang, tgt_lang);
    }

    // In passthrough mode, skip model/worker setup entirely.
    // Binding `_` (not `_name`) drops the paired sender immediately so the
    // response channel returns Disconnected on `try_recv` without ever yielding.
    let (request_tx, response_rx, mut worker_handle) = if passthrough {
        let (dummy_tx, _) = mpsc::channel::<TranslationTask>();
        let (_, never_rx) = mpsc::channel::<TranslationWorkerEvent>();
        (dummy_tx, never_rx, None)
    } else {
        let model_path = resolve_model_path();
        tracing::info!("Loading Qwen3.5 model from: {}", model_path.display());
        let _ = send_log(
            &mut node,
            &format!("Loading Qwen3.5 model from {}", model_path.display()),
        );

        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_model_path = model_path.clone();
        let worker_handle = thread::spawn(move || {
            backend::translation_worker_loop(
                worker_model_path,
                temperature,
                max_tokens,
                warmup_enabled,
                ready_tx,
                request_rx,
                response_tx,
            )
        });

        match ready_rx.recv() {
            Ok(Ok(())) => {
                tracing::info!("Qwen3.5 model loaded");
                let _ = send_log(&mut node, "Qwen3.5 model loaded - ready to translate");
            }
            Ok(Err(e)) => {
                let _ = worker_handle.join();
                return Err(anyhow!("Failed to initialize translation worker: {e}"));
            }
            Err(e) => {
                let _ = worker_handle.join();
                return Err(anyhow!("Translation worker did not report readiness: {e}"));
            }
        }

        (request_tx, response_rx, Some(worker_handle))
    };

    tracing::info!("Buffer merge mode active");
    let _ = send_log(&mut node, "Buffer merge mode active");

    let mut transcript = TranscriptBuffer::new();
    let mut current_burst_id: Option<i64> = None;
    let mut translation_pending = false;
    let mut flush_requested = false;
    let mut stopping = false;
    let mut stop_started_at: Option<Instant> = None;
    let mut last_asr_chunk_at: Option<Instant> = None;
    let mut next_commit_id: i64 = 1;
    let mut last_commit_tick = Instant::now();
    let mut should_join_worker = true;
    // Throttle progressive source refreshes so the overlay does not relayout
    // more often than people can read them.
    const STREAMING_MIN_INTERVAL: Duration = Duration::from_millis(500);
    let mut last_streaming_at: Option<Instant> = None;
    let mut last_streaming_text: String = String::new();
    let mut active_direction = DirectionMeta {
        source_language: src_lang.clone(),
        target_language: tgt_lang.clone(),
        epoch: 0,
    };
    let mut pending_direction: Option<DirectionMeta> = None;
    let mut deferred_inputs: VecDeque<AsrChunk> = VecDeque::new();

    loop {
        let mut did_work = false;
        let mut schedule_requested = false;

        while let Ok(worker_event) = response_rx.try_recv() {
            match worker_event {
                TranslationWorkerEvent::Streaming {
                    commit_id,
                    source_text,
                    translation,
                    direction,
                } => {
                    let _ = send_translation_chunk(
                        &mut node,
                        &translation,
                        "streaming",
                        None,
                        Some(commit_id),
                        Some(&source_text),
                        Some(&direction),
                    );
                }
                TranslationWorkerEvent::Complete(response) => {
                    let did_commit =
                        handle_translation_response(&mut node, &mut transcript, response);
                    translation_pending = false;
                    schedule_requested = did_commit;

                    if did_commit {
                        let tail = transcript.uncommitted_tail();
                        if !tail.is_empty()
                            && should_emit_streaming(
                                &tail,
                                &last_streaming_text,
                                last_streaming_at,
                                STREAMING_MIN_INTERVAL,
                            )
                        {
                            let _ = send_source(
                                &mut node,
                                &tail,
                                "streaming",
                                current_burst_id,
                                None,
                                Some(&active_direction),
                            );
                            last_streaming_at = Some(Instant::now());
                            last_streaming_text = tail;
                        }
                    }

                    if flush_requested && transcript.stable_buffer().trim().is_empty() {
                        flush_requested = false;
                    }
                }
            }
            did_work = true;
        }

        if !translation_pending && transcript.buffer().trim().is_empty() {
            if let Some(next_direction) = pending_direction.take() {
                tracing::info!(
                    "Activating deferred translation direction {} -> {} (epoch={})",
                    next_direction.source_language,
                    next_direction.target_language,
                    next_direction.epoch
                );
                active_direction = next_direction;
                current_burst_id = None;
                flush_requested = false;
                last_asr_chunk_at = None;
                last_streaming_at = None;
                last_streaming_text.clear();
                while let Some(input) = deferred_inputs.pop_front() {
                    if input.direction.epoch == active_direction.epoch {
                        schedule_requested |= apply_asr_chunk(
                            &mut node,
                            &mut transcript,
                            input,
                            &mut current_burst_id,
                            &mut last_asr_chunk_at,
                            &mut last_streaming_at,
                            &mut last_streaming_text,
                        );
                    }
                }
            }
        }

        loop {
            match events.try_recv() {
                Ok(Event::Input {
                    id, data, metadata, ..
                }) => {
                    did_work = true;
                    if id.as_str() != "text" {
                        continue;
                    }

                    let burst_id = metadata
                        .parameters
                        .get("burst_id")
                        .and_then(|p| match p {
                            dora_node_api::Parameter::Integer(v) => Some(*v),
                            _ => None,
                        })
                        .or_else(|| {
                            metadata
                                .parameters
                                .get("question_id")
                                .and_then(|p| match p {
                                    dora_node_api::Parameter::Integer(v) => Some(*v),
                                    _ => None,
                                })
                        });
                    let transcription_mode = metadata
                        .parameters
                        .get("transcription_mode")
                        .and_then(|p| match p {
                            dora_node_api::Parameter::String(v) => Some(v.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "progressive".to_string());
                    let segment_reason =
                        metadata
                            .parameters
                            .get("segment_reason")
                            .and_then(|p| match p {
                                dora_node_api::Parameter::String(v) => Some(v.clone()),
                                _ => None,
                            });

                    let arr = match data.as_any().downcast_ref::<StringArray>() {
                        Some(a) if a.len() > 0 => a,
                        _ => continue,
                    };
                    let direction = direction_from_parameters(
                        &metadata.parameters,
                        pending_direction.as_ref().unwrap_or(&active_direction),
                    );
                    let input = AsrChunk {
                        burst_id,
                        transcription_mode,
                        segment_reason,
                        text: arr.value(0).trim().to_string(),
                        direction: direction.clone(),
                    };

                    if direction.epoch < active_direction.epoch {
                        tracing::warn!(
                            "Dropping stale ASR input from epoch {} (active={})",
                            direction.epoch,
                            active_direction.epoch
                        );
                        continue;
                    }
                    if direction.epoch > active_direction.epoch {
                        let replaces_pending = pending_direction
                            .as_ref()
                            .map(|pending| direction.epoch > pending.epoch)
                            .unwrap_or(true);
                        if replaces_pending {
                            pending_direction = Some(direction.clone());
                            deferred_inputs.clear();
                        }
                        if pending_direction.as_ref().map(|pending| pending.epoch)
                            == Some(direction.epoch)
                        {
                            deferred_inputs.push_back(input);
                        }
                        flush_requested = true;
                        schedule_requested = true;
                        if transcript.seal_active_burst() {
                            tracing::info!(
                                "Sealed previous direction before epoch switch\n{}",
                                transcript.debug_snapshot()
                            );
                        }
                        continue;
                    }

                    schedule_requested |= apply_asr_chunk(
                        &mut node,
                        &mut transcript,
                        input,
                        &mut current_burst_id,
                        &mut last_asr_chunk_at,
                        &mut last_streaming_at,
                        &mut last_streaming_text,
                    );
                }
                Ok(Event::Stop(_)) => {
                    did_work = true;
                    if !stopping {
                        tracing::info!("Stop event received, draining pending translation work");
                    }
                    stopping = true;
                    stop_started_at = stop_started_at.or(Some(Instant::now()));
                    flush_requested = true;
                    schedule_requested = true;
                    if transcript.seal_active_burst() {
                        tracing::info!(
                            "Active burst sealed on stop\n{}",
                            transcript.debug_snapshot()
                        );
                    }
                }
                Ok(Event::Error(err)) => {
                    tracing::error!("Translator event stream error: {err}");
                    let _ = send_log(&mut node, &format!("Translator event stream error: {err}"));
                    should_join_worker = false;
                    break;
                }
                Ok(_) => {
                    did_work = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => {
                    tracing::warn!("Translator event stream closed; exiting immediately");
                    let _ = send_log(&mut node, "Translator event stream closed");
                    should_join_worker = false;
                    break;
                }
            }
        }

        if !should_join_worker {
            break;
        }

        if last_commit_tick.elapsed() >= Duration::from_millis(COMMIT_TICK_MS) {
            last_commit_tick = Instant::now();
            did_work = true;
            schedule_requested = true;

            if should_trigger_idle_flush(
                last_asr_chunk_at.map(|instant| instant.elapsed()),
                !transcript.buffer().trim().is_empty(),
                translation_pending,
                flush_requested,
                idle_flush_ms,
            ) {
                tracing::info!(
                    "Idle flush triggered after {}ms without new ASR chunk",
                    idle_flush_ms
                );
                flush_requested = true;
                if transcript.seal_active_burst() {
                    tracing::info!(
                        "Active burst sealed on idle flush\n{}",
                        transcript.debug_snapshot()
                    );
                }
            }
        }

        if schedule_requested && !translation_pending {
            let next_source = if flush_requested {
                let stable = transcript.stable_buffer().trim();
                if stable.is_empty() {
                    None
                } else {
                    Some(stable.to_string())
                }
            } else if transcript.has_stable_text(COMMIT_THRESHOLD_CHARS) {
                let stable = transcript.stable_buffer();
                find_commit_boundary_from_tail(stable).map(|end| stable[..end].to_string())
            } else {
                None
            };

            if let Some(source_text) = next_source {
                if passthrough {
                    let did_commit = commit_passthrough(
                        &mut node,
                        &mut transcript,
                        &mut next_commit_id,
                        &source_text,
                        &active_direction,
                    );
                    if did_commit {
                        let tail = transcript.uncommitted_tail();
                        if !tail.is_empty() {
                            let _ = send_source(
                                &mut node,
                                &tail,
                                "streaming",
                                current_burst_id,
                                None,
                                Some(&active_direction),
                            );
                        }
                        if flush_requested && transcript.stable_buffer().trim().is_empty() {
                            flush_requested = false;
                        }
                    }
                } else {
                    let commit_id = next_commit_id;
                    match submit_translation_task(
                        &request_tx,
                        commit_id,
                        &source_text,
                        &active_direction,
                        &transcript,
                    ) {
                        Ok(()) => {
                            next_commit_id += 1;
                            translation_pending = true;
                            let _ = send_source(
                                &mut node,
                                &source_text,
                                "translating",
                                None,
                                Some(commit_id),
                                Some(&active_direction),
                            );
                        }
                        Err(e) => {
                            if let Some(h) = worker_handle.take() {
                                let _ = h.join();
                            }
                            return Err(anyhow!("Failed to submit translation task: {e}"));
                        }
                    }
                }
            }
        }

        if stopping && stop_drain_timed_out(stop_started_at, stop_drain_timeout_ms) {
            tracing::warn!(
                "Translator stop drain exceeded {}ms; exiting without waiting for worker",
                stop_drain_timeout_ms
            );
            should_join_worker = false;
            break;
        }

        if stopping
            && !translation_pending
            && transcript.stable_buffer().trim().is_empty()
            && transcript.active_burst_text().is_empty()
        {
            break;
        }

        if !did_work {
            thread::sleep(Duration::from_millis(EVENT_POLL_SLEEP_MS));
        }
    }

    drop(request_tx);
    if should_join_worker {
        if let Some(handle) = worker_handle {
            handle
                .join()
                .map_err(|_| anyhow!("Translation worker thread panicked"))?;
        }
    }

    tracing::info!("dora-qwen35-translator stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_session_meta, build_system_prompt, direction_from_parameters,
        find_commit_boundary_from_tail, format_commit_prompt_debug, seal_final_asr_chunk,
        should_trigger_idle_flush, stop_drain_timed_out, strip_hard_cut_terminal_punctuation,
        warmup_enabled_from_raw, DirectionMeta, TranscriptBuffer,
    };
    #[cfg(target_os = "windows")]
    use super::looks_like_reasoning;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    #[cfg(target_os = "windows")]
    #[test]
    fn looks_like_reasoning_catches_cot_leakage() {
        // The exact leakage observed in the en→zh e2e run (Qwen3-1.7B).
        assert!(looks_like_reasoning(
            "好的，我需要将用户提供的英文内容翻译成中文。首先，我要仔细阅读并理解原文的意思。"
        ));
        assert!(looks_like_reasoning(
            "Let me translate this sentence into Chinese."
        ));
        // Clean translations must pass.
        assert!(!looks_like_reasoning(
            "大家早上好，今天我想介绍我们当地的实时翻译系统。"
        ));
        assert!(!looks_like_reasoning(
            "Hello everyone, welcome to the local real-time translation system."
        ));
    }

    #[test]
    fn strip_hard_cut_terminal_punctuation_removes_single_sentence_mark() {
        assert_eq!(
            strip_hard_cut_terminal_punctuation("我们一八年开源的。"),
            "我们一八年开源的"
        );
        assert_eq!(
            strip_hard_cut_terminal_punctuation("我们一八年开源的！"),
            "我们一八年开源的"
        );
        assert_eq!(
            strip_hard_cut_terminal_punctuation("我们一八年开源的"),
            "我们一八年开源的"
        );
    }

    #[test]
    fn find_commit_boundary_from_tail_prefers_last_comma() {
        let text = "大家下午好，我叫鲍月，然后来自华为，现在也是CoolEdge";
        let end = find_commit_boundary_from_tail(text).expect("boundary should exist");
        assert_eq!(&text[..end], "大家下午好，我叫鲍月，然后来自华为，");
    }

    #[test]
    fn find_commit_boundary_from_tail_falls_back_to_sentence_end() {
        let text = "这是我们一八年开源的。然后继续";
        let end = find_commit_boundary_from_tail(text).expect("boundary should exist");
        assert_eq!(&text[..end], "这是我们一八年开源的。");
    }

    #[test]
    fn find_commit_boundary_from_tail_returns_none_without_supported_separator() {
        assert!(find_commit_boundary_from_tail("大家下午好我叫鲍月然后来自华为").is_none());
    }

    #[test]
    fn build_system_prompt_is_plain_translation_only() {
        let prompt = build_system_prompt("en");
        assert!(prompt.contains("Output only the translated text"));
        assert!(prompt.contains("English"));
    }

    #[test]
    fn build_session_meta_includes_commit_id_when_present() {
        let direction = DirectionMeta {
            source_language: "en".into(),
            target_language: "zh".into(),
            epoch: 3,
        };
        let meta = build_session_meta("complete", Some(42), Some(7), Some(&direction));
        assert_eq!(
            meta.get("session_status"),
            Some(&dora_node_api::Parameter::String("complete".into()))
        );
        assert_eq!(
            meta.get("question_id"),
            Some(&dora_node_api::Parameter::Integer(42))
        );
        assert_eq!(
            meta.get("commit_id"),
            Some(&dora_node_api::Parameter::Integer(7))
        );
        assert_eq!(
            meta.get("target_language"),
            Some(&dora_node_api::Parameter::String("zh".into()))
        );
        assert_eq!(
            meta.get("direction_epoch"),
            Some(&dora_node_api::Parameter::Integer(3))
        );
    }

    #[test]
    fn direction_metadata_overrides_startup_defaults_per_event() {
        let fallback = DirectionMeta {
            source_language: "zh".into(),
            target_language: "en".into(),
            epoch: 0,
        };
        let mut parameters = BTreeMap::new();
        parameters.insert(
            "source_language".to_string(),
            dora_node_api::Parameter::String("en".into()),
        );
        parameters.insert(
            "target_language".to_string(),
            dora_node_api::Parameter::String("zh".into()),
        );
        parameters.insert(
            "direction_epoch".to_string(),
            dora_node_api::Parameter::Integer(8),
        );
        assert_eq!(
            direction_from_parameters(&parameters, &fallback),
            DirectionMeta {
                source_language: "en".into(),
                target_language: "zh".into(),
                epoch: 8,
            }
        );
    }

    #[test]
    fn format_commit_prompt_debug_keeps_full_prompt_sections() {
        let debug = format_commit_prompt_debug(
            "Translate to English. Output translation only.",
            "Input:\n大家下午好，我叫鲍月，然后来自华为，",
        );
        assert!(debug.contains("system_prompt=\nTranslate to English. Output translation only."));
        assert!(debug.contains("user_prompt=\nInput:\n大家下午好，我叫鲍月，然后来自华为，"));
    }

    #[test]
    fn should_trigger_idle_flush_only_after_threshold_with_buffered_text() {
        assert!(!should_trigger_idle_flush(
            Some(Duration::from_millis(800)),
            true,
            false,
            false,
            1100
        ));
        assert!(should_trigger_idle_flush(
            Some(Duration::from_millis(1200)),
            true,
            false,
            false,
            1100
        ));
        assert!(!should_trigger_idle_flush(
            Some(Duration::from_millis(1200)),
            false,
            false,
            false,
            1100
        ));
        assert!(!should_trigger_idle_flush(
            Some(Duration::from_millis(1200)),
            true,
            true,
            false,
            1100
        ));
        assert!(!should_trigger_idle_flush(
            Some(Duration::from_millis(1200)),
            true,
            false,
            true,
            1100
        ));
    }

    #[test]
    fn stop_drain_timeout_only_triggers_after_deadline() {
        assert!(!stop_drain_timed_out(None, 1000));
        assert!(!stop_drain_timed_out(Some(Instant::now()), 1000));
        assert!(stop_drain_timed_out(
            Some(Instant::now() - Duration::from_millis(1001)),
            1000
        ));
    }

    #[test]
    fn final_asr_chunk_seals_immediately_for_scheduling() {
        let mut transcript = TranscriptBuffer::new();
        transcript.update_from_chunk(Some(1), "A complete sentence.");

        assert!(!seal_final_asr_chunk(&mut transcript, "progressive"));
        assert!(seal_final_asr_chunk(&mut transcript, "final"));
        assert!(!transcript.stable_buffer().is_empty());
    }

    #[test]
    fn translator_warmup_is_enabled_by_default_and_can_be_disabled() {
        assert!(warmup_enabled_from_raw(None));
        assert!(warmup_enabled_from_raw(Some("true")));
        assert!(!warmup_enabled_from_raw(Some("0")));
        assert!(!warmup_enabled_from_raw(Some(" OFF ")));
    }
}
