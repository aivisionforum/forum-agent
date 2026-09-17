//! The implemented F02 subset of the Forum v1 ingestion contract.
//! Unknown fields and unknown event types are rejected rather than silently lost.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
mod analysis;
pub use analysis::*;
mod reliable;
pub use reliable::*;
use std::collections::HashSet;
use thiserror::Error;
pub use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("invalid field: {0}")]
    Invalid(&'static str),
    #[error("unsupported schema version")]
    UnsupportedSchema,
    #[error("event type does not match payload")]
    EventTypeMismatch,
    #[error("source span is empty, outside UTF-8 boundaries, or does not match its quote")]
    InvalidSpan,
}

pub type Result<T> = std::result::Result<T, ValidationError>;

pub fn non_nil(id: Uuid, field: &'static str) -> Result<()> {
    if id.is_nil() {
        Err(ValidationError::Invalid(field))
    } else {
        Ok(())
    }
}

pub fn non_blank(value: &str, field: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        Err(ValidationError::Invalid(field))
    } else {
        Ok(())
    }
}

fn sqlite_integer(value: u64, field: &'static str) -> Result<()> {
    if value > 9_007_199_254_740_991 {
        Err(ValidationError::Invalid(field))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "u32", into = "u32")]
pub struct Revision(u32);

impl Revision {
    pub const FIRST: Self = Self(1);
    pub fn get(self) -> u32 {
        self.0
    }
    pub fn next(self) -> Result<Self> {
        self.0
            .checked_add(1)
            .ok_or(ValidationError::Invalid("revision"))
            .and_then(Self::try_from)
    }
}

impl TryFrom<u32> for Revision {
    type Error = ValidationError;
    fn try_from(value: u32) -> Result<Self> {
        if value == 0 {
            Err(ValidationError::Invalid("revision"))
        } else {
            Ok(Self(value))
        }
    }
}

impl From<Revision> for u32 {
    fn from(value: Revision) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionSpec {
    pub session_id: Uuid,
    pub event_id: Uuid,
    pub room_id: Uuid,
    pub owner_device_id: Uuid,
    pub title: String,
}

impl SessionSpec {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.session_id, "session_id")?;
        non_nil(self.event_id, "event_id")?;
        non_nil(self.room_id, "room_id")?;
        non_nil(self.owner_device_id, "owner_device_id")?;
        non_blank(&self.title, "title")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Mic,
    System,
    RoomMix,
    Replay,
    /// Transcript-only import; sample coordinates are milliseconds, not recorded PCM.
    LegacyImport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackSpec {
    pub track_id: Uuid,
    pub session_id: Uuid,
    pub kind: TrackKind,
    pub sample_rate: u32,
}

impl TrackSpec {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.track_id, "track_id")?;
        non_nil(self.session_id, "session_id")?;
        if self.sample_rate == 0 {
            return Err(ValidationError::Invalid("sample_rate"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioRange {
    pub start_sample: u64,
    pub end_sample: u64,
    pub sample_rate: u32,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl AudioRange {
    pub fn validate(&self) -> Result<()> {
        if self.sample_rate == 0
            || self.end_sample <= self.start_sample
            || self.end_ms < self.start_ms
        {
            return Err(ValidationError::Invalid("audio_range"));
        }
        for (value, name) in [
            (self.start_sample, "start_sample"),
            (self.end_sample, "end_sample"),
            (self.start_ms, "start_ms"),
            (self.end_ms, "end_ms"),
        ] {
            sqlite_integer(value, name)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    pub name: String,
    pub run_id: Uuid,
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum EventType {
    #[serde(rename = "session.changed")]
    SessionChanged,
    #[serde(rename = "audio.gap")]
    AudioGap,
    #[serde(rename = "session.capture_stopped")]
    CaptureStopped,
    #[serde(rename = "session.producer_sealed")]
    ProducerSealed,
    #[serde(rename = "session.producer_reconciled")]
    ProducerReconciled,
    #[serde(rename = "session.transcript_sealed")]
    TranscriptSealed,
    #[serde(rename = "translation.requested")]
    TranslationRequested,
    #[serde(rename = "translation.final")]
    TranslationFinal,
    #[serde(rename = "translation.failed")]
    TranslationFailed,
    #[serde(rename = "translation.direction_changed")]
    DirectionChanged,
    #[serde(rename = "audio.segment_closed")]
    AudioSegmentClosed,
    #[serde(rename = "transcript.final")]
    TranscriptFinal,
    #[serde(rename = "transcript.revised")]
    TranscriptRevised,
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionChanged => "session.changed",
            Self::AudioGap => "audio.gap",
            Self::CaptureStopped => "session.capture_stopped",
            Self::ProducerSealed => "session.producer_sealed",
            Self::ProducerReconciled => "session.producer_reconciled",
            Self::TranscriptSealed => "session.transcript_sealed",
            Self::TranslationRequested => "translation.requested",
            Self::TranslationFinal => "translation.final",
            Self::TranslationFailed => "translation.failed",
            Self::DirectionChanged => "translation.direction_changed",
            Self::AudioSegmentClosed => "audio.segment_closed",
            Self::TranscriptFinal => "transcript.final",
            Self::TranscriptRevised => "transcript.revised",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Event<T> {
    pub schema_version: u32,
    pub message_id: Uuid,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub event_id: Uuid,
    pub room_id: Uuid,
    pub session_id: Uuid,
    pub producer: Producer,
    pub payload: T,
}

impl<T> Event<T> {
    pub fn validate_envelope(&self, expected: EventType) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedSchema);
        }
        if self.event_type != expected {
            return Err(ValidationError::EventTypeMismatch);
        }
        for (id, name) in [
            (self.message_id, "message_id"),
            (self.event_id, "event_id"),
            (self.room_id, "room_id"),
            (self.session_id, "session_id"),
            (self.producer.run_id, "producer.run_id"),
        ] {
            non_nil(id, name)?;
        }
        non_blank(&self.producer.name, "producer.name")?;
        if self.producer.seq == 0 {
            return Err(ValidationError::Invalid("producer.seq"));
        }
        sqlite_integer(self.producer.seq, "producer.seq")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureSegmentClosed {
    pub track_id: Uuid,
    pub segment_id: Uuid,
    pub audio: AudioRange,
    /// An opaque recording-manifest reference, never opened as a path here.
    pub recording_ref: Option<String>,
}

impl CaptureSegmentClosed {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.track_id, "track_id")?;
        non_nil(self.segment_id, "segment_id")?;
        self.audio.validate()?;
        if let Some(reference) = &self.recording_ref {
            non_blank(reference, "recording_ref")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Success,
    Empty,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranscriptFinal {
    pub track_id: Uuid,
    pub segment_id: Uuid,
    pub revision: Revision,
    pub audio: AudioRange,
    pub text: String,
    pub configured_source_language: String,
    pub detected_language: Option<String>,
    pub target_languages: Vec<String>,
    pub direction_epoch: u64,
    pub speaker_id: Option<Uuid>,
    pub status: TranscriptStatus,
    pub reason: Option<String>,
    pub backend: String,
    pub model_manifest_id: String,
}

impl TranscriptFinal {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.track_id, "track_id")?;
        non_nil(self.segment_id, "segment_id")?;
        self.audio.validate()?;
        non_blank(
            &self.configured_source_language,
            "configured_source_language",
        )?;
        if let Some(language) = &self.detected_language {
            non_blank(language, "detected_language")?;
        }
        let mut targets = HashSet::new();
        for language in &self.target_languages {
            non_blank(language, "target_languages")?;
            if !targets.insert(language) {
                return Err(ValidationError::Invalid("duplicate_target_language"));
            }
        }
        if self.direction_epoch == 0 {
            return Err(ValidationError::Invalid("direction_epoch"));
        }
        sqlite_integer(self.direction_epoch, "direction_epoch")?;
        non_blank(&self.backend, "backend")?;
        non_blank(&self.model_manifest_id, "model_manifest_id")?;
        if let Some(speaker) = self.speaker_id {
            non_nil(speaker, "speaker_id")?;
        }
        match self.status {
            TranscriptStatus::Success => non_blank(&self.text, "text")?,
            TranscriptStatus::Empty | TranscriptStatus::Failed => {
                if !self.text.is_empty() {
                    return Err(ValidationError::Invalid("non_success_text"));
                }
                non_blank(self.reason.as_deref().unwrap_or(""), "reason")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRevision {
    pub segment_id: Uuid,
    pub expected_revision: Revision,
    pub text: String,
    pub reason: String,
    pub operator_id: String,
}

impl TranscriptRevision {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.segment_id, "segment_id")?;
        non_blank(&self.text, "text")?;
        non_blank(&self.reason, "reason")?;
        non_blank(&self.operator_id, "operator_id")?;
        self.expected_revision.next()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub segment_id: Uuid,
    pub segment_revision: Revision,
    pub start_utf8: usize,
    pub end_utf8: usize,
    pub quote: String,
}

impl SourceSpan {
    pub fn validate_against<'a>(&self, text: &'a str) -> Result<&'a str> {
        non_nil(self.segment_id, "segment_id")?;
        if self.end_utf8 <= self.start_utf8 || self.quote.is_empty() {
            return Err(ValidationError::InvalidSpan);
        }
        match text.get(self.start_utf8..self.end_utf8) {
            Some(source) if source == self.quote => Ok(source),
            _ => Err(ValidationError::InvalidSpan),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_and_uuid_deserialization_reject_invalid_values() {
        assert!(serde_json::from_str::<Revision>("0").is_err());
        assert!(serde_json::from_str::<Uuid>("\"not-a-uuid\"").is_err());
        assert!(Revision::try_from(u32::MAX).unwrap().next().is_err());
        assert_eq!(serde_json::to_string(&Revision::FIRST).unwrap(), "1");
    }

    #[test]
    fn utf8_spans_use_byte_boundaries_and_exact_content() {
        let text = "中📝e\u{301}文";
        let mut span = SourceSpan {
            segment_id: Uuid::new_v4(),
            segment_revision: Revision::FIRST,
            start_utf8: 3,
            end_utf8: 7,
            quote: "📝".into(),
        };
        assert_eq!(span.validate_against(text).unwrap(), "📝");
        span.start_utf8 = 4;
        assert!(span.validate_against(text).is_err());
        span.start_utf8 = 3;
        span.quote = "别".into();
        assert!(span.validate_against(text).is_err());
        span.start_utf8 = 7;
        span.end_utf8 = 10;
        span.quote = "e\u{301}".into();
        assert_eq!(span.validate_against(text).unwrap(), "e\u{301}");
        span.end_utf8 = span.start_utf8;
        span.quote.clear();
        assert!(span.validate_against(text).is_err());
    }

    #[test]
    fn audio_ranges_reject_empty_reversed_and_sqlite_overflow() {
        let mut range = AudioRange {
            start_sample: 0,
            end_sample: 16000,
            sample_rate: 16000,
            start_ms: 0,
            end_ms: 1000,
        };
        assert!(range.validate().is_ok());
        range.end_sample = 0;
        assert!(range.validate().is_err());
        range.end_sample = u64::MAX;
        assert!(range.validate().is_err());
        range.end_sample = 16000;
        range.sample_rate = 0;
        assert!(range.validate().is_err());
    }
}
