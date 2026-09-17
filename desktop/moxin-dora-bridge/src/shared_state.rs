//! Thread-safe state shared by the active translation, microphone, and audio bridges.

use crate::data::{AudioData, StreamingTranslation, TranslationUpdate};
use crate::widgets::AudioSource;
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct DirtyValue<T> {
    data: RwLock<T>,
    dirty: AtomicBool,
}

impl<T: Clone + Default> DirtyValue<T> {
    pub fn new(initial: T) -> Self {
        Self {
            data: RwLock::new(initial),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn set(&self, value: T) {
        *self.data.write() = value;
        self.dirty.store(true, Ordering::Release);
    }

    pub fn read_if_dirty(&self) -> Option<T> {
        self.take_dirty().then(|| self.data.read().clone())
    }

    pub fn read(&self) -> T {
        self.data.read().clone()
    }

    /// Consume the dirty flag without cloning the stored value.
    ///
    /// This is useful for consumers that only need to decide whether to build
    /// a larger snapshot containing this value.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }
}

impl<T: Default> Default for DirtyValue<T> {
    fn default() -> Self {
        Self {
            data: RwLock::new(T::default()),
            dirty: AtomicBool::new(false),
        }
    }
}

pub struct AudioState {
    chunks: RwLock<VecDeque<AudioData>>,
    max_chunks: usize,
    should_clear: AtomicBool,
    force_mute_flag: RwLock<Option<Arc<AtomicBool>>>,
}

impl AudioState {
    pub fn new(max_chunks: usize) -> Self {
        Self {
            chunks: RwLock::new(VecDeque::new()),
            max_chunks,
            should_clear: AtomicBool::new(false),
            force_mute_flag: RwLock::new(None),
        }
    }

    pub fn register_force_mute(&self, flag: Arc<AtomicBool>) {
        *self.force_mute_flag.write() = Some(flag);
    }

    pub fn push(&self, chunk: AudioData) {
        let mut chunks = self.chunks.write();
        chunks.push_back(chunk);
        while chunks.len() > self.max_chunks {
            chunks.pop_front();
        }
    }

    pub fn drain(&self) -> Vec<AudioData> {
        self.chunks.write().drain(..).collect()
    }

    pub fn clear(&self) {
        self.chunks.write().clear();
    }

    pub fn signal_clear(&self) {
        if let Some(flag) = self.force_mute_flag.read().as_ref() {
            flag.store(true, Ordering::Release);
        }
        self.should_clear.store(true, Ordering::Release);
        self.clear();
    }

    pub fn take_clear_signal(&self) -> bool {
        self.should_clear.swap(false, Ordering::AcqRel)
    }
}

#[derive(Debug, Clone, Default)]
pub struct DoraStatus {
    pub active_bridges: Vec<String>,
    pub last_error: Option<String>,
}

/// A language direction applied to one or more complete speech bursts.
///
/// `epoch` is monotonic. It lets downstream nodes distinguish late output from
/// the previous direction even when a rapid sequence of swaps returns to the
/// same language pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranslationDirection {
    pub source_language: String,
    pub target_language: String,
    pub epoch: u64,
}

impl TranslationDirection {
    pub fn new(
        source_language: impl Into<String>,
        target_language: impl Into<String>,
        epoch: u64,
    ) -> Self {
        Self {
            source_language: source_language.into(),
            target_language: target_language.into(),
            epoch,
        }
    }
}

pub struct MicState {
    level: DirtyValue<f32>,
    is_speaking: DirtyValue<bool>,
    is_recording: DirtyValue<bool>,
    aec_enabled: DirtyValue<bool>,
}

impl MicState {
    pub fn new() -> Self {
        Self {
            level: DirtyValue::new(0.0),
            is_speaking: DirtyValue::new(false),
            is_recording: DirtyValue::new(false),
            aec_enabled: DirtyValue::new(true),
        }
    }

    pub fn set_level(&self, level: f32) {
        self.level.set(level);
    }

    pub fn set_speaking(&self, speaking: bool) {
        self.is_speaking.set(speaking);
    }

    pub fn set_recording(&self, recording: bool) {
        self.is_recording.set(recording);
    }

    pub fn set_aec_enabled(&self, enabled: bool) {
        self.aec_enabled.set(enabled);
    }

    pub fn clear(&self) {
        self.level.set(0.0);
        self.is_speaking.set(false);
        self.is_recording.set(false);
        self.aec_enabled.set(true);
    }
}

impl Default for MicState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SharedDoraState {
    /// One dispatcher owns this endpoint. Never inferred from a default port.
    pub dynamic_node_context: RwLock<Option<crate::owned_runtime::DynamicNodeContext>>,
    pub audio: AudioState,
    pub status: DirtyValue<DoraStatus>,
    pub mic: MicState,
    pub translation: DirtyValue<Option<TranslationUpdate>>,
    pub translation_stream: DirtyValue<Option<StreamingTranslation>>,
    pub translation_window_visible: DirtyValue<bool>,
    pub translation_input_device: DirtyValue<Option<String>>,
    pub translation_overlay_opacity: DirtyValue<f64>,
    pub translation_locale_en: DirtyValue<bool>,
    /// Latest direction requested by the desktop UI.
    pub translation_direction_request: DirtyValue<TranslationDirection>,
    /// Direction currently assigned to newly starting speech bursts.
    pub translation_direction_active: DirtyValue<TranslationDirection>,
    pub translation_lang_pair: DirtyValue<(String, String)>,
    pub translation_font_size_preset: DirtyValue<String>,
    pub translation_footer_font_size_preset: DirtyValue<String>,
    pub translation_anchor_position_preset: DirtyValue<String>,
    pub translation_subtitle_split: DirtyValue<bool>,
    pub translation_audio_source: DirtyValue<AudioSource>,
    pub translation_overlay_status: DirtyValue<String>,
    pub translation_overlay_active: DirtyValue<bool>,
}

impl SharedDoraState {
    fn fresh() -> Self {
        Self {
            dynamic_node_context: RwLock::new(None),
            audio: AudioState::new(100),
            status: DirtyValue::default(),
            mic: MicState::new(),
            translation: DirtyValue::default(),
            translation_stream: DirtyValue::default(),
            translation_window_visible: DirtyValue::new(false),
            translation_input_device: DirtyValue::new(None),
            translation_overlay_opacity: DirtyValue::new(1.0),
            translation_locale_en: DirtyValue::new(false),
            translation_direction_request: DirtyValue::new(TranslationDirection::new(
                "zh", "en", 0,
            )),
            translation_direction_active: DirtyValue::new(TranslationDirection::new("zh", "en", 0)),
            translation_lang_pair: DirtyValue::new(("zh".to_string(), "en".to_string())),
            translation_font_size_preset: DirtyValue::new("24".to_string()),
            translation_footer_font_size_preset: DirtyValue::new("20".to_string()),
            translation_anchor_position_preset: DirtyValue::new("50".to_string()),
            translation_subtitle_split: DirtyValue::new(true),
            translation_audio_source: DirtyValue::new(AudioSource::SystemAudio),
            translation_overlay_status: DirtyValue::new("warming".to_string()),
            translation_overlay_active: DirtyValue::new(false),
        }
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self::fresh())
    }

    pub fn add_bridge(&self, bridge_id: String) {
        let mut status = self.status.read();
        if !status.active_bridges.contains(&bridge_id) {
            status.active_bridges.push(bridge_id);
            self.status.set(status);
        }
    }

    pub fn remove_bridge(&self, bridge_id: &str) {
        let mut status = self.status.read();
        status
            .active_bridges
            .retain(|candidate| candidate != bridge_id);
        self.status.set(status);
    }

    pub fn set_error(&self, error: Option<String>) {
        let mut status = self.status.read();
        status.last_error = error;
        self.status.set(status);
    }
}

impl Default for SharedDoraState {
    fn default() -> Self {
        Self::fresh()
    }
}

#[cfg(test)]
mod tests {
    use super::DirtyValue;

    #[test]
    fn different_dispatchers_do_not_share_session_or_endpoint_state() {
        let first = super::SharedDoraState::new();
        let second = super::SharedDoraState::new();
        first.translation_overlay_active.set(true);
        first.add_bridge("first-only".into());
        assert!(!std::sync::Arc::ptr_eq(&first, &second));
        assert!(!second.translation_overlay_active.read());
        assert!(second.status.read().active_bridges.is_empty());
    }

    #[test]
    fn take_dirty_consumes_the_flag_without_changing_the_value() {
        let value = DirtyValue::new(String::from("before"));
        assert!(!value.take_dirty());

        value.set(String::from("after"));
        assert!(value.take_dirty());
        assert!(!value.take_dirty());
        assert_eq!(value.read(), "after");
    }
}
