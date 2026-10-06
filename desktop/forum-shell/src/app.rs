use crate::{
    dataflow::{render_translation_dataflow, RenderOptions},
    meeting::MeetingOptions,
    models::{translation_runtime_env, AutomaticAsrPaths, ModelPaths},
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
include!("analysis_commands.rs");
include!("forum_commands.rs");
include!("part4_commands.rs");
const OVERLAY_MIN_INNER_HEIGHT: f64 = 560.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSettings {
    app_language: String,
    accent_theme: String,
    source_language: String,
    target_language: String,
    input_device: String,
    #[serde(default = "preferences::default_input_gain")]
    input_gain: f64,
    #[serde(default)]
    subtitle_side_by_side: bool,
    subtitle_split: bool,
    translation_only: bool,
    overlay_opacity: f64,
    font_size_preset: String,
    anchor_position_preset: String,
    final_interval_seconds: u64,
    keep_awake_during_translation: bool,
    #[serde(default = "recording_default")]
    recording_enabled: bool,
    #[serde(default)]
    speakers_enabled: bool,
    auto_save_transcript: bool,
    periodic_save_transcript: bool,
    transcript_file_name: String,
    transcript_save_dir: Option<String>,
}

fn recording_default() -> bool {
    true
}

impl From<&AppPreferences> for TranslationSettings {
    fn from(preferences: &AppPreferences) -> Self {
        Self {
            app_language: preferences.app_language.clone(),
            accent_theme: preferences.accent_theme.clone(),
            source_language: preferences.translation_source_language.clone(),
            target_language: preferences.translation_target_language.clone(),
            input_device: preferences.translation_input_device.clone(),
            input_gain: preferences.translation_input_gain,
            subtitle_side_by_side: preferences.translation_subtitle_side_by_side,
            subtitle_split: preferences.translation_subtitle_split,
            translation_only: preferences.translation_only,
            overlay_opacity: preferences.translation_overlay_opacity,
            font_size_preset: preferences.translation_font_size_preset.clone(),
            anchor_position_preset: preferences.translation_anchor_position_preset.clone(),
            final_interval_seconds: preferences.translation_final_interval_seconds,
            keep_awake_during_translation: preferences.translation_keep_awake,
            recording_enabled: preferences.translation_recording_enabled,
            speakers_enabled: preferences.meeting_speakers_enabled,
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
        preferences.translation_input_gain = preferences::sanitize_input_gain(self.input_gain);
        preferences.translation_subtitle_side_by_side = self.subtitle_side_by_side && self.target_language != "none";
        preferences.translation_subtitle_split = self.target_language == "none" || (self.subtitle_split && !preferences.translation_subtitle_side_by_side);
        preferences.translation_only = self.translation_only && self.target_language != "none";
        preferences.translation_overlay_opacity = self.overlay_opacity.clamp(0.35, 1.0);
        preferences.translation_font_size_preset = self.font_size_preset.clone();
        preferences.translation_anchor_position_preset = self.anchor_position_preset.clone();
        preferences.translation_final_interval_seconds =
            preferences::sanitize_final_interval_seconds(self.final_interval_seconds);
        preferences.translation_keep_awake = self.keep_awake_during_translation;
        preferences.translation_recording_enabled = self.recording_enabled;
        preferences.meeting_speakers_enabled = self.speakers_enabled && self.recording_enabled;
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
    source_language: String,
    language_texts: std::collections::HashMap<String, String>,
    segment_id: Option<forum_contracts::Uuid>,
    source_revision: Option<u32>,
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
    app_language: String,
    session_id: Option<forum_contracts::Uuid>,
    runtime_message: String,
    active: bool,
    status: String,
    source_language: String,
    target_language: String,
    subtitle_side_by_side: bool,
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
    asr_ready: bool,
    translation_ready: bool,
    automatic_asr_ready: bool,
    automatic_asr_detail: String,
    translation_runtime_ready: bool,
    translation_runtime_detail: String,
    downloading: bool,
    component: Option<String>,
    progress: f64,
    title: String,
    detail: String,
    core_download_bytes: u64,
}

struct AppState {
    audio_inputs: crate::audio_inputs::AudioInputs,
    activity_gate:Mutex<()>,
    exiting: std::sync::atomic::AtomicBool,
    exit_ready: std::sync::atomic::AtomicBool,
    preferences: Mutex<AppPreferences>,
    runtime: TranslationRuntime,
    analysis: Result<crate::analysis::AnalysisManager, String>,
    forum_lan: Result<crate::lan_manager::LanManager,String>,
    speakers:Result<crate::speaker_manager::SpeakerManager,String>,
    display: Result<forum_gateway::DisplayGateway, String>,
    runtime_state: Mutex<RuntimeState>,
    resource_dir: Option<PathBuf>,
    wake_lock: WakeLock,
    model_download_process: Mutex<Option<(String, Child)>>,
    subtitle_preview_visible: Mutex<bool>,
    usage: Arc<UsageTracker>,
}

impl AppState {
    fn new(resource_dir: Option<PathBuf>) -> Self {
        let preferences = preferences::load();
        let settings = TranslationSettings::from(&preferences);
        let runtime = TranslationRuntime::new(preferences::preferences_dir());
        let analysis = runtime.repository().map(|repo| crate::analysis::AnalysisManager::new(
            repo.core.clone(), preferences::preferences_dir(), resource_dir.clone(), runtime.resource_budget()));
        if let Ok(manager) = &analysis {
            let client=manager.client();
            runtime.set_lifecycle_handler(Arc::new(move |event| match event {
                RuntimeEvent::Started(id)=>{if let Ok(id)=forum_contracts::Uuid::parse_str(&id){client.session_started(id);}},
                RuntimeEvent::Stopped{session_id,..}=>{if let Ok(id)=forum_contracts::Uuid::parse_str(&session_id){client.session_stopped(id);}},
                _=>{}
            }));
        }
        let display = runtime.repository().and_then(|repo| {
            let core=repo.core.clone();
            forum_gateway::DisplayGateway::start(Arc::new(move |session| core.call(move |s| Ok(serde_json::to_value(s.public_snapshot(session)?)?)).map_err(|e|e.to_string())), display_assets()).map_err(|e|e.to_string())
        });
        let forum_lan=runtime.repository().and_then(|repo|crate::lan_manager::LanManager::new(repo.core.clone(),preferences::preferences_dir(),repo.identity(),display_assets()));
        let speakers=runtime.repository().map(|repo|crate::speaker_manager::SpeakerManager::new(repo.core.clone(),preferences::preferences_dir(),resource_dir.clone(),runtime.resource_budget()));
        let usage = UsageTracker::load(preferences::preferences_dir().join("usage.json"));
        let state = Self {
            audio_inputs: crate::audio_inputs::AudioInputs::new(),
            activity_gate:Mutex::new(()),
            exiting: std::sync::atomic::AtomicBool::new(false),
            exit_ready: std::sync::atomic::AtomicBool::new(false),
            preferences: Mutex::new(preferences),
            runtime,
            analysis,
            forum_lan,
            speakers,
            display,
            runtime_state: Mutex::new(RuntimeState::default()),
            resource_dir,
            wake_lock: WakeLock::new(),
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
        shared.input_gain.set(preferences.translation_input_gain as f32);
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
            let device = (!matches!(preferences.translation_input_device.as_str(), "__default_microphone__" | "__dual_audio__"))
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
                segment_id: None,
                source_revision: None,
                language_texts: if settings.target_language == "bilingual" {
                    ["zh", "en"].into_iter().map(|lang| (lang.to_string(), Self::sample_text(lang, index).to_string())).collect()
                } else { Default::default() },
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
                durable_deliveries: None,
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
        let status = if !active {
            "idle".to_owned()
        } else if shared.capture_context.read().is_some() {
            self.runtime_state.lock().status.clone()
        } else {
            shared.translation_overlay_status.read()
        };
        if shared.translation_overlay_status.read() != status {
            shared.translation_overlay_status.set(status.clone());
        }

        let (source_language, target_language) = shared.translation_lang_pair.read();
        let update = shared.translation.read();
        let session_id = update.as_ref().and_then(|u| u.durable_deliveries.as_ref().map(|b| b.session_id))
            .or_else(|| active.then(|| self.runtime.current_session_id()).flatten());
        let (history, pending_source_text) = update
            .map(|update| {
                (
                    update
                        .history
                        .into_iter()
                        .map(|sentence| Sentence {
                            segment_id: sentence.segment_id,
                            source_revision: sentence.source_revision,
                            source_language: sentence.source_language,
                            language_texts: sentence.language_texts,
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
        let app_language = preferences.app_language.clone();
        let translation_only = preferences.translation_only;
        let subtitle_side_by_side = preferences.translation_subtitle_side_by_side;
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
            app_language,
            session_id,
            runtime_message: self.runtime_state.lock().message.clone(),
            active,
            status,
            source_language,
            target_language,
            subtitle_side_by_side,
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
        // Serialize draining and applying events across IPC and the event bridge.
        let mut state = self.runtime_state.lock();
        let events = self.runtime.poll_events();
        for event in events {
            match event {
                RuntimeEvent::Started(id) => {
                    state.running = true;
                    state.status = "listening".into();
                    state.message = "正在收音，字幕持续更新".into();
                    if self.preferences.lock().meeting_speakers_enabled {
                        if let (Ok(manager), Ok(session)) = (&self.speakers, forum_contracts::Uuid::parse_str(&id)) {
                            if let Err(error) = manager.client().enable(session, None) {
                                log::warn!("自动启用匿名标签失败：{error}");
                                state.message = format!("字幕正常；匿名标签未启用：{error}");
                            }
                        }
                    }
                }
                RuntimeEvent::Stopped {
                    session_id,
                    incomplete,
                    translation_pending,
                } => {

                    self.wake_lock.stop();
                    if state.running {
                        if let Err(error) = self.usage.stop() {
                            log::error!("Could not stop usage timer: {error}");
                        }
                    }
                    state.running = false;
                    state.status = "idle".into();
                    if let Err(error) = self.save_transcript_if_needed() {
                        log::error!("Transcript export failed: {error}");
                    }
                    state.message = if incomplete {
                        format!("采音已停止；会议 {session_id} 有待恢复的音频或原文，剩余 {translation_pending} 项翻译")
                    } else if translation_pending > 0 {
                        format!("原文已完整保存；剩余 {translation_pending} 项译文可恢复")
                    } else {
                        "原文和译文已保存，音频设备已释放".into()
                    };
                    let shared = self.runtime.shared_state();
                    let requested = shared.translation_direction_request.read();
                    shared.translation_direction_active.set(requested.clone());
                    shared
                        .translation_lang_pair
                        .set((requested.source_language, requested.target_language));
                }
                RuntimeEvent::Draining(message) => {
                    state.running = true;
                    state.status = "draining".into();
                    state.message = message;
                }
                RuntimeEvent::Degraded(message) => {
                    state.running = true;
                    state.status = "degraded".into();
                    state.message = message;
                }
                RuntimeEvent::ShutdownFailed(message) => {

                    // Keep Stop available and prevent replacing a still-owned session.
                    state.running = true;
                    state.status = "error".into();
                    state.message = message;
                }
                RuntimeEvent::Error(message) => {

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

    fn save_transcript_if_needed(&self) -> Result<(), String> {
        let preferences = self.preferences.lock().clone();
        if !preferences.translation_auto_save_transcript
            && !preferences.translation_periodic_save_transcript
        {
            return Ok(());
        }
        let Some(session_id) = self.runtime.current_session_id() else {
            return Ok(());
        };
        let markdown = self.runtime.repository()?.export_markdown(session_id)?;

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
        let automatic = AutomaticAsrPaths::resolve(self.resource_dir.as_deref());
        let translation_runtime = translation_runtime_env(self.resource_dir.as_deref());
        ModelStatus {
            asr_ready: ModelPaths::resolve_current_for_mode(false).is_ok_and(|paths| paths.ready_for_mode(false, false)),
            automatic_asr_ready: automatic.is_ok(),
            automatic_asr_detail: automatic
                .err()
                .unwrap_or_else(|| "Qwen3-ASR 自动识别已准备".into()),
            translation_runtime_ready: translation_runtime.is_ok(),
            translation_runtime_detail: translation_runtime.err().unwrap_or_default(),
            translation_ready: ModelPaths::resolve_current()
                .is_ok_and(|paths| paths.ready_for(true)),
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
fn get_settings(app: tauri::AppHandle, state: State<'_, AppState>) -> SettingsPayload {
    let preferences = state.preferences.lock().clone();
    let runtime_state = state.poll_runtime_events();
    SettingsPayload {
        settings: TranslationSettings::from(&preferences),
        input_devices: state.audio_inputs.get_or_start(&preferences.translation_input_device, input_devices, move |devices| {
            let _ = app.emit("input-devices", devices);
        }),
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
    let running = state.runtime_state.lock().running;
    let keep_awake_changed = {
        let mut preferences = state.preferences.lock();
        if running
            && (preferences.translation_source_language != settings.source_language
                || preferences.translation_target_language != settings.target_language
                || preferences.translation_input_device != settings.input_device
                || preferences.translation_recording_enabled != settings.recording_enabled
                || preferences.translation_final_interval_seconds
                    != preferences::sanitize_final_interval_seconds(
                        settings.final_interval_seconds,
                    ))
        {
            return Err(
                "Stop live translation before changing its languages, audio input, recording, or caption interval"
                    .into(),
            );
        }
        let keep_awake_changed =
            preferences.translation_keep_awake != settings.keep_awake_during_translation;
        settings.apply_to(&mut preferences);
        preferences::save(&preferences)?;
        keep_awake_changed
    };
    state.sync_shared_state();
    if keep_awake_changed && running {
        if settings.keep_awake_during_translation {
            state.wake_lock.start()?;
        } else {
            state.wake_lock.stop();
        }
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

    let shared = state.runtime.shared_state();
    let requested = shared.translation_direction_request.read();
    let active = shared.translation_direction_active.read();
    if state.preferences.lock().translation_input_device=="__dual_audio__" {return Err("双轨会议请停止采集后再更改语向".into());}
    if requested.epoch != active.epoch {
        return Err("正在等待上一项语向切换到达音频段边界，请稍候".into());
    }
    let next_epoch = active.epoch.checked_add(1).ok_or("语向版本已达到上限")?;

    {
        let mut preferences = state.preferences.lock();
        if settings.source_language != preferences.translation_target_language
            || settings.target_language != preferences.translation_source_language
        {
            return Err("Live direction changes must swap the current source and target".into());
        }
        preferences.translation_source_language = settings.source_language.clone();
        preferences.translation_target_language = settings.target_language.clone();
        preferences::save(&preferences)?;
    }

    state.runtime.change_direction(TranslationDirection::new(
        settings.source_language,
        settings.target_language,
        next_epoch,
    ))?;

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
    insight_mode: Option<forum_contracts::InsightApprovalMode>,
) -> Result<RuntimeState, String> {
    begin_translation(app, state, settings, None, insight_mode.unwrap_or_default())
}

fn begin_translation(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: TranslationSettings,
    recovery: Option<forum_contracts::Uuid>,
    insight_mode: forum_contracts::InsightApprovalMode,
) -> Result<RuntimeState, String> {
    let _activity=state.activity_gate.lock();
    if state.exiting.load(std::sync::atomic::Ordering::Acquire) {return Err("应用正在退出".into());}
    if state.runtime_state.lock().running {
        return Err("请先完成上一场的停止或恢复".into());
    }
    if let Ok(speakers)=&state.speakers{speakers.client().disable();}
    let translate = settings.target_language != "none";
    let models = ModelPaths::resolve_current_for_mode(translate)?;
    if !models.ready_for_mode(settings.source_language == "auto", translate) {
        return Err(if translate { "双语会议模型不完整，请检查模型路径或下载本地模型" } else { "语音识别模型不完整，请检查 ASR 模型路径或下载本地模型" }.into());
    }
    let mut model_env = models.env_vars_for_mode(translate)?;
    model_env.extend(AutomaticAsrPaths::resolve(state.resource_dir.as_deref())?.env_vars()?);
    if translate { model_env.extend(translation_runtime_env(state.resource_dir.as_deref())?); }
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
    if settings.keep_awake_during_translation {
        state.wake_lock.start()?;
    } else {
        state.wake_lock.stop();
    }
    if let Err(error) = state.usage.start() {
        state.wake_lock.stop();
        return Err(error);
    }
    if let Err(error) = apply_overlay_window(&app, &settings) {
        let _ = state.usage.stop();
        state.wake_lock.stop();
        return Err(error);
    }
    if let Some(window) = app.get_webview_window("overlay") {
        if let Err(error) = window.show() {
            let _ = state.usage.stop();
            state.wake_lock.stop();
            return Err(error.to_string());
        }
    }

    let next = RuntimeState {
        running: true,
        status: "warming".into(),
        message: "Starting local translation…".into(),
    };
    // The event consumer takes the same lock. Publish the transition and submit
    // atomically so a fast Started/Error cannot be overwritten by late warming.
    let mut current = state.runtime_state.lock();
    if let Err(error) = state.runtime.start(
        dataflow,
        model_env,
        MeetingOptions {
            insight_mode,
            max_segment_ms: preferences::sanitize_final_interval_seconds(settings.final_interval_seconds) * 1000,            source_language: settings.source_language.clone(),
            target_language: settings.target_language.clone(),
            recording_enabled: settings.recording_enabled,
            system_audio: settings.input_device == "__system_audio__",
            dual_audio: settings.input_device == "__dual_audio__",
        },
        recovery,
    ) {
        state.wake_lock.stop();
        let _ = state.usage.stop();
        return Err(error);
    }
    *current = next.clone();
    Ok(next)
}

#[tauri::command]
fn stop_translation(state: State<'_, AppState>) -> Result<RuntimeState, String> {

    state.wake_lock.stop();
    // Submit while holding the event consumer's state lock. Slow transcript I/O
    // happens afterwards and must never overwrite a completed shutdown receipt.
    {
        let mut current = state.runtime_state.lock();
        if !current.running {
            return Ok(current.clone());
        }
        state.runtime.stop()?;
        *current = RuntimeState {
            running: true,
            status: "stopping".into(),
            message: "Stopping translation; waiting for resource cleanup".into(),
        };
    }
    *state.subtitle_preview_visible.lock() = false;
    // A transcript write error must not prevent the Stop command reaching capture.
    state.save_transcript_if_needed()?;
    Ok(state.runtime_state.lock().clone())
}

#[tauri::command]
fn get_meeting_sessions(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    state.runtime.repository()?.sessions()
}

#[tauri::command]
fn get_meeting_transcript(
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
    cursor: Option<u64>,
    after: Option<forum_core::PageKey>,
) -> Result<serde_json::Value, String> {
    state.runtime.repository()?.page(session_id, cursor, after)
}

#[tauri::command]
fn recover_meeting(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
) -> Result<RuntimeState, String> {
    let setup = state.runtime.repository()?.setup(session_id)?;
    let mut settings = TranslationSettings::from(&*state.preferences.lock());
    settings.source_language = setup.options.source_language;
    settings.target_language = setup.options.target_language;
    settings.recording_enabled = setup.options.recording_enabled;
    settings.final_interval_seconds = setup.options.max_segment_ms / 1000;
    // replay_only is carried separately; the capture bridge returns before
    // initializing CPAL/ScreenCaptureKit, regardless of this saved source kind.
    settings.input_device = if setup.options.dual_audio {
        "__dual_audio__".into()
    } else if setup.options.system_audio {
        "__system_audio__".into()
    } else {
        "__default_microphone__".into()
    };
    begin_translation(app, state, settings, Some(session_id), setup.options.insight_mode)
}

#[tauri::command]
fn get_usage(state: State<'_, AppState>) -> UsageSnapshot {
    state.usage.snapshot()
}

#[tauri::command]
fn show_meeting_window(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let window = app.get_webview_window("overlay").ok_or("实时会议窗口不可用")?;
    window.show().map_err(|e| e.to_string())?;
    window.unminimize().map_err(|e| e.to_string())?;
    state.runtime.shared_state().translation_window_visible.set(true);
    window.set_focus().map_err(|e| e.to_string())
}

#[tauri::command]
fn show_meeting_workspace(app: tauri::AppHandle, session_id: Option<String>, view: Option<String>) -> Result<(), String> {
    let view = if view.as_deref() == Some("settings") { "settings" } else { "forum" };
    let session_id = session_id.map(|id| forum_contracts::Uuid::parse_str(&id))
        .transpose().map_err(|e| e.to_string())?.map(|id| id.to_string());
    if app.get_webview_window("main").is_none() {
        // Bootstrap the route when the main webview does not yet have listeners.
        let mut config = app.config().app.windows.iter().find(|c| c.label == "main")
            .ok_or("主窗口配置不可用")?.clone();
        config.url = WebviewUrl::App(format!("index.html?view={view}&session={}",
            session_id.as_deref().unwrap_or("")).into());
        let window = WebviewWindowBuilder::from_config(&app, &config)
            .map_err(|e| e.to_string())?.build().map_err(|e| e.to_string())?;
        window.show().map_err(|e| e.to_string())?;
        return window.set_focus().map_err(|e| e.to_string());
    }
    show_or_create_main_window(&app)?;
    app.emit_to("main", if view == "settings" { "open-live-settings" } else { "open-forum-session" }, session_id).map_err(|e| e.to_string())
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

fn input_devices() -> Vec<String> {
    let mut devices = crate::audio_inputs::default_inputs();
    if let Ok(discovered) = cpal::default_host().input_devices() {
        devices.extend(discovered.filter_map(|device| device.name().ok()));
    }
    devices.sort_by(|left, right| {
        let rank = |value: &str| match value {
            "__system_audio__" => 0,
            "__default_microphone__" => 1,
            "__dual_audio__" => 2,
            _ => 3,
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
    devices.dedup();
    devices
}

fn create_overlay(app: &tauri::App) -> tauri::Result<WebviewWindow> {
    let overlay = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
        .title(crate::identity::PRODUCT_NAME)
        .inner_size(1180.0, 720.0)
        .min_inner_size(OVERLAY_MIN_INNER_WIDTH, OVERLAY_MIN_INNER_HEIGHT)
        .resizable(true)
        .decorations(false)
        .always_on_top(false)
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
                .set(false);
            if let Some(window) = app_handle.get_webview_window("overlay") {
                let _ = window.hide();
            }
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
        window.set_title(&format!("{name} · {}", if settings.app_language == "en" { "Operator console · Private" } else { "操作员控制台 · 私有" })).map_err(|error| error.to_string())?;
    }
    if let Some(window) = app.get_webview_window("overlay") {
        window.set_title(&format!("{name} · {}", if settings.app_language == "en" { "Public captions · Screen 1" } else { "公开字幕 · 屏幕 1" })).map_err(|error| error.to_string())?;
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

            if last_runtime_state.as_ref() != Some(&runtime_state) {
                if has_main_window {
                    let _ = app_handle.emit_to("main", "runtime-state", &runtime_state);
                }
                last_runtime_state = Some(runtime_state.clone());
            }

            let direction_state = state.direction_switch_state();
            if last_direction_state.as_ref() != Some(&direction_state) {
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
            // Input diagnostics are private to the operator; never emit to audience windows.
            let levels = state.runtime.shared_state().mic.take_levels();
            if has_main_window && runtime_state.running && !levels.is_empty() {
                let _ = app_handle.emit_to("main", "audio-level", levels);
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
                let translate = initial_settings.target_language != "none";
                let mut model_env=ModelPaths::resolve_current_for_mode(translate).and_then(|models|models.env_vars_for_mode(translate)).map_err(anyhow::Error::msg)?;
                model_env.extend(AutomaticAsrPaths::resolve(state.resource_dir.as_deref()).and_then(|paths|paths.env_vars()).map_err(anyhow::Error::msg)?);
                if translate { model_env.extend(translation_runtime_env(state.resource_dir.as_deref()).map_err(anyhow::Error::msg)?); }
                state
                    .runtime
                    .start(
                        dataflow.into(),
                        model_env,
                        MeetingOptions {
                            insight_mode: Default::default(),
                            max_segment_ms: preferences::sanitize_final_interval_seconds(initial_settings.final_interval_seconds) * 1000,
                            source_language: initial_settings.source_language.clone(),
                            target_language: initial_settings.target_language.clone(),
                            recording_enabled: initial_settings.recording_enabled,
                            system_audio: initial_settings.input_device == "__system_audio__",
                            dual_audio: initial_settings.input_device == "__dual_audio__",
                        },
                        None,
                    )
                    .map_err(|error| anyhow::anyhow!(error))?;
                state.usage.start().map_err(anyhow::Error::msg)?;
            }
            start_event_bridge(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            list_meeting_sessions_page,
            import_legacy_transcript,
            get_analysis_state,
            set_insight_settings,
            create_analysis_job,
            cancel_analysis_job,
            retry_analysis_job,
            revise_artifact,
            review_artifact,
            publish_artifact,
            approve_and_publish_artifact,
            prepare_insight_publication,
            hide_artifact,
            get_analysis_evidence,
            get_analysis_artifact,
            export_artifact,
            get_display_info,
            show_insight_wall,
            get_insight_settings,
            revoke_display,
            get_public_snapshot,
            get_lan_state,create_lan_identity,start_lan,stop_lan,create_participant_access,create_peer_invite,pair_forum_peer,revoke_lan_access,disconnect_forum_peer,get_public_sessions,search_public_content,
            join_forum_event,
            get_speaker_status,get_speaker_assignments,enable_session_speakers,disable_session_speakers,correct_speaker_assignment,
            get_model_status,
            start_model_download,
            update_settings,
            swap_translation_direction,
            get_direction_switch_state,
            start_translation,
            stop_translation,
            get_usage,
            get_meeting_sessions,
            get_meeting_transcript,
            recover_meeting,
            get_overlay_state,
            show_meeting_window,
            show_meeting_workspace,
            open_transcript_history,
            toggle_subtitle_preview,
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
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<AppState>();
                use std::sync::atomic::Ordering;
                if state.exit_ready.load(Ordering::Acquire) { return; }
                api.prevent_exit();
                if state.exiting.swap(true, Ordering::AcqRel) { return; }
                if let Ok(analysis) = &state.analysis { analysis.shutdown(); }
                if let Ok(lan)=&state.forum_lan{lan.shutdown();}
                if let Ok(speakers)=&state.speakers{speakers.shutdown();}
                state.runtime.begin_shutdown();

                state.wake_lock.stop();
                if state.usage.snapshot().running {
                    if let Err(error) = state.usage.stop() {
                        log::error!("Could not save usage before exit: {error}");
                    }
                }
                let handle=app.clone();
                thread::spawn(move || {
                    let deadline=std::time::Instant::now()+Duration::from_secs(45);
                    loop {
                        let state=handle.state::<AppState>();
                        let analysis_done=state.analysis.as_ref().map_or(true,|a|a.shutdown_complete());
                        if state.runtime.shutdown_complete() && analysis_done && state.forum_lan.as_ref().map_or(true,|lan|lan.shutdown_complete()) && state.speakers.as_ref().map_or(true,|s|s.shutdown_complete()) {
                            state.exit_ready.store(true, Ordering::Release);
                            handle.exit(0);break;
                        }
                        if std::time::Instant::now()>=deadline {
                            let mut runtime=state.runtime_state.lock();
                            runtime.status="error".into();runtime.message="退出收尾尚未完成，资源退出未确认；请检查会议状态后重试退出".into();
                            state.exiting.store(false,Ordering::Release);
                            let _=show_or_create_main_window(&handle);break;
                        }
                        drop(state);thread::sleep(Duration::from_millis(100));
                    }
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_preserves_translation_preferences() {
        let original = AppPreferences {
            app_language: "en".into(),
            translation_input_gain: 2.25,
            translation_subtitle_side_by_side: true,
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
        assert_eq!(original.translation_input_gain, updated.translation_input_gain);
        assert_eq!(original.translation_subtitle_side_by_side, updated.translation_subtitle_side_by_side);
        assert_eq!(original.app_language, updated.app_language);
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
    }

    #[test]
    fn translation_only_is_disabled_without_a_target_language() {
        let preferences = AppPreferences::default();
        let mut settings = TranslationSettings::from(&preferences);
        settings.target_language = "none".into();
        settings.translation_only = true;
        settings.subtitle_side_by_side = true;

        let mut updated = AppPreferences::default();
        settings.apply_to(&mut updated);

        assert!(!updated.translation_only);
        assert!(!updated.translation_subtitle_side_by_side);
        assert!(updated.translation_subtitle_split);
    }

    #[test]
    fn meeting_window_reserves_space_for_captions_and_insights() {
        assert_eq!(OVERLAY_MIN_INNER_WIDTH, 560.0);
        assert_eq!(OVERLAY_MIN_INNER_HEIGHT, 560.0);
    }
}
