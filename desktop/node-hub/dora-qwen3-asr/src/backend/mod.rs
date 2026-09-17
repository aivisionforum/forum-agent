//! Platform-specific ASR inference backend.
//!
//! The Dora event loop in `main.rs` is platform-independent; only model
//! loading, resampling and transcription differ per platform.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(any(target_os = "macos", windows)))]
compile_error!("dora-qwen3-asr currently supports only macOS (MLX) and Windows (sherpa-onnx)");
