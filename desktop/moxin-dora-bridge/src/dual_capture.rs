//! Two independent journals/VADs and one fair, acknowledged ASR delivery queue.
use crate::reliable_capture::{DeliveryQueue, PcmSegmenter};
use crate::{CaptureContext, CaptureProgress, CaptureSession};
use anyhow::{ensure, Result};
use forum_runtime::audio::SegmentMeta;

pub struct DualCapture {
    pub captures: Vec<CaptureSession>,
    segmenters: Vec<PcmSegmenter>,
    deliveries: DeliveryQueue,
}
impl DualCapture {
    pub fn open(context: CaptureContext) -> Result<Self> {
        let other = context
            .secondary_track
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("dual track missing"))?;
        ensure!(
            context.track.kind == forum_contracts::TrackKind::Mic
                && other.kind == forum_contracts::TrackKind::System
                && context.track.track_id != other.track_id
                && context.track.session_id == other.session_id
                && other.sample_rate == 16000,
            "dual source scope mismatch"
        );
        let mut secondary = context.clone();
        secondary.track = other.clone();
        let ms = context.max_segment_ms;
        Ok(Self {
            captures: vec![
                CaptureSession::open(context)?,
                CaptureSession::open(secondary)?,
            ],
            segmenters: vec![
                PcmSegmenter::with_max_segment_ms(ms)?,
                PcmSegmenter::with_max_segment_ms(ms)?,
            ],
            deliveries: DeliveryQueue::default(),
        })
    }
    pub fn ready(&self) -> Result<bool> {
        self.captures[0].models_ready()
    }
    fn close_pending(&mut self, index: usize) -> Result<()> {
        if let Some(segment) = self.segmenters[index].finish() {
            let capture = &mut self.captures[index];
            capture.use_segment_identity(segment.segment_id, segment.start_sample)?;
            self.deliveries
                .enqueue(capture.close_segment(&segment.samples)?, segment.samples)?;
        }
        Ok(())
    }
    pub fn process(
        &mut self,
        index: usize,
        raw: &[f32],
        processed: &[f32],
        gap_before: u64,
    ) -> Result<()> {
        ensure!(
            index < 2 && raw.len() == processed.len(),
            "dual audio mapping mismatch"
        );
        if gap_before > 0 {
            self.close_pending(index)?;
            self.captures[index].record_gap(gap_before, "device_timestamp_gap")?;
            self.segmenters[index].skip_samples(gap_before);
        }
        // Continuous chunks preserve the input before reference cancellation;
        // segment files preserve the exact PCM supplied to ASR for replay.
        self.captures[index].record(raw)?;
        for segment in self.segmenters[index].push(processed) {
            let capture = &mut self.captures[index];
            capture.use_segment_identity(segment.segment_id, segment.start_sample)?;
            self.deliveries
                .enqueue(capture.close_segment(&segment.samples)?, segment.samples)?;
        }
        self.captures[index].checkpoint_segmenter(&self.segmenters[index])?;
        Ok(())
    }
    pub fn pump(&mut self, dispatch: impl FnMut(&SegmentMeta, &[f32]) -> Result<()>) -> Result<()> {
        self.deliveries.pump_tracks(&mut self.captures, dispatch)
    }
    pub fn empty(&self) -> bool {
        self.deliveries.is_empty()
    }
    pub fn seal(&mut self) -> Result<()> {
        let final_sample = self
            .captures
            .iter()
            .map(|c| c.progress().final_sample)
            .max()
            .unwrap_or(0);
        self.seal_at(final_sample)
    }
    pub fn seal_at(&mut self, final_sample: u64) -> Result<()> {
        // No callback is not evidence of digital silence. Retain a missing-tail
        // gap for either device instead of reporting a falsely complete track.
        for i in 0..2 {
            let captured = self.captures[i].progress().final_sample;
            ensure!(
                captured <= final_sample,
                "audio extends past shared stop boundary"
            );
            if captured < final_sample {
                self.process(i, &[], &[], final_sample - captured)?;
            }
        }
        for i in 0..2 {
            self.close_pending(i)?;
            self.captures[i].checkpoint_segmenter(&self.segmenters[i])?;
        }
        self.seal_journals()
    }
    fn seal_journals(&mut self) -> Result<()> {
        let tracks = self
            .captures
            .iter_mut()
            .map(CaptureSession::seal_recording)
            .collect::<Result<Vec<_>>>()?;
        self.captures[0].publish_capture_seal(tracks)
    }
    pub fn replay(
        &mut self,
        mut dispatch: impl FnMut(&SegmentMeta, &[f32]) -> Result<()>,
    ) -> Result<()> {
        let mut all = vec![];
        for capture in &mut self.captures {
            all.extend(capture.replay_segments(&mut dispatch)?);
        }
        self.seal_journals()?;
        self.captures[0].acknowledge_replay(all)
    }
    pub fn progress(&self) -> CaptureProgress {
        let mut progress = CaptureProgress::default();
        for capture in &self.captures {
            let p = capture.progress();
            progress.final_sample = progress.final_sample.max(p.final_sample);
            progress.segments_closed += p.segments_closed;
            progress.outbox_pending += p.outbox_pending;
        }
        progress
    }
}
