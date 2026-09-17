use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Preparing,
    Ready,
    Recording,
    Stopping,
    Draining,
    Completed,
    Interrupted,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Recording => "recording",
            Self::Stopping => "stopping",
            Self::Draining => "draining",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
        }
    }
    pub fn permits(self, next: Self) -> bool {
        use SessionState::*;
        matches!(
            (self, next),
            (Created, Preparing) | (Preparing, Ready) | (Ready, Recording) | (Recording, Stopping)
        ) || (next == Interrupted && !matches!(self, Completed | Interrupted))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionTransition {
    pub expected_state: SessionState,
    pub next_state: SessionState,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioGap {
    pub gap_id: Uuid,
    pub track_id: Uuid,
    pub audio: AudioRange,
    pub reason: String,
    pub recoverable: bool,
}
impl AudioGap {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.gap_id, "gap_id")?;
        non_nil(self.track_id, "track_id")?;
        self.audio.validate()?;
        non_blank(&self.reason, "reason")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackSeal {
    pub track_id: Uuid,
    pub final_sample: u64,
    pub segment_ids: Vec<Uuid>,
}

pub fn capture_manifest_sha256(tracks: &[TrackSeal]) -> Result<String> {
    let mut sorted = tracks.to_vec();
    let mut unique = HashSet::new();
    for track in &mut sorted {
        non_nil(track.track_id, "track_id")?;
        sqlite_integer(track.final_sample, "final_sample")?;
        if !unique.insert(track.track_id) {
            return Err(ValidationError::Invalid("duplicate_track"));
        }
        track.segment_ids.sort();
        for (i, id) in track.segment_ids.iter().enumerate() {
            non_nil(*id, "segment_id")?;
            if i > 0 && track.segment_ids[i - 1] == *id {
                return Err(ValidationError::Invalid("duplicate_segment"));
            }
        }
    }
    sorted.sort_by_key(|t| t.track_id);
    let json =
        serde_json::to_vec(&sorted).map_err(|_| ValidationError::Invalid("capture_manifest"))?;
    Ok(format!("{:x}", Sha256::digest(json)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureStopped {
    pub tracks: Vec<TrackSeal>,
    pub manifest_sha256: String,
}
impl CaptureStopped {
    pub fn validate(&self) -> Result<()> {
        if self.tracks.is_empty() || capture_manifest_sha256(&self.tracks)? != self.manifest_sha256
        {
            return Err(ValidationError::Invalid("capture_manifest"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerSeal {
    pub producer_run_id: Uuid,
    /// Last durable sequence BEFORE this seal; seal itself uses final_seq + 1.
    pub final_seq: u64,
    pub segment_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranscriptSeal {
    pub capture_manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DirectionChanged {
    pub expected_epoch: u64,
    pub direction_epoch: u64,
    pub target_languages: Vec<String>,
    /// Some applies only to capture at/after this sample boundary. None is an
    /// explicit retroactive retranslation of this session's current sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<DirectionBoundary>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DirectionBoundary {
    pub track_id: Uuid,
    pub start_sample: u64,
    pub configured_source_language: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranslationRequested {
    pub translation_id: Uuid,
    pub revision: Revision,
    pub attempt: u32,
    pub target_language: String,
    pub direction_epoch: u64,
    pub source_spans: Vec<SourceSpan>,
    pub input_text: String,
    pub normalization_version: String,
    pub backend: String,
    pub model_manifest_id: String,
}
impl TranslationRequested {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.translation_id, "translation_id")?;
        if self.attempt == 0 || self.direction_epoch == 0 || self.source_spans.is_empty() {
            return Err(ValidationError::Invalid("translation_fence_or_sources"));
        }
        sqlite_integer(self.direction_epoch, "direction_epoch")?;
        non_blank(&self.target_language, "target_language")?;
        non_blank(&self.backend, "backend")?;
        non_blank(&self.model_manifest_id, "model_manifest_id")?;
        let separator = match self.normalization_version.as_str() {
            "identity-v1" => "",
            "join-space-v1" => " ",
            "join-newline-v1" => "\n",
            _ => return Err(ValidationError::Invalid("normalization_version")),
        };
        let reconstructed = self
            .source_spans
            .iter()
            .map(|s| s.quote.as_str())
            .collect::<Vec<_>>()
            .join(separator);
        if reconstructed != self.input_text || self.input_text.is_empty() {
            return Err(ValidationError::Invalid("input_text"));
        }
        let mut seen = Vec::<&SourceSpan>::new();
        for span in &self.source_spans {
            non_nil(span.segment_id, "segment_id")?;
            if span.end_utf8 <= span.start_utf8
                || span.quote.len() != span.end_utf8 - span.start_utf8
            {
                return Err(ValidationError::InvalidSpan);
            }
            if seen.iter().any(|s| {
                s.segment_id == span.segment_id
                    && s.segment_revision == span.segment_revision
                    && s.start_utf8 < span.end_utf8
                    && span.start_utf8 < s.end_utf8
            }) {
                return Err(ValidationError::Invalid("overlapping_sources"));
            }
            seen.push(span);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranslationStatus {
    Final,
    Passthrough,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranslationFinal {
    pub translation_id: Uuid,
    pub revision: Revision,
    pub attempt: u32,
    pub direction_epoch: u64,
    pub text: String,
    pub status: TranslationStatus,
    pub backend: String,
    pub model_manifest_id: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranslationFailed {
    pub translation_id: Uuid,
    pub revision: Revision,
    pub attempt: u32,
    pub direction_epoch: u64,
    pub code: String,
    pub reason: String,
}

/// Host-only evidence. Boolean assertions must come from owned process cleanup
/// and a fully acknowledged producer outbox, never from untrusted node claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerRecoveryEvidence {
    pub capture_manifest_sha256: String,
    pub owned_process_exited: bool,
    pub outbox_replayed: bool,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerReconciled {
    pub producer_run_id: Uuid,
    pub final_seq: u64,
    pub evidence: ProducerRecoveryEvidence,
}

impl TranslationFinal {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.translation_id, "translation_id")?;
        if self.attempt == 0 || self.direction_epoch == 0 {
            return Err(ValidationError::Invalid("translation_fence"));
        }
        sqlite_integer(self.direction_epoch, "direction_epoch")?;
        sqlite_integer(self.elapsed_ms, "elapsed_ms")?;
        non_blank(&self.backend, "backend")?;
        non_blank(&self.model_manifest_id, "model_manifest_id")?;
        if self.status == TranslationStatus::Final {
            non_blank(&self.text, "text")?;
        }
        Ok(())
    }
}
impl TranslationFailed {
    pub fn validate(&self) -> Result<()> {
        non_nil(self.translation_id, "translation_id")?;
        if self.attempt == 0 || self.direction_epoch == 0 {
            return Err(ValidationError::Invalid("translation_fence"));
        }
        sqlite_integer(self.direction_epoch, "direction_epoch")?;
        non_blank(&self.code, "code")?;
        non_blank(&self.reason, "reason")
    }
}
