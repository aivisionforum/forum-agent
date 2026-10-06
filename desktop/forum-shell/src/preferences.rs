use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

pub const DEFAULT_FINAL_INTERVAL_SECONDS: u64 = 3;
pub const MIN_FINAL_INTERVAL_SECONDS: u64 = 3;
pub const MAX_FINAL_INTERVAL_SECONDS: u64 = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppPreferences {
    pub app_language: String,
    pub accent_theme: String,
    pub translation_recording_enabled: bool,
    pub meeting_speakers_enabled: bool,
    pub translation_auto_save_transcript: bool,
    pub translation_periodic_save_transcript: bool,
    pub translation_transcript_file_name: String,
    pub translation_transcript_save_dir: Option<String>,
    pub translation_source_language: String,
    pub translation_target_language: String,
    pub translation_input_device: String,
    pub translation_input_gain: f64,
    pub translation_subtitle_side_by_side: bool,
    pub translation_subtitle_split: bool,
    pub translation_only: bool,
    pub translation_overlay_opacity: f64,
    pub translation_font_size_preset: String,
    pub translation_anchor_position_preset: String,
    pub translation_final_interval_seconds: u64,
    pub translation_keep_awake: bool,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            app_language: "zh".into(),
            accent_theme: "neon-blue".into(),
            translation_recording_enabled: true,
            meeting_speakers_enabled: false,
            translation_auto_save_transcript: false,
            translation_periodic_save_transcript: false,
            translation_transcript_file_name: "transcript.md".into(),
            translation_transcript_save_dir: None,
            translation_source_language: "zh".into(),
            translation_target_language: "en".into(),
            translation_input_device: "__system_audio__".into(),
            translation_input_gain: 1.0,
            translation_subtitle_side_by_side: false,
            translation_subtitle_split: false,
            translation_only: false,
            translation_overlay_opacity: 1.0,
            translation_font_size_preset: "24".into(),
            translation_anchor_position_preset: "50".into(),
            translation_final_interval_seconds: DEFAULT_FINAL_INTERVAL_SECONDS,
            translation_keep_awake: true,
        }
    }
}

pub fn preferences_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::identity::DATA_DIRECTORY_NAME)
}

pub fn preferences_path() -> PathBuf {
    preferences_dir().join("app_preferences.json")
}

pub fn transcript_dir(preferences: &AppPreferences) -> PathBuf {
    preferences
        .translation_transcript_save_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| preferences_dir().join("transcripts"))
}

pub fn load() -> AppPreferences {
    let mut preferences = fs::read_to_string(preferences_path())
        .ok()
        .and_then(|content| serde_json::from_str::<AppPreferences>(&content).ok())
        .unwrap_or_default();
    sanitize(&mut preferences);
    preferences
}

pub fn save(preferences: &AppPreferences) -> Result<(), String> {
    let directory = preferences_dir();
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create preferences directory: {error}"))?;
    let json = serde_json::to_string_pretty(preferences)
        .map_err(|error| format!("Could not serialize preferences: {error}"))?;
    fs::write(preferences_path(), json)
        .map_err(|error| format!("Could not save preferences: {error}"))
}

pub fn default_input_gain() -> f64 { 1.0 }

pub fn sanitize_input_gain(value: f64) -> f64 {
    if value.is_finite() { value.clamp(0.0, 3.0) } else { 1.0 }
}

fn sanitize(preferences: &mut AppPreferences) {
    preferences.translation_input_gain = sanitize_input_gain(preferences.translation_input_gain);
    if !matches!(
        preferences.translation_source_language.as_str(),
        "auto" | "zh" | "en" | "ja" | "fr"
    ) {
        preferences.translation_source_language = "zh".into();
    }
    if !matches!(
        preferences.translation_target_language.as_str(),
        "zh" | "en" | "ja" | "fr" | "bilingual" | "none"
    ) {
        preferences.translation_target_language = "en".into();
    }
    if preferences.translation_target_language == "none" {
        preferences.translation_subtitle_split = true;
        preferences.translation_only = false;
        preferences.translation_subtitle_side_by_side = false;
    } else if preferences.translation_subtitle_side_by_side {
        preferences.translation_subtitle_split = false;
    }
    if !matches!(
        preferences.translation_font_size_preset.as_str(),
        "16" | "20" | "24" | "30" | "36" | "44" | "52" | "64" | "80" | "96" | "120" | "160"
    ) {
        preferences.translation_font_size_preset = "24".into();
    }
    if !matches!(
        preferences.translation_anchor_position_preset.as_str(),
        "35" | "50" | "70" | "100"
    ) {
        preferences.translation_anchor_position_preset = "50".into();
    }
    preferences.translation_overlay_opacity =
        preferences.translation_overlay_opacity.clamp(0.35, 1.0);
    preferences.translation_final_interval_seconds =
        sanitize_final_interval_seconds(preferences.translation_final_interval_seconds);
    if !matches!(preferences.app_language.as_str(), "zh" | "en") {
        preferences.app_language = "zh".into();
    }
    if !matches!(
        preferences.accent_theme.as_str(),
        "neon-blue" | "neon-orange" | "neon-pink" | "neon-green"
    ) {
        preferences.accent_theme = "neon-blue".into();
    }
}

pub fn sanitize_final_interval_seconds(value: u64) -> u64 {
    value.clamp(MIN_FINAL_INTERVAL_SECONDS, MAX_FINAL_INTERVAL_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_preferences_keep_layout_and_default_to_unity_gain() {
        let mut prefs: AppPreferences = serde_json::from_str(r#"{"translation_subtitle_split":true}"#).unwrap();
        sanitize(&mut prefs);
        assert_eq!(prefs.translation_input_gain, 1.0);
        assert!(prefs.translation_subtitle_split);
        assert!(!prefs.translation_subtitle_side_by_side);
        prefs.translation_subtitle_side_by_side = true;
        sanitize(&mut prefs);
        assert!(!prefs.translation_subtitle_split);
        prefs.translation_target_language = "none".into();
        sanitize(&mut prefs);
        assert!(prefs.translation_subtitle_split);
        assert!(!prefs.translation_subtitle_side_by_side);
    }

    #[test]
    fn invalid_gain_cannot_mute_or_overamplify_by_accident() {
        assert_eq!(sanitize_input_gain(f64::NAN), 1.0);
        assert_eq!(sanitize_input_gain(f64::INFINITY), 1.0);
        assert_eq!(sanitize_input_gain(-1.0), 0.0);
        assert_eq!(sanitize_input_gain(99.0), 3.0);
        assert_eq!(sanitize_input_gain(2.25), 2.25);
    }

    #[test]
    fn sanitizes_invalid_translation_values() {
        let mut preferences = AppPreferences {
            translation_source_language: "xx".into(),
            translation_target_language: "yy".into(),
            translation_font_size_preset: "999".into(),
            translation_anchor_position_preset: "42".into(),
            translation_final_interval_seconds: 99,
            translation_overlay_opacity: 0.1,
            accent_theme: "unknown".into(),
            ..AppPreferences::default()
        };
        sanitize(&mut preferences);
        assert_eq!(preferences.translation_source_language, "zh");
        assert_eq!(preferences.translation_target_language, "en");
        assert_eq!(preferences.translation_font_size_preset, "24");
        assert_eq!(preferences.translation_anchor_position_preset, "50");
        assert_eq!(
            preferences.translation_final_interval_seconds,
            MAX_FINAL_INTERVAL_SECONDS
        );
        assert_eq!(preferences.translation_overlay_opacity, 0.35);
        assert_eq!(preferences.accent_theme, "neon-blue");
    }

    #[test]
    fn preserves_supported_accent_themes() {
        for theme in ["neon-blue", "neon-orange", "neon-pink", "neon-green"] {
            let mut preferences = AppPreferences {
                accent_theme: theme.into(),
                ..AppPreferences::default()
            };
            sanitize(&mut preferences);
            assert_eq!(preferences.accent_theme, theme);
        }
    }

    #[test]
    fn live_translation_keeps_the_screen_awake_by_default() {
        assert!(AppPreferences::default().translation_keep_awake);
    }

    #[test]
    fn legacy_tts_preferences_are_ignored_without_resetting_meeting_settings() {
        let preferences: AppPreferences = serde_json::from_str(
            r#"{
            "experimental_spoken_translation_enabled": true,
            "experimental_spoken_translation_voice": "apple-voice-1",
            "experimental_spoken_translation_output_device": "Speakers",
            "translation_source_language": "auto",
            "translation_target_language": "bilingual",
            "meeting_speakers_enabled": true
        }"#,
        )
        .unwrap();
        assert_eq!(preferences.translation_source_language, "auto");
        assert_eq!(preferences.translation_target_language, "bilingual");
        assert!(preferences.meeting_speakers_enabled);
        let saved = serde_json::to_value(preferences).unwrap();
        assert!(!saved
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.contains("spoken_translation")));
    }

    #[test]
    fn existing_preferences_enable_the_new_wake_setting() {
        let preferences: AppPreferences =
            serde_json::from_str("{}").expect("legacy preferences should deserialize");
        assert!(preferences.translation_keep_awake);
    }

    #[test]
    fn stacked_subtitles_are_the_default() {
        let preferences = AppPreferences::default();
        assert!(!preferences.translation_subtitle_split);
        assert!(!preferences.translation_only);
        assert_eq!(
            preferences.translation_final_interval_seconds,
            DEFAULT_FINAL_INTERVAL_SECONDS
        );
    }

    #[test]
    fn clamps_final_interval_to_supported_range() {
        assert_eq!(
            sanitize_final_interval_seconds(0),
            MIN_FINAL_INTERVAL_SECONDS
        );
        assert_eq!(sanitize_final_interval_seconds(3), 3);
        assert_eq!(
            sanitize_final_interval_seconds(99),
            MAX_FINAL_INTERVAL_SECONDS
        );
    }

    #[test]
    fn no_translation_uses_source_only_caption_layout() {
        let mut preferences = AppPreferences {
            translation_target_language: "none".into(),
            translation_subtitle_split: false,
            translation_only: true,
            ..AppPreferences::default()
        };

        sanitize(&mut preferences);

        assert!(preferences.translation_subtitle_split);
        assert!(!preferences.translation_only);
    }
}
