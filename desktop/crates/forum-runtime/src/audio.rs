//! Stable identity shared by capture, ASR and replay. Legacy burst IDs are hints only.
use forum_contracts::{AudioRange, Uuid};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentMeta {
    pub session_id: Uuid,
    pub track_id: Uuid,
    pub segment_id: Uuid,
    pub revision: forum_contracts::Revision,
    pub audio: AudioRange,
    pub configured_source_language: String,
    pub target_languages: Vec<String>,
    pub direction_epoch: u64,
    pub partial_seq: u64,
    pub final_segment: bool,
}
impl SegmentMeta {
    pub fn validate(&self, sample_count: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.session_id.is_nil() && !self.track_id.is_nil() && !self.segment_id.is_nil(),
            "nil segment identity"
        );
        self.audio.validate()?;
        anyhow::ensure!(
            self.audio.end_sample - self.audio.start_sample == sample_count as u64
                && self.audio.sample_rate == 16000,
            "segment PCM range mismatch"
        );
        anyhow::ensure!(
            self.direction_epoch > 0 && !self.configured_source_language.trim().is_empty(),
            "missing segment language direction"
        );
        Ok(())
    }
}
