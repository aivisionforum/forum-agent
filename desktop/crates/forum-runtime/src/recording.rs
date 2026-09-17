//! Processed 16 kHz PCM journal. It does not claim to preserve raw hardware audio.
use crate::{atomic_write, private_directory};
use anyhow::{ensure, Context};
use forum_contracts::{AudioRange, TrackSpec};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingChunk {
    pub chunk_id: Uuid,
    pub file: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedSegment {
    pub segment_id: Uuid,
    pub audio: AudioRange,
    pub metadata: crate::audio::SegmentMeta,
    pub file: Option<String>,
    pub sha256: Option<String>,
    pub capture_event: Option<serde_json::Value>,
    pub dispatched: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingGap {
    pub audio: AudioRange,
    pub reason: String,
    pub recoverable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureParameters {
    #[serde(default = "default_max_segment_ms")]
    pub max_segment_ms: u64,
    pub configured_source_language: String,
    pub target_languages: Vec<String>,
    pub direction_epoch: u64,
    pub recording_enabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveSegment {
    pub segment_id: Uuid,
    pub start_sample: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingManifest {
    #[serde(default)]
    pub direction_boundaries: Vec<RecordedDirectionBoundary>,
    pub schema_version: u32,
    pub track: TrackSpec,
    pub recording_id: Uuid,
    pub closed: bool,
    pub final_sample: u64,
    #[serde(default)]
    pub capture_parameters: Option<CaptureParameters>,
    #[serde(default)]
    pub classified_sample: u64,
    #[serde(default)]
    pub active_segment: Option<ActiveSegment>,
    pub chunks: Vec<RecordingChunk>,
    pub segments: Vec<RecordedSegment>,
    pub gaps: Vec<RecordingGap>,
    pub capture_seal_event: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedDirectionBoundary {
    pub start_sample: u64,
    pub parameters: CaptureParameters,
    pub event: serde_json::Value,
    pub acknowledged: bool,
}
fn default_max_segment_ms() -> u64 {
    10000
}
pub struct RecordingJournal {
    directory: PathBuf,
    manifest: RecordingManifest,
}
impl RecordingJournal {
    pub fn create(directory: PathBuf, track: TrackSpec) -> anyhow::Result<Self> {
        track.validate()?;
        ensure!(
            track.sample_rate == 16000,
            "recording processing clock must be 16kHz mono"
        );
        private_directory(&directory)?;
        ensure!(
            !directory.join("manifest.json").exists(),
            "recording journal already exists; recover it explicitly"
        );
        let journal = Self {
            directory,
            manifest: RecordingManifest {
                direction_boundaries: Vec::new(),
                schema_version: 1,
                track,
                recording_id: Uuid::new_v4(),
                closed: false,
                final_sample: 0,
                capture_parameters: None,
                classified_sample: 0,
                active_segment: None,
                chunks: Vec::new(),
                segments: Vec::new(),
                gaps: Vec::new(),
                capture_seal_event: None,
            },
        };
        journal.persist()?;
        Ok(journal)
    }
    pub fn reopen(directory: PathBuf) -> anyhow::Result<Self> {
        private_directory(&directory)?;
        let manifest: RecordingManifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json"))?)?;
        ensure!(
            manifest.schema_version == 1,
            "unsupported recording manifest"
        );
        manifest.track.validate()?;
        Ok(Self {
            directory,
            manifest,
        })
    }
    pub fn manifest(&self) -> &RecordingManifest {
        &self.manifest
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    fn persist(&self) -> anyhow::Result<()> {
        atomic_write(
            &self.directory.join("manifest.json"),
            &serde_json::to_vec(&self.manifest)?,
        )
    }

    pub fn configure(&mut self, parameters: CaptureParameters) -> anyhow::Result<()> {
        ensure!(
            !parameters.configured_source_language.is_empty()
                && parameters.direction_epoch > 0
                && (1000..=30000).contains(&parameters.max_segment_ms),
            "invalid capture parameters"
        );
        if let Some(existing) = &self.manifest.capture_parameters {
            ensure!(
                *existing == parameters,
                "recovery capture parameters differ from recorded scope"
            );
            return Ok(());
        }
        self.manifest.capture_parameters = Some(parameters);
        self.persist()
    }
    pub fn checkpoint(
        &mut self,
        classified_sample: u64,
        active: Option<ActiveSegment>,
    ) -> anyhow::Result<()> {
        ensure!(
            classified_sample >= self.manifest.classified_sample
                && classified_sample <= self.manifest.final_sample,
            "invalid VAD checkpoint"
        );
        if let Some(active) = &active {
            ensure!(
                !active.segment_id.is_nil() && active.start_sample <= classified_sample,
                "invalid active segment checkpoint"
            );
        }
        self.manifest.classified_sample = classified_sample;
        self.manifest.active_segment = active;
        self.persist()
    }
    pub fn load_range(&self, start: u64, end: u64) -> anyhow::Result<Vec<f32>> {
        ensure!(
            start <= end && end <= self.manifest.final_sample && end - start <= 480_000,
            "recovery range exceeds 30 seconds"
        );
        let mut result = Vec::with_capacity((end - start) as usize);
        let mut cursor = start;
        for chunk in &self.manifest.chunks {
            if chunk.end_sample <= cursor || chunk.start_sample >= end {
                continue;
            }
            ensure!(
                chunk.start_sample <= cursor,
                "recording PCM has an unrecoverable gap"
            );
            ensure!(
                chunk.file == format!("chunk-{}.f32le", chunk.chunk_id),
                "invalid chunk path"
            );
            let path = self.directory.join(&chunk.file);
            ensure!(
                !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "recording chunk cannot be a symlink"
            );
            let bytes = fs::read(path)?;
            ensure!(
                bytes.len() as u64 == (chunk.end_sample - chunk.start_sample) * 4
                    && format!("{:x}", Sha256::digest(&bytes)) == chunk.sha256,
                "recording chunk hash/size mismatch"
            );
            let finish = chunk.end_sample.min(end);
            let offset = ((cursor - chunk.start_sample) * 4) as usize;
            let count = ((finish - cursor) * 4) as usize;
            result.extend(
                bytes[offset..offset + count]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap())),
            );
            cursor = finish;
        }
        ensure!(
            cursor == end,
            "recording disabled or missing PCM: unclosed tail cannot be recovered"
        );
        Ok(result)
    }
    pub fn save_direction_boundary(
        &mut self,
        boundary: RecordedDirectionBoundary,
    ) -> anyhow::Result<()> {
        ensure!(
            !self.manifest.closed && boundary.start_sample == self.manifest.final_sample,
            "direction boundary must follow durable captured samples"
        );
        let previous = self
            .manifest
            .direction_boundaries
            .last()
            .map(|b| b.parameters.direction_epoch)
            .or_else(|| {
                self.manifest
                    .capture_parameters
                    .as_ref()
                    .map(|p| p.direction_epoch)
            })
            .context("missing initial direction")?;
        ensure!(
            previous.checked_add(1) == Some(boundary.parameters.direction_epoch),
            "direction epoch is not consecutive"
        );
        self.manifest.direction_boundaries.push(boundary);
        self.persist()
    }
    pub fn acknowledge_direction(&mut self, epoch: u64) -> anyhow::Result<()> {
        self.manifest
            .direction_boundaries
            .iter_mut()
            .find(|b| b.parameters.direction_epoch == epoch)
            .context("unknown direction boundary")?
            .acknowledged = true;
        self.persist()
    }
    pub fn append_pcm(&mut self, samples: &[f32]) -> anyhow::Result<(u64, u64)> {
        ensure!(
            !self.manifest.closed && !samples.is_empty() && samples.len() <= 480_000,
            "invalid recording append"
        );
        ensure!(
            samples
                .iter()
                .all(|v| v.is_finite() && (-1.0..=1.0).contains(v)),
            "invalid PCM sample"
        );
        let start = self.manifest.final_sample;
        let end = start
            .checked_add(samples.len() as u64)
            .context("sample overflow")?;
        let chunk_id = Uuid::new_v4();
        let file = format!("chunk-{chunk_id}.f32le");
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        atomic_write(&self.directory.join(&file), &bytes)?;
        self.manifest.chunks.push(RecordingChunk {
            chunk_id,
            file,
            start_sample: start,
            end_sample: end,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        });
        self.manifest.final_sample = end;
        self.persist()?;
        Ok((start, end))
    }
    /// Persist the exact ASR input before the capture-closed event can be acked.
    pub fn close_segment(
        &mut self,
        metadata: &crate::audio::SegmentMeta,
        samples: &[f32],
        recording_enabled: bool,
    ) -> anyhow::Result<Option<String>> {
        metadata.validate(samples.len())?;
        let segment_id = metadata.segment_id;
        let audio = metadata.audio.clone();
        audio.validate()?;
        ensure!(
            !self.manifest.closed
                && audio.sample_rate == 16000
                && audio.end_sample <= self.manifest.final_sample
                && audio.end_sample - audio.start_sample == samples.len() as u64,
            "segment/audio clock mismatch"
        );
        ensure!(
            !segment_id.is_nil()
                && !self
                    .manifest
                    .segments
                    .iter()
                    .any(|s| s.segment_id == segment_id),
            "duplicate segment"
        );
        let file = format!("segment-{segment_id}.f32le");
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        if recording_enabled {
            atomic_write(&self.directory.join(&file), &bytes)?;
        }
        self.manifest.segments.push(RecordedSegment {
            segment_id,
            audio,
            metadata: metadata.clone(),
            file: recording_enabled.then_some(file),
            sha256: recording_enabled.then(|| format!("{:x}", Sha256::digest(&bytes))),
            capture_event: None,
            dispatched: false,
        });
        self.persist()?;
        Ok(recording_enabled.then(|| {
            format!(
                "recording:{}:segment:{segment_id}",
                self.manifest.recording_id
            )
        }))
    }
    pub fn advance_without_recording(&mut self, samples: usize) -> anyhow::Result<(u64, u64)> {
        ensure!(!self.manifest.closed, "recording journal is sealed");
        let start = self.manifest.final_sample;
        self.manifest.final_sample = start
            .checked_add(samples as u64)
            .context("sample overflow")?;
        self.persist()?;
        Ok((start, self.manifest.final_sample))
    }
    pub fn save_capture_event(
        &mut self,
        segment_id: Uuid,
        event: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.manifest
            .segments
            .iter_mut()
            .find(|s| s.segment_id == segment_id)
            .context("unknown recording segment")?
            .capture_event = Some(event);
        self.persist()
    }
    pub fn mark_dispatched(&mut self, segment_id: Uuid) -> anyhow::Result<()> {
        self.manifest
            .segments
            .iter_mut()
            .find(|s| s.segment_id == segment_id)
            .context("unknown recording segment")?
            .dispatched = true;
        self.persist()
    }
    pub fn add_gap(&mut self, gap: RecordingGap) -> anyhow::Result<()> {
        gap.audio.validate()?;
        ensure!(!gap.reason.trim().is_empty(), "gap requires reason");
        self.manifest.gaps.push(gap);
        self.persist()
    }
    pub fn save_capture_seal_event(&mut self, event: serde_json::Value) -> anyhow::Result<()> {
        self.manifest.capture_seal_event = Some(event);
        self.persist()
    }
    pub fn seal(&mut self) -> anyhow::Result<()> {
        self.manifest.active_segment = None;
        self.manifest.classified_sample = self.manifest.final_sample;
        self.manifest.closed = true;
        self.persist()
    }
    pub fn load_segment(&self, segment_id: Uuid) -> anyhow::Result<Vec<f32>> {
        let segment = self
            .manifest
            .segments
            .iter()
            .find(|s| s.segment_id == segment_id)
            .context("unknown recorded segment")?;
        let file = segment
            .file
            .as_ref()
            .context("recording disabled: PCM cannot be recovered")?;
        ensure!(
            *file == format!("segment-{segment_id}.f32le"),
            "invalid recording reference"
        );
        let bytes = fs::read(self.directory.join(file))?;
        ensure!(
            bytes.len() % 4 == 0 && Some(format!("{:x}", Sha256::digest(&bytes))) == segment.sha256,
            "recording hash mismatch"
        );
        Ok(bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    }
}
