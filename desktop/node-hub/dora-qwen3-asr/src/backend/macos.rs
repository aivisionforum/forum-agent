//! macOS / Apple Silicon backend: Qwen3-ASR running on MLX (original implementation).

use anyhow::Result;
use qwen3_asr_mlx::{audio, default_model_path, Qwen3ASR, SamplingConfig};
use std::path::{Path, PathBuf};

const ASR_MAX_TOKENS: usize = 1024;

pub struct AsrEngine {
    model: Qwen3ASR,
}

impl AsrEngine {
    pub fn load(model_dir: &Path) -> Result<Self> {
        Ok(Self {
            model: Qwen3ASR::load(model_dir)?,
        })
    }

    pub fn transcribe(&mut self, samples_16k: &[f32], language: &str) -> Result<String> {
        let config = SamplingConfig {
            temperature: 0.0,
            max_tokens: ASR_MAX_TOKENS,
        };
        Ok(self.model
            .transcribe_samples_with_config(samples_16k, language, &config)?)
    }
}

pub fn default_model_dir() -> PathBuf {
    default_model_path()
}

pub fn resample(samples: &[f32], from_hz: u32, to_hz: u32) -> Result<Vec<f32>> {
    Ok(audio::resample(samples, from_hz, to_hz)?)
}

pub fn clear_cache() {
    unsafe {
        mlx_sys::mlx_clear_cache();
    }
}
