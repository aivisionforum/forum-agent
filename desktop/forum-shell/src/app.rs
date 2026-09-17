use crate::{
    apple_speech::{self, AppleSpeech},
    dataflow::{render_translation_dataflow, RenderOptions},
    models::ModelPaths,
    power_activity::WakeLock,
    preferences::{self, AppPreferences},
    runtime::{RuntimeEvent, TranslationRuntime},
    usage::{self, UsageSnapshot, UsageTracker},
    Args,
};
use cpal::traits::{DeviceTrait, HostTrait};
use moxin_dora_bridge::{data::SentenceUnit, AudioSource, TranslationDirection, TranslationUpdate};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::Duration,
};
use tauri::{
    Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

const OVERLAY_MIN_INNER_WIDTH: f64 = 560.0;
const OVERLAY_MIN_INNER_HEIGHT: f64 = 96.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSettings {
    app_language: String,
    accent_theme: String,
    source_language: String,
    target_language: String,
    input_device: String,
    subtitle_split: bool,
    translation_only: bool,
    overlay_opacity: f64,
    font_size_preset: String,
    anchor_position_preset: String,
    final_interval_seconds: u64,
    keep_awake_during_translation: bool,
    spoken_translation_enabled: bool,
    spoken_translation_output_device: Option<String>,
    spoken_translation_voice: Option<String>,
    auto_save_transcript: bool,
    periodic_save_transcript: bool,
    transcript_file_name: String,
    transcript_save_dir: Option<String>,
}

impl From<&AppPreferences> for TranslationSettings {
    fn from(preferences: &AppPreferences) -> Self {
        Self {
            app_language: preferences.app_language.clone(),
            accent_theme: preferences.accent_theme.clone(),
            source_language: preferences.translation_source_language.clone(),
            target_language: preferences.translation_target_language.clone(),
            input_device: preferences.translation_input_device.clone(),
            subtitle_split: preferences.translation_subtitle_split,
            translation_only: preferences.translation_only,
            overlay_opacity: preferences.translation_overlay_opacity,
            font_size_preset: preferences.translation_font_size_preset.clone(),
            anchor_position_preset: preferences.translation_anchor_position_preset.clone(),
            final_interval_seconds: preferences.translation_final_interval_seconds,
            keep_awake_during_translation: preferences.translation_keep_awake,
            spoken_translation_enabled: preferences.experimental_spoken_translation_enabled,
            spoken_translation_output_device: preferences
                .experimental_spoken_translation_output_device
                .clone(),
            spoken_translation_voice: preferences.experimental_spoken_translation_voice.clone(),
            auto_save_transcript: preferences.translation_auto_save_transcript,
            periodic_save_transcript: preferences.translation_periodic_save_transcript,
            transcript_file_name: preferences.translation_transcript_file_name.clone(),
            transcript_save_dir: preferences.translation_transcript_save_dir.clone(),
        }
    }
}

impl TranslationSettings {
    fn apply_to(&self, preferences: &mut AppPreferences) {
        preferences.app_language = self.app_language.clone();
        preferences.accent_theme = match self.accent_theme.as_str() {
            "neon-orange" | "neon-pink" | "neon-green" => self.accent_theme.clone(),
            _ => "neon-blue".into(),
        };
        preferences.translation_source_language = self.source_language.clone();
        preferences.translation_target_language = self.target_language.clone();
        preferences.translation_input_device = self.input_device.clone();
        preferences.translation_subtitle_split =
            self.subtitle_split || self.target_language == "none";
        preferences.translation_only = self.translation_only && self.target_language != "none";
        preferences.translation_overlay_opacity = self.overlay_opacity.clamp(0.35, 1.0);
        preferences.translation_font_size_preset = self.font_size_preset.clone();
        preferences.translation_anchor_position_preset = self.anchor_position_preset.clone();
        preferences.translation_final_interval_seconds =
            preferences::sanitize_final_interval_seconds(self.final_interval_seconds);
        preferences.translation_keep_awake = self.keep_awake_during_translation;
        preferences.experimental_spoken_translation_enabled = self.spoken_translation_enabled;
        preferences.experimental_spoken_translation_output_device =
            self.spoken_translation_output_device.clone();
        preferences.experimental_spoken_translation_voice = self.spoken_translation_voice.clone();
        preferences.translation_auto_save_transcript = self.auto_save_transcript;
        preferences.translation_periodic_save_transcript = self.periodic_save_transcript;
        preferences.translation_transcript_file_name = self.transcript_file_name.clone();
        preferences.translation_transcript_save_dir = self.transcript_save_dir.clone();
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPayload {
    settings: TranslationSettings,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    installed_apple_voices: Vec<apple_speech::SystemVoice>,
    subtitle_preview_visible: bool,
    running: bool,
    runtime_status: String,
    runtime_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeState {
    running: bool,
    status: String,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectionSwitchState {
    source_language: String,
    target_language: String,
    active_source_language: String,
    active_target_language: String,
    epoch: u64,
    active_epoch: u64,
    pending: bool,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            running: false,
            status: "idle".into(),
            message: "Local AI is ready".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Sentence {
    source_text: String,
    translation: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranslatingSentence {
    commit_id: i64,
    source_text: String,
    translation: String,
    complete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OverlayState {
    active: bool,
    status: String,
    source_language: String,
    target_language: String,
    subtitle_split: bool,
    translation_only: bool,
    font_size: u32,
    anchor_position: u32,
    accent_theme: String,
    history: Vec<Sentence>,
    translating: Option<TranslatingSentence>,
    pending_source_text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelStatus {
    core_ready: bool,
    downloading: bool,
    component: Option<String>,
    progress: f64,
    title: String,
    detail: String,
    core_download_bytes: u64,
}

struct AppState {
    preferences: Mutex<AppPreferences>,
    runtime: TranslationRuntime,
    runtime_state: Mutex<RuntimeState>,
    resource_dir: Option<PathBuf>,
    voice_preview_process: Mutex<Option<Child>>,
    apple_speech: AppleSpeech,
    wake_lock: WakeLock,
    spoken_completed_count: Mutex<u64>,
    model_download_process: Mutex<Option<(String, Child)>>,
    subtitle_preview_visible: Mutex<bool>,
    usage: Arc<UsageTracker>,
}

impl AppState {
    fn new(resource_dir: Option<PathBuf>) -> Self {
        let preferences = preferences::load();
        let settings = TranslationSettings::from(&preferences);
        let runtime = TranslationRuntime::new();
        let usage = UsageTracker::load(preferences::preferences_dir().join("usage.json"));
        let state = Self {
            preferences: Mutex::new(preferences),
            runtime,
            runtime_state: Mutex::new(RuntimeState::default()),
            resource_dir,
            voice_preview_process: Mutex::new(None),
            apple_speech: AppleSpeech::new(),
            wake_lock: WakeLock::new(),
            spoken_completed_count: Mutex::new(0),
            model_download_process: Mutex::new(None),
            subtitle_preview_visible: Mutex::new(true),
            usage,
        };
        state.sync_shared_state();
        state.show_subtitle_preview(&settings);
        state
    }

    fn sync_shared_state(&self) {
        let preferences = self.preferences.lock().clone();
        let shared = self.runtime.shared_state();
        shared.translation_window_visible.set(true);
        shared
            .translation_locale_en
            .set(preferences.app_language == "en");
        if !self.runtime_state.lock().running {
            let epoch = shared.translation_direction_request.read().epoch;
            let direction = TranslationDirection::new(
                preferences.translation_source_language.clone(),
                preferences.translation_target_language.clone(),
                epoch,
            );
            shared.translation_direction_request.set(direction.clone());
            shared.translation_direction_active.set(direction.clone());
            shared
                .translation_lang_pair
                .set((direction.source_language, direction.target_language));
        }
        shared
            .translation_subtitle_split
            .set(preferences.translation_subtitle_split);
        shared
            .translation_overlay_opacity
            .set(preferences.translation_overlay_opacity);
        shared
            .translation_font_size_preset
            .set(preferences.translation_font_size_preset.clone());
        shared.translation_footer_font_size_preset.set("12".into());
        shared
            .translation_anchor_position_preset
            .set(preferences.translation_anchor_position_preset.clone());
        if preferences.translation_input_device == "__system_audio__" {
            shared
                .translation_audio_source
                .set(AudioSource::SystemAudio);
            shared.translation_input_device.set(None);
        } else {
            shared.translation_audio_source.set(AudioSource::Microphone);
            let device = (preferences.translation_input_device != "__default_microphone__")
                .then(|| preferences.translation_input_device.clone());
            shared.translation_input_device.set(device);
        }
    }

    fn direction_switch_state(&self) -> DirectionSwitchState {
        let shared = self.runtime.shared_state();
        let requested = shared.translation_direction_request.read();
        let active = shared.translation_direction_active.read();
        DirectionSwitchState {
            source_language: requested.source_language,
            target_language: requested.target_language,
            active_source_language: active.source_language,
            active_target_language: active.target_language,
            epoch: requested.epoch,
            active_epoch: active.epoch,
            pending: requested.epoch != active.epoch,
        }
    }

    fn sample_text(language: &str, index: usize) -> &'static str {
        match (language, index) {
            ("zh", 0) => "这是一段用于调整字幕大小和布局的测试内容。",
            ("zh", _) => "请确认每句话都清晰、易读，并适合现场屏幕。",
            ("ja", 0) => "これは字幕のサイズとレイアウトを調整するためのテストです。",
            ("ja", _) => "各文が読みやすく、会場の画面に適しているか確認してください。",
            ("fr", 0) => {
                "Ceci est un texte de test pour régler la taille et la disposition des sous-titres."
            }
            ("fr", _) => {
                "Vérifiez que chaque phrase est claire et lisible sur l’écran de la salle."
            }
            (_, 0) => "This sample helps you adjust subtitle size and layout.",
            (_, _) => {
                "Check that every sentence is clear, readable, and suitable for the venue screen."
            }
        }
    }

    fn show_subtitle_preview(&self, settings: &TranslationSettings) {
        let history = (0..2)
            .map(|index| SentenceUnit {
                source_text: Self::sample_text(&settings.source_language, index).to_string(),
                translation: if settings.target_language == "none" {
                    String::new()
                } else {
                    Self::sample_text(&settings.target_language, index).to_string()
                },
                source_language: settings.source_language.clone(),
                target_language: settings.target_language.clone(),
                direction_epoch: 0,
            })
            .collect();
        self.runtime
            .shared_state()
            .translation
            .set(Some(TranslationUpdate {
                history,
                pending_source_text: String::new(),
                completed_count: 2,
            }));
        self.runtime.shared_state().translation_stream.set(None);
        *self.subtitle_preview_visible.lock() = true;
    }

    fn clear_subtitle_preview(&self) {
        self.runtime.shared_state().translation.set(None);
        self.runtime.shared_state().translation_stream.set(None);
        *self.subtitle_preview_visible.lock() = false;
    }

    fn overlay_state(&self) -> OverlayState {
        let shared = self.runtime.shared_state();
        let active = shared.translation_overlay_active.read();
        let bridge_status = shared.status.read();
        let bridges_ready = bridge_status
            .active_bridges
            .iter()
            .any(|bridge| bridge == "moxin-mic-input")
            && bridge_status
                .active_bridges
                .iter()
                .any(|bridge| bridge == "moxin-translation-listener");
        let status = if !active {
            "idle"
        } else if bridges_ready {
            "listening"
        } else {
            "warming"
        }
        .to_string();
        if shared.translation_overlay_status.read() != status {
            shared.translation_overlay_status.set(status.clone());
        }

        let (source_language, target_language) = shared.translation_lang_pair.read();
        let update = shared.translation.read();
        let (history, pending_source_text) = update
            .map(|update| {
                (
                    update
                        .history
                        .into_iter()
                        .map(|sentence| Sentence {
                            source_text: sentence.source_text,
                            translation: sentence.translation,
                        })
                        .collect(),
                    update.pending_source_text,
                )
            })
            .unwrap_or_default();

        let preferences = self.preferences.lock();
        let accent_theme = preferences.accent_theme.clone();
        let translation_only = preferences.translation_only;
        drop(preferences);
        let translating = shared
            .translation_stream
            .read()
            .map(|stream| TranslatingSentence {
                commit_id: stream.commit_id,
                source_text: stream.source_text,
                translation: stream.translation,
                complete: stream.complete,
            });
        OverlayState {
            active,
            status,
            source_language,
            target_language,
            subtitle_split: shared.translation_subtitle_split.read(),
            translation_only,
            font_size: shared
                .translation_font_size_preset
                .read()
                .parse()
                .unwrap_or(24),
            anchor_position: shared
                .translation_anchor_position_preset
                .read()
                .parse()
                .unwrap_or(50),
            accent_theme,
            history,
            translating,
            pending_source_text,
        }
    }

    fn take_overlay_dirty(&self) -> bool {
        let shared = self.runtime.shared_state();
        let mut dirty = false;
        dirty |= shared.status.take_dirty();
        dirty |= shared.translation.take_dirty();
        dirty |= shared.translation_stream.take_dirty();
        dirty |= shared.translation_lang_pair.take_dirty();
        dirty |= shared.translation_subtitle_split.take_dirty();
        dirty |= shared.translation_font_size_preset.take_dirty();
        dirty |= shared.translation_anchor_position_preset.take_dirty();
        dirty |= shared.translation_overlay_active.take_dirty();
        dirty
    }

    fn poll_runtime_events(&self) -> RuntimeState {
        let events = self.runtime.poll_events();
        let mut state = self.runtime_state.lock();
        for event in events {
            match event {
                RuntimeEvent::Started(id) => {
                    state.running = true;
                    state.status = "listening".into();
                    state.message = format!("Local translation connected · {id}");
                }
                RuntimeEvent::Stopped => {
                    self.apple_speech.stop();
                    self.wake_lock.stop();
                    if state.running {
                        if let Err(error) = self.usage.stop() {
                            log::error!("Could not stop usage timer: {error}");
                        }
                    }
                    state.running = false;
                    state.status = "idle".into();
                    state.message = "Translation stopped".into();
                    let shared = self.runtime.shared_state();
                    let requested = shared.translation_direction_request.read();
                    shared.translation_direction_active.set(requested.clone());
                    shared
                        .translation_lang_pair
                        .set((requested.source_language, requested.target_language));
                }
                RuntimeEvent::Error(message) => {
                    self.apple_speech.stop();
                    self.wake_lock.stop();
                    if state.running {
                        if let Err(error) = self.usage.stop() {
                            log::error!("Could not stop usage timer after runtime error: {error}");
                        }
                    }
                    state.running = false;
                    state.status = "error".into();
                    state.message = message;
                    let shared = self.runtime.shared_state();
                    let requested = shared.translation_direction_request.read();
                    shared.translation_direction_active.set(requested.clone());
                    shared
                        .translation_lang_pair
                        .set((requested.source_language, requested.target_language));
                }
            }
        }
        state.clone()
    }

    fn queue_completed_translations_for_speech(&self) {
        if !self.runtime_state.lock().running {
            return;
        }
        let Some(update) = self.runtime.shared_state().translation.read() else {
            return;
        };
        let mut spoken = self.spoken_completed_count.lock();
        if update.completed_count < *spoken {
            *spoken = 0;
        }
        let new_sentences = update.completed_count.saturating_sub(*spoken) as usize;
        for sentence in update
            .history
            .iter()
            .skip(update.history.len().saturating_sub(new_sentences))
        {
            self.apple_speech.speak(sentence.translation.clone());
        }
        *spoken = update.completed_count;
    }

    fn save_transcript_if_needed(&self) -> Result<(), String> {
        let preferences = self.preferences.lock().clone();
        if !preferences.translation_auto_save_transcript
            && !preferences.translation_periodic_save_transcript
        {
            return Ok(());
        }
        let Some(update) = self.runtime.shared_state().translation.read() else {
            return Ok(());
        };
        if update.history.is_empty() {
            return Ok(());
        }

        let directory = preferences::transcript_dir(&preferences);
        fs::create_dir_all(&directory)
            .map_err(|error| format!("Could not create transcript directory: {error}"))?;
        let filename = if preferences
            .translation_transcript_file_name
            .trim()
            .is_empty()
        {
            "transcript.md"
        } else {
            preferences.translation_transcript_file_name.trim()
        };
        let mut markdown = String::from("# Translation transcript\n\n");
        let mut last_direction_epoch = None;
        for sentence in update.history {
            if last_direction_epoch != Some(sentence.direction_epoch) {
                markdown.push_str(&format!(
                    "## {} → {}\n\n",
                    sentence.source_language, sentence.target_language
                ));
                last_direction_epoch = Some(sentence.direction_epoch);
            }
            markdown.push_str(&format!(
                "**Source**\n\n{}\n\n**Translation**\n\n{}\n\n---\n\n",
                sentence.source_text, sentence.translation
            ));
        }
        fs::write(directory.join(filename), markdown)
            .map_err(|error| format!("Could not save transcript: {error}"))
    }

    fn model_status(&self) -> ModelStatus {
        let mut process = self.model_download_process.lock();
        let mut active_component = None;
        if let Some((component, child)) = process.as_mut() {
            match child.try_wait() {
                Ok(None) => active_component = Some(component.clone()),
                Ok(Some(_)) | Err(_) => *process = None,
            }
        }
        let component = active_component.clone();
        let (progress, title, detail) = component
            .as_deref()
            .and_then(read_model_download_state)
            .unwrap_or_else(|| {
                if core_models_ready() {
                    (
                        1.0,
                        "Ready".into(),
                        "Core translation models are installed".into(),
                    )
                } else {
                    (
                        0.0,
                        "Models required".into(),
                        "Download models to start translating".into(),
                    )
                }
            });
        ModelStatus {
            core_ready: core_models_ready(),
            downloading: active_component.is_some(),
            component,
            progress,
            title,
            detail,
            core_download_bytes: core_download_bytes(),
        }
    }
}

/// Approximate total download size shown in the UI while models are fetched.
fn core_download_bytes() -> u64 {
    // macOS (and any other non-Windows target): original MLX model bundle.
    #[cfg(not(windows))]
    {
        4_222_472_192
    }
    #[cfg(windows)]
    {
        // SenseVoice int8 (~230MB) + Qwen3-1.7B GGUF (~1.1GB) + llama.cpp + dora
        1_500_000_000
    }
}

fn core_models_ready() -> bool {
    ModelPaths::resolve_current().is_ok_and(|models| models.ready())
}

fn model_state_path(component: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Logs")
        .join(crate::identity::DATA_DIRECTORY_NAME)
        .join(format!("{component}_model_state.txt"))
}

fn read_model_download_state(component: &str) -> Option<(f64, String, String)> {
    let content = fs::read_to_string(model_state_path(component)).ok()?;
    let mut fields = content.trim().split('|');
    let _step = fields.next()?;
    let title = fields.next()?.to_string();
    let detail = fields.next()?.to_string();
    let progress = fields.next()?.parse::<f64>().ok()?.clamp(0.0, 1.0);
    Some((progress, title, detail))
}

// Bundled MLX downloader is only used by the macOS bootstrap path.
#[cfg(not(windows))]
fn resolve_model_downloader() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join("hen-local-init"));
        }
    }
    for variable in ["FORUM_AGENT_DORA_TARGET_DIR", "CARGO_TARGET_DIR"] {
        if let Some(directory) = std::env::var_os(variable) {
            let directory = PathBuf::from(directory);
            candidates.push(directory.join("debug/hen-local-init"));
            candidates.push(directory.join("release/hen-local-init"));
        }
    }
    candidates.extend([
        PathBuf::from("target/debug/hen-local-init"),
        PathBuf::from("target/release/hen-local-init"),
    ]);
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Locate `init_windows_models.ps1`: bundled next to the executable in
/// installed layouts, or in the source tree when running via cargo
/// (`<root>/target/<profile>/<exe>` → `<root>/scripts/`). The compile-time
/// manifest dir is only a last-resort fallback, since it points at the build
/// machine's source path in release binaries.
#[cfg(windows)]
fn resolve_windows_bootstrap_script() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join("init_windows_models.ps1"));
            if let Some(root) = directory.parent().and_then(|p| p.parent()) {
                candidates.push(root.join("scripts").join("init_windows_models.ps1"));
            }
        }
    }
    candidates
        .push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/init_windows_models.ps1"));
    candidates.into_iter().find(|candidate| candidate.is_file())
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> SettingsPayload {
    let preferences = state.preferences.lock().clone();
    let runtime_state = state.poll_runtime_events();
    let output_devices = apple_speech::output_device_names();
    let mut settings = TranslationSettings::from(&preferences);
    if settings
        .spoken_translation_output_device
        .as_ref()
        .is_some_and(|selected| !output_devices.contains(selected))
    {
        settings.spoken_translation_output_device = None;
    }
    SettingsPayload {
        settings,
        input_devices: input_devices(),
        output_devices,
        installed_apple_voices: apple_speech::available_voices(),
        subtitle_preview_visible: *state.subtitle_preview_visible.lock(),
        running: runtime_state.running,
        runtime_status: runtime_state.status,
        runtime_message: runtime_state.message,
    }
}

#[tauri::command]
fn get_model_status(state: State<'_, AppState>) -> ModelStatus {
    state.model_status()
}

#[tauri::command]
fn start_model_download(
    state: State<'_, AppState>,
    component: String,
) -> Result<ModelStatus, String> {
    let component = match component.as_str() {
        "core" => "core",
        _ => return Err("Unknown model component".into()),
    };
    if component == "core" && core_models_ready() {
        return Ok(state.model_status());
    }
    ModelPaths::validate_download_overrides()?;
    let mut running = state.model_download_process.lock();
    if let Some((_, child)) = running.as_mut() {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Err("A model download is already running".into());
        }
        *running = None;
    }

    let state_path = model_state_path(component);
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let log_path = state_path.with_extension("log");
    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)
        .map_err(|error| format!("Could not create model download log: {error}"))?;

    // Windows: run the PowerShell bootstrapper (SenseVoice + Qwen3 GGUF +
    // llama.cpp runtime + dora CLI) instead of the bundled MLX downloader.
    #[cfg(windows)]
    let child = {
        let script = resolve_windows_bootstrap_script().ok_or_else(|| {
            "Windows model bootstrap script (init_windows_models.ps1) not found next to the app or in the source tree.".to_string()
        })?;
        let _ = fs::write(
            &state_path,
            "download|Downloading models|Running init_windows_models.ps1|0.5",
        );
        Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .env("FORUM_AGENT_BOOTSTRAP_STATE_PATH", &state_path)
            .stdout(Stdio::from(log.try_clone().map_err(|error| {
                format!("Could not open model log: {error}")
            })?))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| format!("Could not start model download: {error}"))?
    };

    #[cfg(not(windows))]
    let child = {
        let downloader = resolve_model_downloader().ok_or_else(|| {
            "The bundled model downloader is missing. Reinstall the app.".to_string()
        })?;
        let _ = fs::remove_file(&state_path);
        // Inference overrides and legacy shared caches are read-only inputs.
        // Only the two product-owned destinations may be passed to the installer.
        let download_models = ModelPaths::owned_current()?;
        Command::new(downloader)
            .envs(download_models.env_vars()?)
            .env("FORUM_AGENT_MODEL_COMPONENT", component)
            .env("FORUM_AGENT_BOOTSTRAP_STATE_PATH", &state_path)
            .stdout(Stdio::from(log.try_clone().map_err(|error| {
                format!("Could not open model log: {error}")
            })?))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| format!("Could not start model download: {error}"))?
    };

    *running = Some((component.to_string(), child));
    drop(running);
    Ok(state.model_status())
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: TranslationSettings,
) -> Result<(), String> {
    if settings.spoken_translation_enabled {
        apple_speech::ensure_voice_available(
            &settings.target_language,
            settings
                .spoken_translation_voice
                .as_deref()
                .unwrap_or("apple-voice-1"),
        )?;
    }
    let running = state.runtime_state.lock().running;
    let (speech_settings_changed, keep_awake_changed) = {
        let mut preferences = state.preferences.lock();
        if running
            && (preferences.translation_source_language != settings.source_language
                || preferences.translation_target_language != settings.target_language
                || preferences.translation_input_device != settings.input_device
                || preferences.translation_final_interval_seconds
                    != preferences::sanitize_final_interval_seconds(
                        settings.final_interval_seconds,
                    ))
        {
            return Err(
                "Stop live translation before changing its languages, audio input, or caption interval"
                    .into(),
            );
        }
        let speech_changed = preferences.experimental_spoken_translation_enabled
            != settings.spoken_translation_enabled
            || preferences.experimental_spoken_translation_voice
                != settings.spoken_translation_voice
            || preferences.experimental_spoken_translation_output_device
                != settings.spoken_translation_output_device;
        let keep_awake_changed =
            preferences.translation_keep_awake != settings.keep_awake_during_translation;
        settings.apply_to(&mut preferences);
        preferences::save(&preferences)?;
        (speech_changed, keep_awake_changed)
    };
    state.sync_shared_state();
    if keep_awake_changed && running {
        if settings.keep_awake_during_translation {
            state.wake_lock.start()?;
        } else {
            state.wake_lock.stop();
        }
    }
    if speech_settings_changed && state.runtime_state.lock().running {
        *state.spoken_completed_count.lock() = state
            .runtime
            .shared_state()
            .translation
            .read()
            .map(|update| update.completed_count)
            .unwrap_or(0);
        state.apple_speech.configure(
            settings.spoken_translation_enabled,
            &settings.target_language,
            settings
                .spoken_translation_voice
                .as_deref()
                .unwrap_or("apple-voice-1"),
            settings.spoken_translation_output_device.as_deref(),
        );
    }
    apply_native_identity(&app, &settings)?;
    apply_overlay_window(&app, &settings)?;
    if *state.subtitle_preview_visible.lock() {
        state.show_subtitle_preview(&settings);
    }
    app.emit_to("overlay", "overlay-state", state.overlay_state())
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn swap_translation_direction(
    state: State<'_, AppState>,
    settings: TranslationSettings,
) -> Result<DirectionSwitchState, String> {
    if !state.runtime_state.lock().running {
        return Err("Live translation is not running".into());
    }
    if settings.target_language == "none" {
        return Err("Choose a target language before swapping live translation".into());
    }

    if settings.spoken_translation_enabled {
        apple_speech::ensure_voice_available(
            &settings.target_language,
            settings
                .spoken_translation_voice
                .as_deref()
                .unwrap_or("apple-voice-1"),
        )?;
    }

    {
        let mut preferences = state.preferences.lock();
        if settings.source_language != preferences.translation_target_language
            || settings.target_language != preferences.translation_source_language
        {
            return Err("Live direction changes must swap the current source and target".into());
        }
        preferences.translation_source_language = settings.source_language.clone();
        preferences.translation_target_language = settings.target_language.clone();
        preferences.experimental_spoken_translation_enabled = settings.spoken_translation_enabled;
        preferences.experimental_spoken_translation_voice =
            settings.spoken_translation_voice.clone();
        preferences.experimental_spoken_translation_output_device =
            settings.spoken_translation_output_device.clone();
        preferences::save(&preferences)?;
    }

    let shared = state.runtime.shared_state();
    let requested = shared.translation_direction_request.read();
    let active = shared.translation_direction_active.read();
    let next_epoch = requested.epoch.max(active.epoch).saturating_add(1);
    shared
        .translation_direction_request
        .set(TranslationDirection::new(
            settings.source_language,
            settings.target_language,
            next_epoch,
        ));

    // Pause spoken output while the old epoch drains. The new target voice is
    // configured after the requested direction reaches a speech boundary.
    state.apple_speech.stop();
    *state.spoken_completed_count.lock() = state
        .runtime
        .shared_state()
        .translation
        .read()
        .map(|update| update.completed_count)
        .unwrap_or(0);

    Ok(state.direction_switch_state())
}

#[tauri::command]
fn get_direction_switch_state(state: State<'_, AppState>) -> DirectionSwitchState {
    state.direction_switch_state()
}

#[tauri::command]
fn start_translation(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: TranslationSettings,
) -> Result<RuntimeState, String> {
    stop_preview_process(&state);
    if settings.spoken_translation_enabled {
        apple_speech::ensure_voice_available(
            &settings.target_language,
            settings
                .spoken_translation_voice
                .as_deref()
                .unwrap_or("apple-voice-1"),
        )?;
    }
    let models = ModelPaths::resolve_current()?;
    if !models.ready() {
        return Err("The selected core models are incomplete. Check explicit model paths or download the Forum models before starting live translation".into());
    }
    let model_env = models.env_vars()?;
    {
        let mut preferences = state.preferences.lock();
        settings.apply_to(&mut preferences);
        preferences::save(&preferences)?;
    }
    state.sync_shared_state();

    let dataflow = render_translation_dataflow(
        state.resource_dir.as_deref(),
        RenderOptions {
            source_language: &settings.source_language,
            target_language: &settings.target_language,
            asr_model_path: &models.asr,
            translator_model_path: &models.translator,
            system_audio: settings.input_device == "__system_audio__",
            max_segment_ms: preferences::sanitize_final_interval_seconds(
                settings.final_interval_seconds,
            ) * 1000,
        },
    )?;

    state.clear_subtitle_preview();
    let shared = state.runtime.shared_state();
    shared.translation.set(None);
    shared.translation_stream.set(None);
    shared.translation_window_visible.set(true);
    shared.translation_overlay_active.set(true);
    shared.translation_overlay_status.set("warming".into());
    *state.spoken_completed_count.lock() = 0;
    state.apple_speech.configure(
        settings.spoken_translation_enabled,
        &settings.target_language,
        settings
            .spoken_translation_voice
            .as_deref()
            .unwrap_or("apple-voice-1"),
        settings.spoken_translation_output_device.as_deref(),
    );
    if settings.keep_awake_during_translation {
        state.wake_lock.start()?;
    } else {
        state.wake_lock.stop();
    }
    if let Err(error) = state.runtime.start(dataflow, model_env) {
        state.wake_lock.stop();
        return Err(error);
    }
    if let Err(error) = state.usage.start() {
        let _ = state.runtime.stop();
        state.wake_lock.stop();
        return Err(error);
    }
    if let Err(error) = apply_overlay_window(&app, &settings) {
        let _ = state.usage.stop();
        let _ = state.runtime.stop();
        state.wake_lock.stop();
        return Err(error);
    }
    if let Some(window) = app.get_webview_window("overlay") {
        if let Err(error) = window.show() {
            let _ = state.usage.stop();
            let _ = state.runtime.stop();
            state.wake_lock.stop();
            return Err(error.to_string());
        }
    }

    let next = RuntimeState {
        running: true,
        status: "warming".into(),
        message: "Starting local translation…".into(),
    };
    *state.runtime_state.lock() = next.clone();
    Ok(next)
}

#[tauri::command]
fn stop_translation(state: State<'_, AppState>) -> Result<RuntimeState, String> {
    state.apple_speech.stop();
    state.wake_lock.stop();
    *state.spoken_completed_count.lock() = 0;
    state.save_transcript_if_needed()?;
    state.runtime.stop()?;
    state.usage.stop()?;
    let shared = state.runtime.shared_state();
    shared.translation.set(None);
    shared.translation_stream.set(None);
    shared.translation_window_visible.set(true);
    shared.translation_overlay_active.set(false);
    shared.translation_overlay_status.set("idle".into());
    let requested_direction = shared.translation_direction_request.read();
    shared
        .translation_direction_active
        .set(requested_direction.clone());
    shared.translation_lang_pair.set((
        requested_direction.source_language,
        requested_direction.target_language,
    ));
    *state.subtitle_preview_visible.lock() = false;

    let next = RuntimeState {
        running: false,
        status: "idle".into(),
        message: "Translation stopped".into(),
    };
    *state.runtime_state.lock() = next.clone();
    Ok(next)
}

#[tauri::command]
fn get_usage(state: State<'_, AppState>) -> UsageSnapshot {
    state.usage.snapshot()
}

#[tauri::command]
fn get_overlay_state(state: State<'_, AppState>) -> OverlayState {
    state.overlay_state()
}

#[tauri::command]
fn open_transcript_history(state: State<'_, AppState>) -> Result<(), String> {
    let directory = preferences::transcript_dir(&state.preferences.lock());
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create transcript directory: {error}"))?;
    open_directory(&directory)
}

#[tauri::command]
fn toggle_subtitle_preview(state: State<'_, AppState>) -> Result<bool, String> {
    if state.runtime_state.lock().running {
        return Err("Stop live translation before changing the test subtitles".into());
    }

    let next = !*state.subtitle_preview_visible.lock();
    if next {
        let preferences = state.preferences.lock();
        let settings = TranslationSettings::from(&*preferences);
        drop(preferences);
        state.show_subtitle_preview(&settings);
    } else {
        state.clear_subtitle_preview();
    }
    Ok(next)
}

fn stop_preview_process(state: &AppState) {
    if let Some(mut child) = state.voice_preview_process.lock().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[tauri::command]
fn preview_spoken_voice(
    state: State<'_, AppState>,
    voice: String,
    language: String,
) -> Result<(), String> {
    stop_preview_process(&state);
    let sample = match language.as_str() {
        "zh" => "欢迎使用AI Vision Forum，这是苹果系统音色试听。",
        "ja" => "AI Vision Forum リアルタイム翻訳のシステム音声プレビューです。",
        "fr" => "Bienvenue dans AI Vision Forum, voici un aperçu de la voix système Apple.",
        _ => "Welcome to AI Vision Forum. This is an Apple system voice preview.",
    };
    let output_device = state
        .preferences
        .lock()
        .experimental_spoken_translation_output_device
        .clone();
    let child = apple_speech::preview(&voice, &language, output_device.as_deref(), sample)?;

    *state.voice_preview_process.lock() = Some(child);
    Ok(())
}

#[tauri::command]
fn stop_spoken_voice_preview(state: State<'_, AppState>) {
    stop_preview_process(&state);
}

#[tauri::command]
fn list_apple_voices() -> Vec<apple_speech::SystemVoice> {
    apple_speech::available_voices()
}

#[tauri::command]
fn open_apple_voice_settings() -> Result<(), String> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.Accessibility-Settings.extension?LiveSpeech")
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open Apple voice settings: {error}"))
}

#[tauri::command]
fn list_output_devices() -> Vec<String> {
    apple_speech::output_device_names()
}

#[tauri::command]
fn preview_apple_voice(
    state: State<'_, AppState>,
    name: String,
    locale: String,
) -> Result<(), String> {
    stop_preview_process(&state);
    let child = apple_speech::preview_named(&name, &locale)?;
    *state.voice_preview_process.lock() = Some(child);
    Ok(())
}

fn input_devices() -> Vec<String> {
    let mut devices = vec!["__system_audio__".into(), "__default_microphone__".into()];
    if let Ok(discovered) = cpal::default_host().input_devices() {
        devices.extend(discovered.filter_map(|device| device.name().ok()));
    }
    devices.sort_by(|left, right| {
        let rank = |value: &str| match value {
            "__system_audio__" => 0,
            "__default_microphone__" => 1,
            _ => 2,
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
    devices.dedup();
    devices
}

fn create_overlay(app: &tauri::App) -> tauri::Result<WebviewWindow> {
    let overlay = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
        .title(crate::identity::PRODUCT_NAME)
        .inner_size(960.0, 640.0)
        .min_inner_size(OVERLAY_MIN_INNER_WIDTH, OVERLAY_MIN_INNER_HEIGHT)
        .resizable(true)
        .decorations(false)
        .always_on_top(true)
        .visible(true)
        .center()
        .build()?;

    let app_handle = app.handle().clone();
    overlay.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let state = app_handle.state::<AppState>();
            state
                .runtime
                .shared_state()
                .translation_window_visible
                .set(true);
        }
    });
    Ok(overlay)
}

fn show_or_create_main_window(app: &tauri::AppHandle) -> Result<(), String> {
    let window = match app.get_webview_window("main") {
        Some(window) => window,
        None => {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|config| config.label == "main")
                .ok_or_else(|| "Main window configuration is missing".to_string())?;
            WebviewWindowBuilder::from_config(app, config)
                .map_err(|error| error.to_string())?
                .build()
                .map_err(|error| error.to_string())?
        }
    };

    window.show().map_err(|error| error.to_string())?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

fn apply_native_identity(
    app: &tauri::AppHandle,
    settings: &TranslationSettings,
) -> Result<(), String> {
    let name = crate::identity::PRODUCT_NAME;
    if let Some(window) = app.get_webview_window("main") {
        window.set_title(name).map_err(|error| error.to_string())?;
    }
    if let Some(window) = app.get_webview_window("overlay") {
        window.set_title(name).map_err(|error| error.to_string())?;
    }
    apply_macos_dock_icon(app, &settings.accent_theme)
}

#[cfg(target_os = "macos")]
fn apply_macos_dock_icon(app: &tauri::AppHandle, accent_theme: &str) -> Result<(), String> {
    let icon: &'static [u8] = match accent_theme {
        "neon-orange" => include_bytes!("../icons/icon-neon-orange.png"),
        "neon-pink" => include_bytes!("../icons/icon-neon-pink.png"),
        "neon-green" => include_bytes!("../icons/icon-neon-green.png"),
        _ => include_bytes!("../icons/icon-neon-blue.png"),
    };

    app.run_on_main_thread(move || {
        use objc2::{AllocAnyThread, MainThreadMarker};
        use objc2_app_kit::{NSApplication, NSImage};
        use objc2_foundation::NSData;

        let marker = unsafe { MainThreadMarker::new_unchecked() };
        let application = NSApplication::sharedApplication(marker);
        let data = NSData::with_bytes(icon);
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
            unsafe { application.setApplicationIconImage(Some(&image)) };
        }
    })
    .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "macos"))]
fn apply_macos_dock_icon(_app: &tauri::AppHandle, _accent_theme: &str) -> Result<(), String> {
    Ok(())
}

fn apply_overlay_window(
    app: &tauri::AppHandle,
    settings: &TranslationSettings,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window("overlay") else {
        return Ok(());
    };
    set_window_opacity(&window, settings.overlay_opacity)
}

#[cfg(target_os = "macos")]
fn set_window_opacity(window: &WebviewWindow, opacity: f64) -> Result<(), String> {
    use objc2::{msg_send, runtime::AnyObject};
    let ns_window = window.ns_window().map_err(|error| error.to_string())? as *mut AnyObject;
    unsafe {
        let _: () = msg_send![ns_window, setAlphaValue: opacity.clamp(0.35, 1.0)];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn set_window_opacity(_window: &WebviewWindow, _opacity: f64) -> Result<(), String> {
    Ok(())
}

fn open_directory(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");

    command
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open transcript history: {error}"))
}

fn start_event_bridge(app_handle: tauri::AppHandle) {
    thread::spawn(move || {
        let mut last_runtime_state: Option<RuntimeState> = None;
        let mut last_direction_state: Option<DirectionSwitchState> = None;
        loop {
            let has_main_window = app_handle.get_webview_window("main").is_some();
            let has_overlay_window = app_handle.get_webview_window("overlay").is_some();
            if !has_main_window && !has_overlay_window {
                break;
            }
            let state = app_handle.state::<AppState>();
            let runtime_state = state.poll_runtime_events();
            state.queue_completed_translations_for_speech();

            if last_runtime_state.as_ref() != Some(&runtime_state) {
                if has_main_window {
                    let _ = app_handle.emit_to("main", "runtime-state", &runtime_state);
                }
                last_runtime_state = Some(runtime_state.clone());
            }

            let direction_state = state.direction_switch_state();
            if last_direction_state.as_ref() != Some(&direction_state) {
                if runtime_state.running && !direction_state.pending {
                    let preferences = state.preferences.lock().clone();
                    *state.spoken_completed_count.lock() = state
                        .runtime
                        .shared_state()
                        .translation
                        .read()
                        .map(|update| update.completed_count)
                        .unwrap_or(0);
                    state.apple_speech.configure(
                        preferences.experimental_spoken_translation_enabled,
                        &direction_state.active_target_language,
                        preferences
                            .experimental_spoken_translation_voice
                            .as_deref()
                            .unwrap_or("apple-voice-1"),
                        preferences
                            .experimental_spoken_translation_output_device
                            .as_deref(),
                    );
                }
                if has_main_window {
                    let _ = app_handle.emit_to("main", "direction-switch-state", &direction_state);
                }
                last_direction_state = Some(direction_state);
            }

            if state.take_overlay_dirty() {
                let overlay_state = state.overlay_state();
                if has_overlay_window {
                    let _ = app_handle.emit_to("overlay", "overlay-state", &overlay_state);
                }
            }
            thread::sleep(Duration::from_millis(80));
        }
    });
}

pub fn run(args: Args) {
    let cli_dataflow = args.dataflow.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Err(error) = show_or_create_main_window(app) {
                log::error!("Could not restore the main window: {error}");
            }
        }))
        .setup(move |app| {
            let resource_dir = app.path().resource_dir().ok();
            app.manage(AppState::new(resource_dir));
            {
                let state = app.state::<AppState>();
                usage::start_checkpoint_loop(state.usage.clone());
            }
            create_overlay(app)?;
            let initial_settings = {
                let state = app.state::<AppState>();
                let preferences = state.preferences.lock();
                TranslationSettings::from(&*preferences)
            };
            apply_native_identity(app.handle(), &initial_settings).map_err(anyhow::Error::msg)?;
            apply_overlay_window(app.handle(), &initial_settings).map_err(anyhow::Error::msg)?;

            if let Some(dataflow) = cli_dataflow.clone() {
                let state = app.state::<AppState>();
                if initial_settings.keep_awake_during_translation {
                    state.wake_lock.start().map_err(anyhow::Error::msg)?;
                }
                state
                    .runtime
                    .start(
                        dataflow.into(),
                        ModelPaths::resolve_current()
                            .and_then(|models| models.env_vars())
                            .map_err(anyhow::Error::msg)?,
                    )
                    .map_err(|error| anyhow::anyhow!(error))?;
                state.usage.start().map_err(anyhow::Error::msg)?;
            }
            start_event_bridge(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_model_status,
            start_model_download,
            update_settings,
            swap_translation_direction,
            get_direction_switch_state,
            start_translation,
            stop_translation,
            get_usage,
            get_overlay_state,
            open_transcript_history,
            toggle_subtitle_preview,
            preview_spoken_voice,
            stop_spoken_voice_preview,
            list_apple_voices,
            open_apple_voice_settings,
            list_output_devices,
            preview_apple_voice
        ])
        .build(tauri::generate_context!())
        .expect("failed to build AI Vision Forum")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Err(error) = show_or_create_main_window(app) {
                    log::error!("Could not restore the main window from the Dock: {error}");
                }
            }
            if let tauri::RunEvent::Ready = event {
                let settings = {
                    let state = app.state::<AppState>();
                    let preferences = state.preferences.lock();
                    TranslationSettings::from(&*preferences)
                };
                if let Err(error) = apply_native_identity(app, &settings) {
                    log::error!("Could not apply native application identity: {error}");
                }
            }
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app.state::<AppState>();
                state.wake_lock.stop();
                if state.usage.snapshot().running {
                    if let Err(error) = state.usage.stop() {
                        log::error!("Could not save usage before exit: {error}");
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_preserves_translation_preferences() {
        let original = AppPreferences {
            experimental_spoken_translation_output_device: Some("Studio Display Speakers".into()),
            translation_only: true,
            translation_final_interval_seconds: 6,
            ..AppPreferences::default()
        };
        let settings = TranslationSettings::from(&original);
        let mut updated = AppPreferences::default();
        settings.apply_to(&mut updated);
        assert_eq!(
            original.translation_source_language,
            updated.translation_source_language
        );
        assert_eq!(
            original.translation_target_language,
            updated.translation_target_language
        );
        assert_eq!(
            original.translation_input_device,
            updated.translation_input_device
        );
        assert_eq!(original.accent_theme, updated.accent_theme);
        assert_eq!(original.translation_only, updated.translation_only);
        assert_eq!(
            original.translation_final_interval_seconds,
            updated.translation_final_interval_seconds
        );
        assert_eq!(
            original.translation_keep_awake,
            updated.translation_keep_awake
        );
        assert_eq!(
            original.experimental_spoken_translation_output_device,
            updated.experimental_spoken_translation_output_device
        );
    }

    #[test]
    fn translation_only_is_disabled_without_a_target_language() {
        let preferences = AppPreferences::default();
        let mut settings = TranslationSettings::from(&preferences);
        settings.target_language = "none".into();
        settings.translation_only = true;

        let mut updated = AppPreferences::default();
        settings.apply_to(&mut updated);

        assert!(!updated.translation_only);
        assert!(updated.translation_subtitle_split);
    }

    #[test]
    fn overlay_window_allows_a_constrained_vertical_viewport() {
        assert_eq!(OVERLAY_MIN_INNER_WIDTH, 560.0);
        assert_eq!(OVERLAY_MIN_INNER_HEIGHT, 96.0);
    }
}
