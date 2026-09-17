//! Windows backend: translation via a managed `llama-server.exe` child process.
//!
//! The node spawns the official llama.cpp prebuilt server with a Qwen GGUF
//! model and talks to its OpenAI-compatible HTTP API (SSE streaming). The
//! chat template is applied server-side from the GGUF metadata, so no local
//! tokenizer or jinja rendering is needed here.
//!
//! Configuration (env):
//!   LLAMA_SERVER_PATH           - llama-server.exe location
//!                                 (default: ~/.OminiX/llama.cpp/llama-server.exe)
//!   QWEN35_TRANSLATOR_MODEL_PATH - GGUF file to load (resolved in main.rs)
//!   LLAMA_NGL                   - GPU layers to offload (default 0 = CPU;
//!                                 set e.g. 99 with a Vulkan build to use the iGPU)
//!   LLAMA_CTX                   - context size (default 2048)

use crate::{TranslationResponse, TranslationTask, TranslationWorkerEvent};
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(400);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(180);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

fn default_llama_server_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".OminiX")
        .join("llama.cpp")
        .join("llama-server.exe")
}

fn resolve_llama_server_path() -> PathBuf {
    if let Ok(v) = std::env::var("LLAMA_SERVER_PATH") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    default_llama_server_path()
}

fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("Failed to bind ephemeral port")?;
    Ok(listener.local_addr()?.port())
}

struct LlamaServer {
    child: Child,
    base_url: String,
    client: reqwest::blocking::Client,
}

impl LlamaServer {
    fn spawn(model_path: &Path) -> Result<Self> {
        let server_path = resolve_llama_server_path();
        if !server_path.exists() {
            return Err(anyhow!(
                "llama-server.exe not found at {}. Run scripts\\init_windows_models.ps1 first, or set LLAMA_SERVER_PATH.",
                server_path.display()
            ));
        }
        if !model_path.exists() {
            return Err(anyhow!(
                "GGUF model not found at {}. Run scripts\\init_windows_models.ps1 first, or set QWEN35_TRANSLATOR_MODEL_PATH.",
                model_path.display()
            ));
        }

        let port = pick_free_port()?;
        let ngl = std::env::var("LLAMA_NGL").unwrap_or_else(|_| "0".to_string());
        let ctx = std::env::var("LLAMA_CTX").unwrap_or_else(|_| "2048".to_string());

        tracing::info!(
            "Starting llama-server: model={}, port={}, ngl={}, ctx={}",
            model_path.display(),
            port,
            ngl,
            ctx
        );

        let mut child = Command::new(&server_path)
            .args([
                "-m",
                &model_path.to_string_lossy(),
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "-c",
                &ctx,
                "-ngl",
                &ngl,
                "--jinja",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn {}: {e}", server_path.display()))?;

        // Drain stderr so a chatty llama-server never blocks on a full pipe;
        // surface it at debug level for troubleshooting.
        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    match line {
                        Ok(l) => tracing::debug!(target: "llama-server", "{}", l),
                        Err(_) => break,
                    }
                }
            });
        }

        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("Failed to build HTTP client")?;

        let mut server = Self {
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            client,
        };
        server.wait_ready()?;
        Ok(server)
    }

    fn wait_ready(&mut self) -> Result<()> {
        let started = Instant::now();
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .context("Failed to query llama-server process")?
            {
                return Err(anyhow!("llama-server exited early with {status}"));
            }
            match self.client.get(format!("{}/health", self.base_url)).send() {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(
                        "llama-server ready after {:.1}s",
                        started.elapsed().as_secs_f32()
                    );
                    return Ok(());
                }
                _ => {
                    if started.elapsed() >= HEALTH_TIMEOUT {
                        return Err(anyhow!(
                            "llama-server did not become healthy within {:?}",
                            HEALTH_TIMEOUT
                        ));
                    }
                    thread::sleep(HEALTH_POLL_INTERVAL);
                }
            }
        }
    }

    /// Streaming chat completion; `on_delta` receives the full text accumulated
    /// so far (mirrors the MLX backend's streaming contract).
    fn chat_completion_stream<F>(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: usize,
        mut on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": true,
            "cache_prompt": true,
            // Qwen3 template switch: never emit <think> blocks.
            "chat_template_kwargs": {"enable_thinking": false}
        });

        let resp = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&body)
            .send()
            .context("chat completion request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().unwrap_or_default();
            return Err(anyhow!("llama-server returned {status}: {detail}"));
        }

        let mut reader = BufReader::new(resp);
        let mut full = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .context("failed reading SSE stream")?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            let Some(data) = trimmed.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            if let Some(content) = value
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !content.is_empty() {
                    full.push_str(content);
                    on_delta(full.trim_start());
                }
            }
        }

        Ok(sanitize_translation_output(full.trim()))
    }
}

/// Normalize model output to just the translation:
/// - proper thinking mode (`<think>…</think>answer`) → keep the answer
/// - stray `</think>` with no opener (observed: the model emitted the
///   translation, then a spurious closing tag, then reasoning) → keep the part
///   before the tag
fn sanitize_translation_output(text: &str) -> String {
    let stripped = strip_think_block(text);
    if let Some(idx) = stripped.find("</think>") {
        return stripped[..idx].trim().to_string();
    }
    stripped
}

/// Defensive: remove a `<think>…</think>` block if the model still emitted one
/// (e.g. a template that ignores `enable_thinking`).
fn strip_think_block(text: &str) -> String {
    let Some(end) = text.find("</think>") else {
        return text.to_string();
    };
    if text.trim_start().starts_with("<think>") {
        text[end + "</think>".len()..].trim().to_string()
    } else {
        text.to_string()
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn translation_worker_loop(
    model_path: PathBuf,
    temperature: f32,
    max_tokens: usize,
    warmup_enabled: bool,
    ready_tx: mpsc::Sender<Result<(), String>>,
    request_rx: mpsc::Receiver<TranslationTask>,
    response_tx: mpsc::Sender<TranslationWorkerEvent>,
) {
    let server = match LlamaServer::spawn(&model_path) {
        Ok(server) => server,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };

    if warmup_enabled {
        let started_at = Instant::now();
        tracing::info!("Warming translation model before accepting live speech");
        let warmup_result = server.chat_completion_stream(
            "/no_think Translate the source text into English. Output only the translation.",
            "Source:\nHello.",
            0.0,
            2,
            |_| {},
        );
        match warmup_result {
            Ok(_) => tracing::info!(
                "Translation model warm-up completed in {:.2}s",
                started_at.elapsed().as_secs_f32()
            ),
            Err(error) => tracing::warn!("Translation model warm-up failed: {error}"),
        }
    }
    let _ = ready_tx.send(Ok(()));

    while let Ok(task) = request_rx.recv() {
        let streaming_tx = response_tx.clone();
        let streaming_source = task.source_text.clone();
        let commit_id = task.commit_id;
        let streaming_direction = task.direction.clone();
        let mut output = server
            .chat_completion_stream(
                &task.system_prompt,
                &task.user_prompt,
                temperature,
                max_tokens,
                move |translation| {
                    let _ = streaming_tx.send(TranslationWorkerEvent::Streaming {
                        commit_id,
                        source_text: streaming_source.clone(),
                        translation: translation.to_string(),
                        direction: streaming_direction.clone(),
                    });
                },
            )
            .map_err(|e| e.to_string());

        // Guard against rare chain-of-thought leakage from small non-thinking
        // models: retry once without streaming (the UI already saw the first
        // stream) and with a nudged temperature to leave the greedy path.
        if let Ok(text) = &output {
            if crate::looks_like_reasoning(text) {
                let preview: String = text.chars().take(80).collect();
                tracing::warn!("Translation output looks like reasoning, retrying once: {preview}");
                output = server
                    .chat_completion_stream(
                        &task.system_prompt,
                        &task.user_prompt,
                        (temperature + 0.3).min(1.0),
                        max_tokens,
                        |_| {},
                    )
                    .map(|text| sanitize_translation_output(&text))
                    .map_err(|e| e.to_string());
            }
        }

        if response_tx
            .send(TranslationWorkerEvent::Complete(TranslationResponse {
                commit_id: task.commit_id,
                source_text: task.source_text,
                output,
                direction: task.direction,
            }))
            .is_err()
        {
            break;
        }
    }
}
