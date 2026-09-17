//! Reliable capture boundary shared by hardware capture and file-only replay.
use anyhow::{ensure, Context, Result};
use forum_contracts::{
    capture_manifest_sha256, AudioRange, CaptureSegmentClosed, CaptureStopped, EventType, Revision,
    TrackSeal, TrackSpec, Uuid,
};
use forum_runtime::{
    audio::SegmentMeta, recording::RecordingJournal, DurableProducer, RuntimeConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureContext {
    #[serde(default = "default_max_segment_ms")]
    pub max_segment_ms: u64,
    pub runtime: RuntimeConfig,
    pub track: TrackSpec,
    /// A second independent source, sharing this meeting's processing clock.
    #[serde(default)]
    pub secondary_track: Option<TrackSpec>,
    pub recording_dir: PathBuf,
    pub recording_enabled: bool,
    pub replay_only: bool,
    pub configured_source_language: String,
    pub target_languages: Vec<String>,
    pub direction_epoch: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureProgress {
    pub started: bool,
    pub devices_released: bool,
    pub capture_sealed: bool,
    pub final_sample: u64,
    pub segments_closed: usize,
    pub outbox_pending: usize,
    pub error: Option<String>,
}
fn default_max_segment_ms() -> u64 {
    10000
}
pub struct CaptureSession {
    context: CaptureContext,
    producer: DurableProducer,
    journal: RecordingJournal,
    active: Option<(Uuid, u64)>,
    partial_seq: u64,
    buffered_pcm: Vec<f32>,
    observed_samples: u64,
}
impl CaptureSession {
    pub fn open(context: CaptureContext) -> Result<Self> {
        ensure!(
            context.track.session_id == context.runtime.session.session_id
                && context.track.sample_rate == 16000,
            "capture scope/processing clock mismatch"
        );
        ensure!(context.direction_epoch > 0, "capture direction required");
        ensure!(
            (1000..=30000).contains(&context.max_segment_ms),
            "segment duration must be 1..30 seconds"
        );
        let producer_name=if context.secondary_track.is_some(){format!("capture-{}",context.track.track_id)}else{"capture".into()};
        let producer = DurableProducer::open(context.runtime.for_producer(&producer_name)?)?;
        let recording_dir=if context.secondary_track.is_some(){context.recording_dir.join("tracks").join(context.track.track_id.to_string())}else{context.recording_dir.clone()};
        let mut journal = if context.replay_only {
            RecordingJournal::reopen(recording_dir)?
        } else {
            RecordingJournal::create(recording_dir, context.track.clone())?
        };
        ensure!(
            journal.manifest().track == context.track,
            "recording track scope mismatch"
        );
        journal.configure(forum_runtime::recording::CaptureParameters {
            max_segment_ms: context.max_segment_ms,
            configured_source_language: context.configured_source_language.clone(),
            target_languages: context.target_languages.clone(),
            direction_epoch: context.direction_epoch,
            recording_enabled: context.recording_enabled,
        })?;
        let observed_samples = journal.manifest().final_sample;
        Ok(Self {
            context,
            producer,
            journal,
            active: None,
            partial_seq: 0,
            buffered_pcm: Vec::new(),
            observed_samples,
        })
    }
    pub fn context(&self) -> &CaptureContext {
        &self.context
    }
    pub fn cursor(&self) -> u64 {
        self.observed_samples
    }
    pub fn models_ready(&self) -> Result<bool> {
        let status = self.producer.client().call(
            "status",
            json!({"session_id":self.context.track.session_id}),
        )?;
        let roles = status["ready_roles"]
            .as_array()
            .context("runtime status has no ready_roles")?;
        Ok(["asr"]
            .iter()
            .all(|role| roles.iter().any(|value| value == role)))
    }
    pub fn record(&mut self, pcm: &[f32]) -> Result<()> {
        ensure!(
            pcm.iter()
                .all(|sample| sample.is_finite() && (-1.0..=1.0).contains(sample)),
            "invalid input PCM"
        );
        self.observed_samples = self
            .observed_samples
            .checked_add(pcm.len() as u64)
            .context("capture clock overflow")?;
        self.buffered_pcm.extend_from_slice(pcm);
        // Each drained callback batch is journaled before VAD or external dispatch.
        // A crash may lose still-in-device callbacks, never silently claim them saved.
        self.flush_pcm()
    }
    pub fn checkpoint_segmenter(&mut self, segmenter: &PcmSegmenter) -> Result<()> {
        self.journal.checkpoint(
            segmenter.cursor,
            segmenter
                .active
                .as_ref()
                .map(|a| forum_runtime::recording::ActiveSegment {
                    segment_id: a.segment_id,
                    start_sample: a.start_sample,
                }),
        )
    }
    pub fn use_segment_identity(&mut self, id: Uuid, start: u64) -> Result<()> {
        ensure!(
            !id.is_nil() && start <= self.cursor(),
            "invalid capture segment identity"
        );
        self.active = Some((id, start));
        self.partial_seq = 0;
        Ok(())
    }
    fn flush_pcm(&mut self) -> Result<()> {
        if !self.buffered_pcm.is_empty() {
            let buffered = std::mem::take(&mut self.buffered_pcm);
            for chunk in buffered.chunks(16000) {
                if self.context.recording_enabled {
                    self.journal.append_pcm(chunk)?;
                } else {
                    self.journal.advance_without_recording(chunk.len())?;
                }
            }
        }
        Ok(())
    }
    pub fn record_gap(&mut self, count: u64, reason: &str) -> Result<()> {
        ensure!(
            count > 0 && count <= i64::MAX as u64,
            "invalid missing sample count"
        );
        self.flush_pcm()?;
        let start = self.cursor();
        self.observed_samples = start.checked_add(count).context("gap sample overflow")?;
        self.journal.advance_without_recording(count as usize)?;
        let audio = AudioRange {
            start_sample: start,
            end_sample: self.cursor(),
            sample_rate: 16000,
            start_ms: start / 16,
            end_ms: self.cursor() / 16,
        };
        self.journal
            .add_gap(forum_runtime::recording::RecordingGap {
                audio: audio.clone(),
                reason: reason.into(),
                recoverable: false,
            })?;
        let gap = forum_contracts::AudioGap {
            gap_id: Uuid::new_v4(),
            track_id: self.context.track.track_id,
            audio,
            reason: reason.into(),
            recoverable: false,
        };
        let pending = self.producer.append(EventType::AudioGap, &gap)?;
        self.producer.flush_one(pending.message_id)?;
        Ok(())
    }
    pub fn begin_segment(&mut self, buffer_len: usize) -> Result<()> {
        ensure!(
            self.active.is_none() && buffer_len as u64 <= self.cursor(),
            "overlapping capture segment"
        );
        self.active = Some((Uuid::new_v4(), self.cursor() - buffer_len as u64));
        self.partial_seq = 0;
        Ok(())
    }
    pub fn metadata(&mut self, pcm_len: usize, final_segment: bool) -> Result<SegmentMeta> {
        if self.active.is_none() {
            self.begin_segment(pcm_len)?;
        }
        let (id, start) = self.active.unwrap();
        let end = start
            .checked_add(pcm_len as u64)
            .context("segment range overflow")?;
        ensure!(end <= self.cursor(), "segment exceeds captured samples");
        self.partial_seq += 1;
        let metadata = SegmentMeta {
            session_id: self.context.track.session_id,
            track_id: self.context.track.track_id,
            segment_id: id,
            revision: Revision::FIRST,
            audio: AudioRange {
                start_sample: start,
                end_sample: end,
                sample_rate: 16000,
                start_ms: start / 16,
                end_ms: end / 16,
            },
            configured_source_language: self.context.configured_source_language.clone(),
            target_languages: self.context.target_languages.clone(),
            direction_epoch: self.context.direction_epoch,
            partial_seq: self.partial_seq,
            final_segment,
        };
        metadata.validate(pcm_len)?;
        Ok(metadata)
    }
    /// Return only after the final PCM/manifest and capture registration are durable.
    pub fn close_segment(&mut self, pcm: &[f32]) -> Result<SegmentMeta> {
        ensure!(
            self.models_ready()?,
            "ASR not ready; final PCM cannot be dispatched"
        );
        let metadata = self.metadata(pcm.len(), true)?;
        self.flush_pcm()?;
        let reference =
            self.journal
                .close_segment(&metadata, pcm, self.context.recording_enabled)?;
        let event = CaptureSegmentClosed {
            track_id: metadata.track_id,
            segment_id: metadata.segment_id,
            audio: metadata.audio.clone(),
            recording_ref: reference,
        };
        let pending = self
            .producer
            .append(EventType::AudioSegmentClosed, &event)?;
        self.journal
            .save_capture_event(metadata.segment_id, pending.event)?;
        self.active = None;
        self.producer.flush_one(pending.message_id)?;
        Ok(metadata)
    }
    /// Close/ACK every old segment before calling. The persisted event is replayed
    /// after a crash before any later PCM is interpreted under the new direction.
    pub fn change_direction(
        &mut self,
        source: String,
        targets: Vec<String>,
        epoch: u64,
    ) -> Result<()> {
        ensure!(
            !self.context.replay_only && self.active.is_none(),
            "direction change requires a live segment boundary"
        );
        ensure!(
            self.context.configured_source_language != "auto"
                && self.context.target_languages.len() == 1
                && targets.len() == 1,
            "automatic/bilingual capture direction cannot be swapped"
        );
        ensure!(
            self.context.direction_epoch.checked_add(1) == Some(epoch),
            "direction epoch must increase exactly once"
        );
        ensure!(!source.is_empty(), "source language required");
        self.flush_pcm()?;
        let payload = forum_contracts::DirectionChanged {
            expected_epoch: self.context.direction_epoch,
            direction_epoch: epoch,
            target_languages: targets.clone(),
            boundary: Some(forum_contracts::DirectionBoundary {
                track_id: self.context.track.track_id,
                start_sample: self.cursor(),
                configured_source_language: source.clone(),
            }),
        };
        let pending = self
            .producer
            .append(EventType::DirectionChanged, &payload)?;
        let parameters = forum_runtime::recording::CaptureParameters {
            max_segment_ms: self.context.max_segment_ms,
            configured_source_language: source,
            target_languages: targets,
            direction_epoch: epoch,
            recording_enabled: self.context.recording_enabled,
        };
        self.journal.save_direction_boundary(
            forum_runtime::recording::RecordedDirectionBoundary {
                start_sample: self.cursor(),
                parameters: parameters.clone(),
                event: pending.event,
                acknowledged: false,
            },
        )?;
        self.producer.flush_one(pending.message_id)?;
        self.journal.acknowledge_direction(epoch)?;
        self.apply_direction(&parameters);
        Ok(())
    }
    fn apply_direction(&mut self, parameters: &forum_runtime::recording::CaptureParameters) {
        self.context.configured_source_language = parameters.configured_source_language.clone();
        self.context.target_languages = parameters.target_languages.clone();
        self.context.direction_epoch = parameters.direction_epoch;
    }
    fn replay_directions(&mut self) -> Result<()> {
        for boundary in self.journal.manifest().direction_boundaries.clone() {
            self.producer
                .client()
                .call("ingest", json!({"event":boundary.event}))?;
            if !boundary.acknowledged {
                self.journal
                    .acknowledge_direction(boundary.parameters.direction_epoch)?;
            }
            self.apply_direction(&boundary.parameters);
        }
        Ok(())
    }
    pub fn mark_dispatched(&mut self, id: Uuid) -> Result<()> {
        self.journal.mark_dispatched(id)
    }
    pub fn progress(&self) -> CaptureProgress {
        CaptureProgress {
            final_sample: self.cursor(),
            segments_closed: self.journal.manifest().segments.len(),
            outbox_pending: self.producer.pending().map(|p| p.len()).unwrap_or(1),
            ..Default::default()
        }
    }
    /// Caller must have stopped and dropped every capture device before calling.
    pub fn seal_after_devices_released(&mut self) -> Result<()> {
        ensure!(self.context.secondary_track.is_none(),"dual capture requires a joint seal");
        let track=self.seal_recording()?;
        self.publish_capture_seal(vec![track])
    }
    pub fn seal_recording(&mut self)->Result<TrackSeal>{
        self.flush_pcm()?;
        for (_, result) in self.producer.flush_pending() {
            result?;
        }
        self.journal.seal()?;
        Ok(TrackSeal {
            track_id: self.context.track.track_id,
            final_sample: self.cursor(),
            segment_ids: self
                .journal
                .manifest()
                .segments
                .iter()
                .map(|s| s.segment_id)
                .collect(),
        })
    }
    pub fn publish_capture_seal(&mut self,tracks:Vec<TrackSeal>)->Result<()> {
        let stopped = CaptureStopped {
            manifest_sha256: capture_manifest_sha256(&tracks)?,
            tracks,
        };
        let status = self.producer.client().call(
            "status",
            json!({"session_id":self.context.track.session_id}),
        )?;
        if status["capture_stopped"] == true {
            ensure!(
                status["capture_manifest_sha256"].as_str()
                    == Some(stopped.manifest_sha256.as_str()),
                "existing core capture seal differs from recording manifest"
            );
            return Ok(());
        }
        if let Some(event) = self.journal.manifest().capture_seal_event.clone() {
            self.producer
                .client()
                .call("ingest", json!({"event":event}))?;
        } else {
            let pending = self.producer.append(EventType::CaptureStopped, &stopped)?;
            self.journal.save_capture_seal_event(pending.event)?;
            self.producer.flush_one(pending.message_id)?;
        }
        Ok(())
    }
    fn recover_unclosed_tail(&mut self) -> Result<()> {
        let manifest = self.journal.manifest().clone();
        if manifest.closed {
            return Ok(());
        }
        let mut start = manifest
            .active_segment
            .as_ref()
            .map(|a| a.start_sample)
            .unwrap_or(manifest.classified_sample);
        // A crash between closing a segment and the VAD checkpoint must not create
        // an overlapping replacement for that already journaled identity.
        for segment in &manifest.segments {
            if segment.audio.end_sample > start {
                start = segment.audio.end_sample;
            }
        }
        if start >= manifest.final_sample {
            return Ok(());
        }
        let active = manifest.active_segment.filter(|a| a.start_sample == start);
        let pcm = self.journal.load_range(start, manifest.final_sample)?;
        let mut segmenter = PcmSegmenter::with_max_segment_ms(self.context.max_segment_ms)?;
        segmenter.cursor = start;
        if let Some(active) = active {
            segmenter.active = Some(ClosedAudio {
                segment_id: active.segment_id,
                start_sample: start,
                samples: Vec::new(),
            });
        }
        let mut closed = segmenter.push(&pcm);
        if let Some(tail) = segmenter.finish() {
            closed.push(tail);
        }
        for segment in closed {
            self.use_segment_identity(segment.segment_id, segment.start_sample)?;
            self.close_segment(&segment.samples)?;
        }
        self.checkpoint_segmenter(&segmenter)?;
        Ok(())
    }
    /// Missing PCM is an explicit error; recovery never silently reopens a device.
    pub fn replay_pending(
        &mut self,
        dispatch: impl FnMut(&SegmentMeta, &[f32]) -> Result<()>,
    ) -> Result<()> {
        let dispatched_revisions=self.replay_segments(dispatch)?;
        self.seal_after_devices_released()?;
        self.acknowledge_replay(dispatched_revisions)
    }
    pub fn replay_segments(&mut self,mut dispatch:impl FnMut(&SegmentMeta,&[f32])->Result<()>)->Result<Vec<serde_json::Value>> {
        ensure!(
            self.context.replay_only,
            "replay requires explicit recovery mode"
        );
        for (_, result) in self.producer.flush_pending() {
            result?;
        }
        self.replay_directions()?;
        self.recover_unclosed_tail()?;
        let segments = self.journal.manifest().segments.clone();
        // Register original identities before querying authoritative revisions.
        // A truncated status list must never imply revision 1.
        for segment in &segments {
            if let Some(event) = &segment.capture_event {
                self.producer
                    .client()
                    .call("ingest", json!({"event":event}))?;
            } else {
                let event = CaptureSegmentClosed {
                    track_id: self.context.track.track_id,
                    segment_id: segment.segment_id,
                    audio: segment.audio.clone(),
                    recording_ref: segment.file.as_ref().map(|_| {
                        format!(
                            "recording:{}:segment:{}",
                            self.journal.manifest().recording_id,
                            segment.segment_id
                        )
                    }),
                };
                let pending = self
                    .producer
                    .append(EventType::AudioSegmentClosed, &event)?;
                self.journal
                    .save_capture_event(segment.segment_id, pending.event)?;
                self.producer.flush_one(pending.message_id)?;
            }
        }
        let mut dispatched_revisions = Vec::new();
        for batch in segments.chunks(256) {
            let requested: Vec<_> = batch.iter().map(|s| s.segment_id).collect();
            let response = self.producer.client().call(
                "recovery_revisions",
                json!({"session_id":self.context.track.session_id,"segment_ids":requested}),
            )?;
            let entries = response
                .as_array()
                .context("recovery revision list missing")?;
            ensure!(
                entries.len() == batch.len(),
                "incomplete recovery revision response"
            );
            let mut revisions = std::collections::HashMap::new();
            for entry in entries {
                let id: Uuid = serde_json::from_value(entry["segment_id"].clone())?;
                ensure!(
                    requested.contains(&id) && !revisions.contains_key(&id),
                    "unexpected or duplicate recovery segment ID"
                );
                let terminal = entry["terminal"]
                    .as_bool()
                    .context("recovery terminal flag missing")?;
                let revision: Option<Revision> =
                    serde_json::from_value(entry["next_revision"].clone())?;
                ensure!(
                    terminal == revision.is_none(),
                    "inconsistent recovery terminal/revision"
                );
                revisions.insert(id, revision);
            }
            for segment in batch {
                let revision = revisions
                    .get(&segment.segment_id)
                    .context("missing recovery segment revision")?;
                let Some(revision) = revision else { continue };
                let pcm = self.journal.load_segment(segment.segment_id)?;
                let mut metadata = segment.metadata.clone();
                metadata.revision = *revision;
                dispatch(&metadata, &pcm)?;
                self.journal.mark_dispatched(segment.segment_id)?;
                dispatched_revisions
                    .push(json!({"segment_id":metadata.segment_id,"revision":metadata.revision}));
            }
        }
        Ok(dispatched_revisions)
    }
    pub fn acknowledge_replay(&self,dispatched_revisions:Vec<serde_json::Value>)->Result<()> {
        self.producer.client().call(
            "capture_dispatch_complete",
            json!({"session_id":self.context.track.session_id,"segments":dispatched_revisions}),
        )?;
        Ok(())
    }
}

/// Exact revision acknowledgement, including an explicit failed terminal result.
pub fn final_is_durable(
    client: &forum_runtime::RuntimeClient,
    metadata: &SegmentMeta,
) -> Result<bool> {
    let response = client.call(
        "recovery_revisions",
        json!({"session_id":metadata.session_id,"segment_ids":[metadata.segment_id]}),
    )?;
    let entries = response.as_array().context("final receipt list missing")?;
    ensure!(
        entries.len() == 1 && entries[0]["segment_id"] == json!(metadata.segment_id),
        "final receipt identity mismatch"
    );
    let terminal = entries[0]["terminal"]
        .as_bool()
        .context("final receipt terminal missing")?;
    let next: Option<Revision> = serde_json::from_value(entries[0]["next_revision"].clone())?;
    ensure!(
        terminal == next.is_none(),
        "final receipt terminal/revision mismatch"
    );
    Ok(terminal || next.is_some_and(|revision| revision.get() > metadata.revision.get()))
}
pub fn wait_for_final(
    client: &forum_runtime::RuntimeClient,
    metadata: &SegmentMeta,
    mut cancelled: impl FnMut() -> bool,
) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        ensure!(!cancelled(), "ASR drain cancelled");
        if final_is_durable(client, metadata)? {
            return Ok(());
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "ASR final acknowledgement timed out; PCM remains recoverable"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
/// One frame in Dora at a time; a bounded local backlog stays recoverable on disk.
/// The live capture loop never waits for model inference while holding a device.
#[derive(Default)]
pub struct DeliveryQueue {
    queued: std::collections::VecDeque<(SegmentMeta, Vec<f32>)>,
    in_flight: Option<(SegmentMeta, std::time::Instant)>,
    last_track: Option<Uuid>,
}
impl DeliveryQueue {
    pub fn enqueue(&mut self, metadata: SegmentMeta, pcm: Vec<f32>) -> Result<()> {
        ensure!(
            self.queued.len() + usize::from(self.in_flight.is_some()) < 8,
            "ASR backlog exceeds eight segments; saved PCM requires recovery"
        );
        self.queued.push_back((metadata, pcm));
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty() && self.in_flight.is_none()
    }
    pub fn pump(
        &mut self,
        capture: &mut CaptureSession,
        dispatch: impl FnMut(&SegmentMeta, &[f32]) -> Result<()>,
    ) -> Result<()> {
        self.pump_tracks(std::slice::from_mut(capture),dispatch)
    }
    pub fn pump_tracks(&mut self,captures:&mut [CaptureSession],mut dispatch:impl FnMut(&SegmentMeta,&[f32])->Result<()>)->Result<()> {
        ensure!(!captures.is_empty(),"capture tracks missing");
        if let Some((metadata, started)) = &self.in_flight {
            if final_is_durable(captures[0].producer.client(), metadata)? {
                self.in_flight = None;
            } else {
                ensure!(
                    started.elapsed() < std::time::Duration::from_secs(60),
                    "ASR final acknowledgement timed out; PCM remains recoverable"
                );
                return Ok(());
            }
        }
        // A busy system source cannot starve the microphone on the shared ASR.
        let next=self.queued.iter().position(|(meta,_)|Some(meta.track_id)!=self.last_track).unwrap_or(0);
        if let Some((metadata, pcm)) = self.queued.remove(next) {
            let capture=captures.iter_mut().find(|c|c.context.track.track_id==metadata.track_id).context("queued segment belongs to another track")?;
            dispatch(&metadata, &pcm)?;
            capture.mark_dispatched(metadata.segment_id)?;
            self.last_track=Some(metadata.track_id);
            self.in_flight = Some((metadata, std::time::Instant::now()));
        }
        Ok(())
    }
}

/// The same deterministic frame segmenter is used for live capture and PCM probes.
/// UUID is allocated at voiced onset and kept through the complete segment.
pub struct ClosedAudio {
    pub segment_id: Uuid,
    pub start_sample: u64,
    pub samples: Vec<f32>,
}
pub struct PcmSegmenter {
    cursor: u64,
    remainder: Vec<f32>,
    active: Option<ClosedAudio>,
    silence_frames: usize,
    max_samples: usize,
}
impl Default for PcmSegmenter {
    fn default() -> Self {
        Self {
            cursor: 0,
            remainder: Vec::new(),
            active: None,
            silence_frames: 0,
            max_samples: 160_000,
        }
    }
}
impl PcmSegmenter {
    pub fn with_max_segment_ms(ms: u64) -> Result<Self> {
        ensure!(
            (1000..=30000).contains(&ms),
            "segment duration must be 1..30 seconds"
        );
        Ok(Self {
            max_samples: (ms * 16) as usize,
            ..Self::default()
        })
    }
    pub fn push(&mut self, samples: &[f32]) -> Vec<ClosedAudio> {
        self.remainder.extend_from_slice(samples);
        let complete = self.remainder.len() / 160 * 160;
        let frames: Vec<f32> = self.remainder.drain(..complete).collect();
        let mut closed = Vec::new();
        for frame in frames.chunks_exact(160) {
            let voiced =
                frame.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / 160.0 > 0.012f64.powi(2);
            if self.active.is_none() && voiced {
                self.active = Some(ClosedAudio {
                    segment_id: Uuid::new_v4(),
                    start_sample: self.cursor,
                    samples: Vec::new(),
                });
            }
            if let Some(active) = self.active.as_mut() {
                active.samples.extend_from_slice(frame);
                self.silence_frames = if voiced { 0 } else { self.silence_frames + 1 };
                if self.silence_frames >= 30 || active.samples.len() >= self.max_samples {
                    closed.push(self.active.take().unwrap());
                    self.silence_frames = 0;
                }
            }
            self.cursor += 160;
        }
        closed
    }
    pub fn skip_samples(&mut self, count: u64) {
        self.cursor += self.remainder.len() as u64 + count;
        self.remainder.clear();
        self.silence_frames = 0;
    }
    pub fn finish(&mut self) -> Option<ClosedAudio> {
        if self.active.is_none() && self.remainder.iter().any(|sample| sample.abs() > 0.012) {
            self.active = Some(ClosedAudio {
                segment_id: Uuid::new_v4(),
                start_sample: self.cursor,
                samples: Vec::new(),
            });
        }
        self.cursor += self.remainder.len() as u64;
        if let Some(active) = self.active.as_mut() {
            active.samples.append(&mut self.remainder);
        } else {
            self.remainder.clear();
        }
        self.active.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forum_contracts::{
        Event, Producer, SessionSpec, SessionState, SessionTransition, TrackKind, SCHEMA_VERSION,
    };
    use forum_core::CoreHandle;
    use forum_runtime::{Endpoint, RpcError, UdsServer};
    use std::{
        fs,
        os::unix::fs::DirBuilderExt,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    struct Fixture {
        root: PathBuf,
        config: CaptureContext,
        core: CoreHandle,
        server: UdsServer,
        ready: Arc<AtomicBool>,
        reject_capture: Arc<AtomicBool>,
        host_run: Uuid,
        host_seq: u64,
    }
    impl Fixture {
        fn new(recording_enabled: bool) -> Self {
            Self::with_dual(recording_enabled,false)
        }
        fn with_dual(recording_enabled:bool,dual:bool)->Self {
            let root = PathBuf::from("/tmp").join(format!("f03-{}", Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let session = SessionSpec {
                session_id: Uuid::new_v4(),
                event_id: Uuid::new_v4(),
                room_id: Uuid::new_v4(),
                owner_device_id: Uuid::new_v4(),
                title: "synthetic PCM recovery".into(),
            };
            let track = TrackSpec {
                track_id: Uuid::new_v4(),
                session_id: session.session_id,
                kind: if dual{TrackKind::Mic}else{TrackKind::Replay},
                sample_rate: 16000,
            };
            let core = CoreHandle::open(root.join("core.sqlite"), 16).unwrap();
            let session_copy = session.clone();
            let track_copy = track.clone();
            let secondary=dual.then(||TrackSpec{track_id:Uuid::new_v4(),session_id:session.session_id,kind:TrackKind::System,sample_rate:16000});
            let secondary_copy=secondary.clone();
            core.call(move |store| {
                store.create_session(&session_copy)?;
                store.create_track(&track_copy)?;
                if let Some(other)=secondary_copy{store.create_track(&other)?;}
                Ok(())
            })
            .unwrap();
            let endpoint = Endpoint::new(root.join("core.sock"));
            let ready = Arc::new(AtomicBool::new(false));
            let readiness = ready.clone();
            let rejected = Arc::new(AtomicBool::new(false));
            let reject_capture = rejected.clone();
            let writer = core.clone();
            let sid = session.session_id;
            let tid = track.track_id;
            let other_id=secondary.as_ref().map(|t|t.track_id);
            let server=UdsServer::bind(endpoint.clone(),move|request|{
                match request.method.as_str(){
                    "ingest"=>{
                        if reject_capture.load(Ordering::Acquire)&&matches!(request.params["event"]["type"].as_str(),Some("audio.segment_closed"|"translation.direction_changed")){return Err(RpcError::new("CORE_UNAVAILABLE",true,"injected writer unavailability"));}
                        writer.ingest_json(request.params["event"].clone()).map(|r|serde_json::to_value(r).unwrap()).map_err(|e|RpcError::new(e.code(),e.retryable(),e))
                    },
                    "status"=>{ let is_ready=readiness.load(Ordering::Acquire); writer.call(move|store|{
                        let status=store.session_status(sid)?;
                        let mut registered=store.registered_segment_ids(sid,tid)?;
                        if let Some(other)=other_id{registered.extend(store.registered_segment_ids(sid,other)?);}
                        Ok(json!({"ready_roles":if is_ready{vec!["asr"]}else{vec![]},"capture_stopped":status.capture_stopped,"capture_manifest_sha256":store.capture_seal(sid)?.map(|s|s.manifest_sha256),"terminal_segment_ids":store.terminal_segment_ids(sid)?,"registered_segment_ids":registered,"recovery_segments":store.recovery_segments(sid,100)?}))
                    }).map_err(|e|RpcError::new(e.code(),e.retryable(),e))},
                    "recovery_revisions"=>{
                        let ids:Vec<Uuid>=serde_json::from_value(request.params["segment_ids"].clone()).map_err(|e|RpcError::new("INVALID_PARAMS",false,e))?;
                        writer.call(move|store|Ok(serde_json::to_value(store.recovery_revisions(sid,ids)?)?)).map_err(|e|RpcError::new(e.code(),e.retryable(),e))
                    },
                    "capture_dispatch_complete"=>Ok(json!({"accepted":true})),
                    _=>Err(RpcError::new("UNKNOWN_METHOD",false,"test fixture")),
                }
            }).unwrap();
            let config = CaptureContext {
                secondary_track: secondary,
                max_segment_ms: 10000,
                runtime: RuntimeConfig {
                    endpoint,
                    session,
                    producer_dir: root.join("producers"),
                    producer_name: "node".into(),
                },
                track,
                recording_dir: root.join("recording"),
                recording_enabled,
                replay_only: false,
                configured_source_language: "auto".into(),
                target_languages: vec!["en".into()],
                direction_epoch: 1,
            };
            let mut fixture = Self {
                root,
                config,
                core,
                server,
                ready,
                reject_capture: rejected,
                host_run: Uuid::new_v4(),
                host_seq: 0,
            };
            fixture.transition(SessionState::Created, SessionState::Preparing);
            fixture.transition(SessionState::Preparing, SessionState::Ready);
            fixture.transition(SessionState::Ready, SessionState::Recording);
            fixture
        }
        fn transition(&mut self, from: SessionState, to: SessionState) {
            self.host_seq += 1;
            let s = &self.config.runtime.session;
            let event = Event {
                schema_version: SCHEMA_VERSION,
                message_id: Uuid::new_v4(),
                event_type: EventType::SessionChanged,
                event_id: s.event_id,
                room_id: s.room_id,
                session_id: s.session_id,
                producer: Producer {
                    name: "host-test".into(),
                    run_id: self.host_run,
                    seq: self.host_seq,
                },
                payload: SessionTransition {
                    expected_state: from,
                    next_state: to,
                    reason: "test transition".into(),
                },
            };
            self.core
                .ingest_json(serde_json::to_value(event).unwrap())
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.server.stop();
            let _ = self.core.shutdown();
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn first_frame_is_not_dispatched_before_asr_ready_and_translator_is_not_required() {
        let fixture = Fixture::new(true);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 480];
        capture.record(&pcm).unwrap();
        assert!(!capture.models_ready().unwrap());
        assert!(capture.close_segment(&pcm).is_err());
        let sid = fixture.config.track.session_id;
        assert!(fixture
            .core
            .call(move |s| s.session_snapshot(sid))
            .unwrap()
            .incomplete
            .is_empty());
        fixture.ready.store(true, Ordering::Release);
        let meta = capture.close_segment(&pcm).unwrap();
        assert_eq!(meta.audio.end_sample, 480);
        assert_eq!(
            fixture
                .core
                .call(move |s| s.session_snapshot(sid))
                .unwrap()
                .incomplete
                .len(),
            1
        );
    }
    #[test]
    fn dual_tracks_seal_together_and_replay_exact_processed_tail_with_independent_ids(){
        let mut fixture=Fixture::with_dual(true,true);fixture.ready.store(true,Ordering::Release);
        let mut dual=crate::dual_capture::DualCapture::open(fixture.config.clone()).unwrap();
        dual.process(0,&vec![0.4;481],&vec![0.2;481],0).unwrap();
        dual.process(1,&vec![0.3;481],&vec![0.3;481],0).unwrap();
        assert!(dual.captures[0].seal_after_devices_released().is_err());
        fixture.transition(SessionState::Recording,SessionState::Stopping);
        dual.seal_at(481).unwrap();
        let sid=fixture.config.track.session_id;
        let before=fixture.core.call(move|s|s.capture_seal(sid)).unwrap().unwrap();
        assert_eq!(before.tracks.len(),2);
        assert!(before.tracks.iter().all(|t|t.final_sample==481 && t.segment_ids.len()==1));
        assert_ne!(before.tracks[0].segment_ids,before.tracks[1].segment_ids);
        drop(dual);
        let mut config=fixture.config.clone();config.replay_only=true;
        let mut replay=crate::dual_capture::DualCapture::open(config).unwrap();let mut delivered=vec![];
        replay.replay(|meta,pcm|{delivered.push((meta.clone(),pcm.to_vec()));Ok(())}).unwrap();
        assert_eq!(delivered.len(),2);
        assert!(delivered.iter().all(|(meta,_)|meta.audio.start_sample==0 && meta.audio.end_sample==481));
        assert_eq!(delivered.iter().find(|(m,_)|m.track_id==fixture.config.track.track_id).unwrap().1,vec![0.2;481]);
        let after=fixture.core.call(move|s|s.capture_seal(sid)).unwrap().unwrap();assert_eq!(before.manifest_sha256,after.manifest_sha256);
    }
    #[test]
    fn missing_dual_callback_tail_is_a_gap_not_verified_silence(){
        let mut fixture=Fixture::with_dual(true,true);fixture.ready.store(true,Ordering::Release);
        let mut dual=crate::dual_capture::DualCapture::open(fixture.config.clone()).unwrap();
        dual.process(0,&vec![0.2;480],&vec![0.2;480],0).unwrap();
        fixture.transition(SessionState::Recording,SessionState::Stopping);
        dual.seal_at(800).unwrap();let sid=fixture.config.track.session_id;
        let gaps=fixture.core.call(move|s|s.audio_gaps(sid)).unwrap();assert_eq!(gaps.len(),2);
        assert!(gaps.iter().all(|g|!g.recoverable && g.audio.end_sample==800));
        assert!(gaps.iter().any(|g|g.audio.start_sample==480));assert!(gaps.iter().any(|g|g.audio.start_sample==0));
    }
    #[test]
    fn dual_shared_asr_is_fair_after_each_durable_ack(){
        let mut fixture=Fixture::with_dual(true,true);fixture.config.max_segment_ms=1000;fixture.ready.store(true,Ordering::Release);
        let mut dual=crate::dual_capture::DualCapture::open(fixture.config.clone()).unwrap();
        dual.process(1,&vec![0.3;48000],&vec![0.3;48000],0).unwrap();
        dual.process(0,&vec![0.2;16000],&vec![0.2;16000],0).unwrap();
        let mut asr=DurableProducer::open(fixture.config.runtime.for_producer("test-asr").unwrap()).unwrap();let mut tracks=vec![];
        while !dual.empty(){dual.pump(|meta,_|{
            tracks.push(meta.track_id);
            let result=forum_contracts::TranscriptFinal{track_id:meta.track_id,segment_id:meta.segment_id,revision:meta.revision,audio:meta.audio.clone(),text:"synthetic".into(),configured_source_language:meta.configured_source_language.clone(),detected_language:None,target_languages:meta.target_languages.clone(),direction_epoch:meta.direction_epoch,speaker_id:None,status:forum_contracts::TranscriptStatus::Success,reason:None,backend:"synthetic".into(),model_manifest_id:"no-model".into()};
            let pending=asr.append(EventType::TranscriptFinal,&result)?;asr.flush_one(pending.message_id)?;Ok(())
        }).unwrap();}
        assert_eq!(tracks.len(),4);assert_eq!(tracks[0],fixture.config.secondary_track.as_ref().unwrap().track_id);assert_eq!(tracks[1],fixture.config.track.track_id);
    }
    #[test]
    fn short_stop_tail_has_stable_identity_and_final_sample_range() {
        let fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let mut segmenter = PcmSegmenter::default();
        let pcm = vec![0.2; 481];
        capture.record(&pcm).unwrap();
        assert!(segmenter.push(&pcm).is_empty());
        let tail = segmenter.finish().unwrap();
        let id = tail.segment_id;
        capture.use_segment_identity(id, tail.start_sample).unwrap();
        let meta = capture.close_segment(&tail.samples).unwrap();
        assert_eq!(meta.segment_id, id);
        assert_eq!(meta.audio.end_sample, 481);
        assert_eq!(meta.audio.start_sample, 0);
    }
    #[test]
    fn unavailable_core_keeps_pcm_and_event_then_replays_exact_segment() {
        let mut fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        fixture.reject_capture.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 3200];
        capture.record(&pcm).unwrap();
        assert!(capture.close_segment(&pcm).is_err());
        let manifest = RecordingJournal::reopen(fixture.config.recording_dir.clone()).unwrap();
        assert_eq!(manifest.manifest().segments.len(), 1);
        let original = manifest.manifest().segments[0].clone();
        assert_eq!(manifest.load_segment(original.segment_id).unwrap(), pcm);
        assert_eq!(capture.progress().outbox_pending, 1);
        drop(capture);
        fixture.reject_capture.store(false, Ordering::Release);
        fixture.transition(SessionState::Recording, SessionState::Interrupted);
        let mut config = fixture.config.clone();
        config.replay_only = true;
        let mut replay = CaptureSession::open(config).unwrap();
        let mut delivered = Vec::new();
        replay
            .replay_pending(|metadata, pcm| {
                delivered.push((metadata.clone(), pcm.to_vec()));
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0.segment_id, original.segment_id);
        assert_eq!(delivered[0].1, pcm);
        let sid = fixture.config.track.session_id;
        assert_eq!(
            fixture
                .core
                .call(move |s| s.session_snapshot(sid))
                .unwrap()
                .incomplete
                .len(),
            1
        );
        assert_eq!(replay.progress().outbox_pending, 0);
    }
    #[test]
    fn recording_disabled_registers_expected_segment_but_never_claims_recoverable_pcm() {
        let fixture = Fixture::new(false);
        fixture.ready.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 1600];
        capture.record(&pcm).unwrap();
        let meta = capture.close_segment(&pcm).unwrap();
        let journal = RecordingJournal::reopen(fixture.config.recording_dir.clone()).unwrap();
        assert!(journal.load_segment(meta.segment_id).is_err());
        assert!(journal.manifest().chunks.is_empty());
        let sid = fixture.config.track.session_id;
        let missing = fixture
            .core
            .call(move |s| s.session_snapshot(sid))
            .unwrap()
            .incomplete;
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].recording_ref, None);
    }
    #[test]
    fn overflow_records_gap_and_never_compresses_processing_sample_clock() {
        let fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        capture.record(&vec![0.0; 1600]).unwrap();
        capture.record_gap(800, "capture_buffer_overflow").unwrap();
        capture.record(&vec![0.2; 1600]).unwrap();
        capture.use_segment_identity(Uuid::new_v4(), 2400).unwrap();
        let meta = capture.close_segment(&vec![0.2; 1600]).unwrap();
        assert_eq!(meta.audio.start_sample, 2400);
        assert_eq!(meta.audio.end_sample, 4000);
        let sid = fixture.config.track.session_id;
        let gaps = fixture.core.call(move |s| s.audio_gaps(sid)).unwrap();
        assert_eq!(gaps.len(), 1);
        assert!(!gaps[0].recoverable);
        assert_eq!(gaps[0].audio.start_sample, 1600);
        assert_eq!(gaps[0].audio.end_sample, 2400);
    }
    #[test]
    fn unclosed_active_pcm_recovers_with_same_vad_identity_after_crash() {
        let mut fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 1600];
        let mut segmenter = PcmSegmenter::default();
        capture.record(&pcm).unwrap();
        assert!(segmenter.push(&pcm).is_empty());
        let original_id = segmenter.active.as_ref().unwrap().segment_id;
        capture.checkpoint_segmenter(&segmenter).unwrap();
        drop(capture);
        fixture.transition(SessionState::Recording, SessionState::Interrupted);
        let mut config = fixture.config.clone();
        config.replay_only = true;
        let mut capture = CaptureSession::open(config).unwrap();
        let mut outputs = Vec::new();
        capture
            .replay_pending(|metadata, pcm| {
                outputs.push((metadata.clone(), pcm.to_vec()));
                Ok(())
            })
            .unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0.segment_id, original_id);
        assert_eq!(outputs[0].1, pcm);
    }
    #[test]
    fn recording_off_unclosed_tail_fails_recovery_and_does_not_seal() {
        let mut fixture = Fixture::new(false);
        fixture.ready.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 1600];
        let mut segmenter = PcmSegmenter::default();
        capture.record(&pcm).unwrap();
        segmenter.push(&pcm);
        capture.checkpoint_segmenter(&segmenter).unwrap();
        drop(capture);
        fixture.transition(SessionState::Recording, SessionState::Interrupted);
        let mut config = fixture.config.clone();
        config.replay_only = true;
        let mut capture = CaptureSession::open(config).unwrap();
        assert!(capture
            .replay_pending(|_, _| panic!("lost PCM must not be dispatched"))
            .is_err());
        let sid = fixture.config.track.session_id;
        assert!(
            !fixture
                .core
                .call(move |store| store.session_status(sid))
                .unwrap()
                .capture_stopped
        );
    }
    #[test]
    fn recovery_does_not_create_second_capture_seal() {
        let mut fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 1600];
        capture.record(&pcm).unwrap();
        capture.close_segment(&pcm).unwrap();
        fixture.transition(SessionState::Recording, SessionState::Stopping);
        capture.seal_after_devices_released().unwrap();
        drop(capture);
        let sid = fixture.config.track.session_id;
        let original = fixture
            .core
            .call(move |store| store.capture_seal(sid))
            .unwrap()
            .unwrap();
        let mut config = fixture.config.clone();
        config.replay_only = true;
        let mut capture = CaptureSession::open(config).unwrap();
        capture.replay_pending(|_, _| Ok(())).unwrap();
        let after = fixture
            .core
            .call(move |store| store.capture_seal(sid))
            .unwrap()
            .unwrap();
        assert_eq!(original.manifest_sha256, after.manifest_sha256);
    }

    #[test]
    fn source_only_capture_accepts_empty_translation_targets() {
        let mut fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        fixture.config.target_languages.clear();
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 1600];
        capture.record(&pcm).unwrap();
        assert!(capture
            .close_segment(&pcm)
            .unwrap()
            .target_languages
            .is_empty());
    }
    #[test]
    fn direction_ack_failure_keeps_old_state_and_replay_uses_new_boundary_for_tail() {
        let mut fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        fixture.config.configured_source_language = "zh".into();
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let pcm = vec![0.2; 1600];
        capture.record(&pcm).unwrap();
        let old = capture.close_segment(&pcm).unwrap();
        fixture.reject_capture.store(true, Ordering::Release);
        assert!(capture
            .change_direction("en".into(), vec!["zh".into()], 2)
            .is_err());
        assert_eq!(capture.context().direction_epoch, 1);
        capture.record(&pcm).unwrap();
        drop(capture);
        fixture.reject_capture.store(false, Ordering::Release);
        fixture.transition(SessionState::Recording, SessionState::Interrupted);
        let mut context = fixture.config.clone();
        context.replay_only = true;
        let mut capture = CaptureSession::open(context).unwrap();
        let mut dispatched = Vec::new();
        capture
            .replay_pending(|meta, _| {
                dispatched.push(meta.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(dispatched.len(), 2);
        assert_eq!(
            dispatched
                .iter()
                .find(|m| m.segment_id == old.segment_id)
                .unwrap()
                .direction_epoch,
            1
        );
        let new = dispatched
            .iter()
            .find(|m| m.segment_id != old.segment_id)
            .unwrap();
        assert_eq!(new.direction_epoch, 2);
        assert_eq!(new.configured_source_language, "en");
        assert_eq!(new.audio.start_sample, 1600);
        assert_eq!(new.target_languages, vec!["zh"]);
    }
    #[test]
    fn configured_segment_limit_is_enforced_without_reading_process_environment() {
        assert!(PcmSegmenter::with_max_segment_ms(999).is_err());
        assert!(PcmSegmenter::with_max_segment_ms(30001).is_err());
        let mut segmenter = PcmSegmenter::with_max_segment_ms(1000).unwrap();
        let closed = segmenter.push(&vec![0.2; 32000]);
        assert_eq!(closed.len(), 2);
        assert!(closed.iter().all(|segment| segment.samples.len() == 16000));
        assert_eq!(closed[1].start_sample, 16000);
    }
    #[test]
    fn delivery_does_not_send_next_segment_before_current_revision_is_durable() {
        let fixture = Fixture::new(true);
        fixture.ready.store(true, Ordering::Release);
        let mut capture = CaptureSession::open(fixture.config.clone()).unwrap();
        let mut queue = DeliveryQueue::default();
        let mut metas = Vec::new();
        for _ in 0..2 {
            let pcm = vec![0.2; 1600];
            capture.record(&pcm).unwrap();
            let metadata = capture.close_segment(&pcm).unwrap();
            metas.push(metadata.clone());
            queue.enqueue(metadata, pcm).unwrap();
        }
        let mut delivered = Vec::new();
        queue
            .pump(&mut capture, |m, _| {
                delivered.push(m.segment_id);
                Ok(())
            })
            .unwrap();
        queue
            .pump(&mut capture, |m, _| {
                delivered.push(m.segment_id);
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered, vec![metas[0].segment_id]);
        let mut asr =
            DurableProducer::open(fixture.config.runtime.for_producer("test-asr").unwrap())
                .unwrap();
        for (index, metadata) in metas.iter().enumerate() {
            let failed = index == 1;
            let result = forum_contracts::TranscriptFinal {
                track_id: metadata.track_id,
                segment_id: metadata.segment_id,
                revision: metadata.revision,
                audio: metadata.audio.clone(),
                text: if failed {
                    String::new()
                } else {
                    "synthetic transcript".into()
                },
                configured_source_language: metadata.configured_source_language.clone(),
                detected_language: None,
                target_languages: metadata.target_languages.clone(),
                direction_epoch: metadata.direction_epoch,
                speaker_id: None,
                status: if failed {
                    forum_contracts::TranscriptStatus::Failed
                } else {
                    forum_contracts::TranscriptStatus::Success
                },
                reason: failed.then(|| "injected ASR failure".into()),
                backend: "synthetic-test".into(),
                model_manifest_id: "synthetic-fixture".into(),
            };
            let pending = asr.append(EventType::TranscriptFinal, &result).unwrap();
            asr.flush_one(pending.message_id).unwrap();
            queue
                .pump(&mut capture, |m, _| {
                    delivered.push(m.segment_id);
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(
            delivered,
            metas.iter().map(|m| m.segment_id).collect::<Vec<_>>()
        );
        assert!(queue.is_empty());
    }
}
