//! Windows backend: SenseVoice-small (int8) via sherpa-onnx prebuilt libraries.
//!
//! SenseVoice covers zh/en/ja/ko/yue with per-utterance auto detection, which
//! matches this app's primary language pairs. Other source languages are a
//! known v1 limitation on Windows (the macOS build uses Qwen3-ASR instead).

use anyhow::{anyhow, Result};
use sherpa_onnx::{
    LinearResampler, OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
};
use std::path::{Path, PathBuf};

pub struct AsrEngine {
    recognizer: OfflineRecognizer,
}

impl AsrEngine {
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model = model_dir.join("model.int8.onnx");
        let tokens = model_dir.join("tokens.txt");
        if !model.exists() || !tokens.exists() {
            return Err(anyhow!(
                "SenseVoice model files not found in {}. Expected model.int8.onnx and tokens.txt. Run scripts\\init_windows_models.ps1 first.",
                model_dir.display()
            ));
        }

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(model.to_string_lossy().to_string()),
            language: Some("auto".to_string()),
            use_itn: true,
        };
        config.model_config.tokens = Some(tokens.to_string_lossy().to_string());
        config.model_config.num_threads = 4;
        config.model_config.provider = Some("cpu".to_string());

        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            anyhow!(
                "Failed to create SenseVoice recognizer from {}",
                model_dir.display()
            )
        })?;
        Ok(Self { recognizer })
    }

    pub fn transcribe(&mut self, samples_16k: &[f32], _language: &str) -> Result<String> {
        let stream = self.create_stream();
        stream.accept_waveform(16000, samples_16k);
        self.recognizer.decode(&stream);
        let result = stream
            .get_result()
            .ok_or_else(|| anyhow!("SenseVoice decode returned no result"))?;
        Ok(strip_rich_tags(&result.text))
    }

    // `create_stream` goes through a small helper so the borrow of
    // `self.recognizer` ends before `decode` is called.
    fn create_stream(&self) -> sherpa_onnx::OfflineStream {
        self.recognizer.create_stream()
    }
}

/// SenseVoice can emit rich-caption tags (`<|zh|><|HAPPY|><|Speech|>…`).
/// Remove any `<|…|>` segments so only spoken text reaches the translator.
fn strip_rich_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        match rest[start..].find("|>") {
            Some(end) => rest = &rest[start + end + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

pub fn default_model_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".OminiX")
        .join("models")
        .join("sensevoice-small-int8")
}

pub fn resample(samples: &[f32], from_hz: u32, to_hz: u32) -> Result<Vec<f32>> {
    let resampler = LinearResampler::create(from_hz as i32, to_hz as i32)
        .ok_or_else(|| anyhow!("Failed to create resampler {from_hz}Hz -> {to_hz}Hz"))?;
    Ok(resampler.resample(samples, true))
}

/// Present for parity with the MLX backend; sherpa-onnx manages its own memory.
pub fn clear_cache() {}
