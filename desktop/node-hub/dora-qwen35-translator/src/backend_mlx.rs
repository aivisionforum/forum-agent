//! macOS / Apple Silicon backend: Qwen3.5 translation via OminiX-MLX
//! (original implementation, unchanged logic — extracted from main.rs).

use crate::{TranslationResponse, TranslationTask, TranslationWorkerEvent};
use anyhow::{anyhow, Result};
use minijinja::{context, Environment};
use minijinja_contrib::pycompat::unknown_method_callback;
use mlx_lm_utils::tokenizer::{
    load_model_chat_template_from_file, ApplyChatTemplateArgs, Conversation, Tokenizer,
};
use mlx_rs::ops::indexing::{IndexOp, NewAxis};
use mlx_rs::transforms::eval;
use qwen3_5_35b_mlx::{load_model, Generate};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

pub(crate) fn load_eos_tokens(model_path: &std::path::Path) -> Result<HashSet<u32>> {
    let config_path = model_path.join("config.json");
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
    let eos_value = config.get("eos_token_id").or_else(|| {
        config
            .get("text_config")
            .and_then(|v| v.get("eos_token_id"))
    });

    let eos_tokens = match eos_value {
        Some(serde_json::Value::Array(ids)) => ids
            .iter()
            .filter_map(|v| v.as_u64().map(|n| n as u32))
            .collect(),
        Some(serde_json::Value::Number(n)) => {
            let mut set = HashSet::new();
            set.insert(n.as_u64().unwrap_or(248044) as u32);
            set
        }
        _ => {
            let mut set = HashSet::new();
            set.insert(248044);
            set
        }
    };

    Ok(eos_tokens)
}

fn supports_enable_thinking(chat_template: &str) -> bool {
    chat_template.contains("enable_thinking")
}

fn render_prompt_with_enable_thinking(
    chat_template: &str,
    system_prompt: &str,
    user_text: &str,
    enable_thinking: bool,
) -> Result<String> {
    let mut env = Environment::new();
    env.set_unknown_method_callback(unknown_method_callback);
    env.add_template_owned("chat".to_string(), chat_template.to_string())
        .map_err(|e| anyhow!("Failed to compile chat_template: {e}"))?;
    let template = env
        .get_template("chat")
        .map_err(|e| anyhow!("Failed to load compiled chat_template: {e}"))?;

    let messages = vec![
        Conversation {
            role: "system",
            content: system_prompt,
        },
        Conversation {
            role: "user",
            content: user_text,
        },
    ];

    let rendered = template
        .render(context! {
            messages => messages,
            add_generation_prompt => true,
            enable_thinking => enable_thinking,
        })
        .map_err(|e| anyhow!("Failed to render chat_template: {e}"))?;
    Ok(rendered)
}

fn build_prompt_token_ids(
    tokenizer: &mut Tokenizer,
    chat_template: &str,
    model_id: &str,
    system_prompt: &str,
    user_text: &str,
    force_disable_thinking: bool,
) -> Result<Vec<u32>> {
    // Qwen3 template supports a real switch: enable_thinking=false
    if force_disable_thinking && supports_enable_thinking(chat_template) {
        match render_prompt_with_enable_thinking(chat_template, system_prompt, user_text, false) {
            Ok(rendered) => {
                let encoding = tokenizer
                    .encode(rendered.as_str(), false)
                    .map_err(|e| anyhow!("Tokenization failed (manual template render): {e:?}"))?;
                return Ok(encoding.get_ids().to_vec());
            }
            Err(e) => {
                tracing::warn!(
                    "No-think template render failed, fallback to default template path: {e}"
                );
            }
        }
    }

    // Fallback: single-turn conversation.
    let conversations: Vec<Conversation<&str, &str>> = vec![
        Conversation {
            role: "system",
            content: system_prompt,
        },
        Conversation {
            role: "user",
            content: user_text,
        },
    ];
    let args = ApplyChatTemplateArgs {
        conversations: vec![conversations.into()],
        documents: None,
        model_id,
        chat_template_id: None,
        add_generation_prompt: Some(true),
        continue_final_message: None,
    };
    let encodings = tokenizer
        .apply_chat_template_and_encode(chat_template.to_string(), args)
        .map_err(|e| anyhow!("Tokenization failed: {e:?}"))?;

    let prompt_ids = encodings
        .iter()
        .flat_map(|enc| enc.get_ids().iter().copied())
        .collect::<Vec<u32>>();
    Ok(prompt_ids)
}

fn append_streaming_text<F>(full_translation: &mut String, decoded: &str, on_streaming: &mut F)
where
    F: FnMut(&str),
{
    if decoded.is_empty() {
        return;
    }
    full_translation.push_str(decoded);
    on_streaming(full_translation.trim_start());
}

fn generate_text_completion<F>(
    tokenizer: &mut Tokenizer,
    model: &mut qwen3_5_35b_mlx::Model,
    chat_template: &str,
    model_id: &str,
    system_prompt: &str,
    text_to_translate: &str,
    force_disable_thinking: bool,
    temperature: f32,
    max_tokens: usize,
    eos_tokens: &HashSet<u32>,
    mut on_streaming: F,
) -> Result<String>
where
    F: FnMut(&str),
{
    const MAX_TRANSLATION_SECS: f32 = 45.0;
    const STREAM_BATCH: usize = 5;

    let prompt_ids = build_prompt_token_ids(
        tokenizer,
        chat_template,
        model_id,
        system_prompt,
        text_to_translate,
        force_disable_thinking,
    )?;

    let prompt_len = prompt_ids.len();
    let prompt_tokens = mlx_rs::Array::from(&prompt_ids[..]).index(NewAxis);
    tracing::info!(
        "Translating {} chars ({} prompt tokens)…",
        text_to_translate.len(),
        prompt_len
    );
    let t_start = Instant::now();

    let generator = Generate::new(model, temperature, &prompt_tokens);

    let mut token_buf: Vec<mlx_rs::Array> = Vec::new();
    let mut full_translation = String::new();
    let mut generated = 0usize;

    let token_budget = max_tokens;
    for token_result in generator {
        if t_start.elapsed().as_secs_f32() >= MAX_TRANSLATION_SECS {
            tracing::warn!(
                "Generation timeout after {:.2}s, forcing finalize",
                t_start.elapsed().as_secs_f32()
            );
            break;
        }

        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                return Err(anyhow!("Generation error: {e}"));
            }
        };

        let token_id = token.item::<u32>();
        if eos_tokens.contains(&token_id) {
            break;
        }

        token_buf.push(token);
        generated += 1;

        if token_buf.len() >= STREAM_BATCH {
            if let Err(e) = eval(&token_buf) {
                tracing::warn!("eval failed: {e}");
            }
            let ids: Vec<u32> = token_buf.drain(..).map(|t| t.item::<u32>()).collect();
            if let Ok(text) = tokenizer.decode(&ids, true) {
                append_streaming_text(&mut full_translation, &text, &mut on_streaming);
            }
        }

        if generated >= token_budget {
            break;
        }
    }

    if !token_buf.is_empty() {
        let _ = eval(&token_buf);
        let ids: Vec<u32> = token_buf.drain(..).map(|t| t.item::<u32>()).collect();
        if let Ok(text) = tokenizer.decode(&ids, true) {
            append_streaming_text(&mut full_translation, &text, &mut on_streaming);
        }
    }

    let elapsed = t_start.elapsed().as_secs_f32();
    tracing::info!(
        "Translation done in {:.2}s ({} tokens)\n{}",
        elapsed,
        generated,
        full_translation
    );

    // Release Metal buffer pool accumulated during KV-cache inference.
    // Equivalent to Python's mx.metal.clear_cache(); prevents memory pressure
    // from building up across successive translations.
    unsafe {
        mlx_sys::mlx_clear_cache();
    }

    Ok(full_translation.trim().to_string())
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
    let init = || -> Result<(
        Tokenizer,
        qwen3_5_35b_mlx::Model,
        String,
        String,
        bool,
        HashSet<u32>,
    )> {
        let tokenizer_file = model_path.join("tokenizer.json");
        let tokenizer_config_file = model_path.join("tokenizer_config.json");

        let tokenizer = Tokenizer::from_file(&tokenizer_file)
            .map_err(|e| anyhow!("Failed to load tokenizer: {e:?}"))?;

        let chat_template = match load_model_chat_template_from_file(&tokenizer_config_file)? {
            Some(t) => t,
            None => {
                let jinja_path = model_path.join("chat_template.jinja");
                std::fs::read_to_string(&jinja_path).map_err(|_| {
                    anyhow!("Chat template not found in tokenizer_config.json or chat_template.jinja")
                })?
            }
        };

        let model =
            load_model(&model_path).map_err(|e| anyhow!("Failed to load Qwen3.5 model: {e}"))?;
        let eos_tokens = load_eos_tokens(&model_path)?;
        let model_id = model_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("qwen3.5")
            .to_string();
        let force_disable_thinking = model_id.to_lowercase().contains("qwen3");

        Ok((
            tokenizer,
            model,
            chat_template,
            model_id,
            force_disable_thinking,
            eos_tokens,
        ))
    };

    let (mut tokenizer, mut model, chat_template, model_id, force_disable_thinking, eos_tokens) =
        match init() {
            Ok(state) => state,
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return;
            }
        };

    if warmup_enabled {
        let started_at = Instant::now();
        tracing::info!("Warming translation model before accepting live speech");
        let warmup_result = generate_text_completion(
            &mut tokenizer,
            &mut model,
            &chat_template,
            &model_id,
            "/no_think Translate the source text into English. Output only the translation.",
            "Source:\nHello.",
            force_disable_thinking,
            0.0,
            2,
            &eos_tokens,
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
        let output = generate_text_completion(
            &mut tokenizer,
            &mut model,
            &chat_template,
            &model_id,
            &task.system_prompt,
            &task.user_prompt,
            force_disable_thinking,
            temperature,
            max_tokens,
            &eos_tokens,
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

#[cfg(test)]
mod tests {
    use super::append_streaming_text;

    #[test]
    fn streaming_text_callback_receives_cumulative_snapshots() {
        let mut full = String::new();
        let mut snapshots = Vec::new();
        let mut collect = |text: &str| snapshots.push(text.to_string());

        append_streaming_text(&mut full, "  一个", &mut collect);
        append_streaming_text(&mut full, "完整的句子。", &mut collect);
        append_streaming_text(&mut full, "", &mut collect);

        assert_eq!(full, "  一个完整的句子。");
        assert_eq!(snapshots, vec!["一个", "一个完整的句子。"]);
    }
}
