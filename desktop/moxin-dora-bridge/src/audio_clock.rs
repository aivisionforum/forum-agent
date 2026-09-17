//! Bounded PCM clock alignment. Device timestamps, not polling time, locate audio.
use anyhow::{ensure, Result};
use std::time::{Duration, Instant};

pub struct TimedPcm {
    pub at: Instant,
    pub samples: Vec<f32>,
}
pub struct AlignedPcm {
    pub gap_before: u64,
    pub samples: Vec<f32>,
}
pub struct ClockMapper {
    epoch: Instant,
    cursor: u64,
    cutoff: Option<Instant>,
    pub corrected_samples: i64,
}
impl ClockMapper {
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            cursor: 0,
            cutoff: None,
            corrected_samples: 0,
        }
    }
    pub fn stop_at(&mut self, cutoff: Instant) {
        self.cutoff = Some(cutoff);
    }
    pub fn sample_at(&self, time: Instant) -> u64 {
        (time.saturating_duration_since(self.epoch).as_secs_f64() * 16000.0).round() as u64
    }
    pub fn map(&mut self, packet: TimedPcm, next: Option<Instant>) -> Result<AlignedPcm> {
        ensure!(
            !packet.samples.is_empty() && packet.samples.len() <= 64000,
            "invalid timed PCM packet"
        );
        let nominal = Duration::from_secs_f64(packet.samples.len() as f64 / 16000.0);
        let mut end = packet.at + nominal;
        if let Some(next) = next {
            ensure!(next >= packet.at, "audio device clock moved backwards");
            let actual = next.duration_since(packet.at);
            // Correct ordinary device-rate drift; a missing callback is a gap,
            // never a several-second time stretch of the preceding audio.
            if actual.abs_diff(nominal) <= Duration::from_millis(2) {
                end = next;
            }
        }
        if end <= self.epoch {
            return Ok(AlignedPcm {
                gap_before: 0,
                samples: vec![],
            });
        }
        let unclipped_end = end;
        if let Some(cutoff) = self.cutoff {
            if packet.at >= cutoff {
                return Ok(AlignedPcm {
                    gap_before: 0,
                    samples: vec![],
                });
            }
            end = end.min(cutoff);
        }
        let sample_at =
            |time: Instant| time.saturating_duration_since(self.epoch).as_secs_f64() * 16000.0;
        let start = sample_at(packet.at).round() as u64;
        let target_end = sample_at(end).round() as u64;
        ensure!(
            start.saturating_add(32) >= self.cursor,
            "audio device clock overlapped previous frames"
        );
        let first = start.max(self.cursor);
        if target_end <= first {
            return Ok(AlignedPcm {
                gap_before: 0,
                samples: vec![],
            });
        }
        let gap_before = first - self.cursor;
        let total = (unclipped_end.duration_since(packet.at).as_secs_f64() * 16000.0)
            .round()
            .max(1.0) as usize;
        let prefix = if packet.at < self.epoch {
            (self.epoch.duration_since(packet.at).as_secs_f64() * 16000.0).round() as usize
        } else {
            (first - start) as usize
        };
        let mut samples = resize_pcm(&packet.samples, total);
        samples.drain(..prefix.min(samples.len()));
        samples.truncate((target_end - first) as usize);
        self.corrected_samples +=
            samples.len() as i64 + prefix as i64 - packet.samples.len() as i64;
        self.cursor = first + samples.len() as u64;
        Ok(AlignedPcm {
            gap_before,
            samples,
        })
    }
}
fn resize_pcm(input: &[f32], length: usize) -> Vec<f32> {
    if length == input.len() {
        return input.to_vec();
    }
    (0..length)
        .map(|n| {
            let p = n as f64 * input.len() as f64 / length as f64;
            let left = (p as usize).min(input.len() - 1);
            let right = (left + 1).min(input.len() - 1);
            input[left] + (input[right] - input[left]) * (p - left as f64) as f32
        })
        .collect()
}

/// Continuous rational resampling avoids rounding each CPAL callback separately.
pub struct StreamingResampler {
    input_rate: u32,
    output_rate: u32,
    buffer: Vec<f32>,
    base: u64,
    next: u64,
}
impl StreamingResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            input_rate,
            output_rate,
            buffer: vec![],
            base: 0,
            next: 0,
        }
    }
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        if self.input_rate == self.output_rate {
            return input.to_vec();
        }
        self.buffer.extend_from_slice(input);
        let mut output = vec![];
        loop {
            let absolute = self.next / self.output_rate as u64;
            let left = (absolute - self.base) as usize;
            if left + 1 >= self.buffer.len() {
                break;
            }
            let fraction = (self.next % self.output_rate as u64) as f32 / self.output_rate as f32;
            output.push(self.buffer[left] + (self.buffer[left + 1] - self.buffer[left]) * fraction);
            self.next += self.input_rate as u64;
        }
        let remove = (self.next / self.output_rate as u64 - self.base)
            .min(self.buffer.len().saturating_sub(1) as u64) as usize;
        self.buffer.drain(..remove);
        self.base += remove as u64;
        output
    }
}

/// Conservative reference cancellation. Never deduplicates transcript text.
/// Ambiguous correlation/double talk passes through and requires acoustic QA.
#[derive(Default)]
pub struct EchoReference {
    samples: std::collections::VecDeque<f32>,
}
impl EchoReference {
    pub fn push(&mut self, pcm: &[f32]) {
        self.samples.extend(pcm);
        while self.samples.len() > 48000 {
            self.samples.pop_front();
        }
    }
    pub fn filter(&self, mic: &[f32]) -> Vec<f32> {
        if mic.len() < 320 || mic.len() > 16000 || self.samples.len() < mic.len() {
            return mic.to_vec();
        }
        let reference: Vec<_> = self.samples.iter().copied().collect();
        let near = mic
            .iter()
            .step_by(8)
            .map(|v| (*v as f64).powi(2))
            .sum::<f64>();
        if near < 1e-5 {
            return mic.to_vec();
        }
        let mut best = (0.0, 0usize, 0.0);
        for delay in (0..=3200.min(reference.len() - mic.len())).step_by(16) {
            let start = reference.len() - mic.len() - delay;
            let mut dot = 0.0;
            let mut far = 0.0;
            for i in (0..mic.len()).step_by(8) {
                let r = reference[start + i] as f64;
                dot += r * mic[i] as f64;
                far += r * r;
            }
            if far < 1e-5 {
                continue;
            }
            let correlation = dot / (far * near).sqrt();
            if correlation > best.0 {
                best = (correlation, start, dot / far);
            }
        }
        if best.0 < 0.97 || !(0.05..=2.0).contains(&best.2) {
            return mic.to_vec();
        }
        mic.iter()
            .enumerate()
            .map(|(i, s)| (s - reference[best.1 + i] * best.2 as f32).clamp(-1.0, 1.0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_chunking_does_not_accumulate_resampling_drift() {
        let input: Vec<_> = (0..441000).map(|n| (n as f32 * 0.013).sin()).collect();
        let expected = StreamingResampler::new(44100, 16000).push(&input);
        let mut r = StreamingResampler::new(44100, 16000);
        let actual: Vec<_> = input.chunks(257).flat_map(|v| r.push(v)).collect();
        assert_eq!(actual, expected);
        assert!((actual.len() as i64 - 160000).abs() <= 1);
    }
    #[test]
    fn timestamps_correct_drift_but_preserve_missing_callback_gap() {
        let epoch = Instant::now();
        let mut mapper = ClockMapper::new(epoch);
        for i in 0..1000 {
            let at = epoch + Duration::from_micros(i * 100050);
            let next = epoch + Duration::from_micros((i + 1) * 100050);
            let p = mapper
                .map(
                    TimedPcm {
                        at,
                        samples: vec![0.1; 1600],
                    },
                    Some(next),
                )
                .unwrap();
            assert_eq!(p.gap_before, 0);
        }
        assert_eq!(mapper.cursor, 1600800);
        let result = mapper
            .map(
                TimedPcm {
                    at: epoch + Duration::from_secs(101),
                    samples: vec![0.1; 1600],
                },
                None,
            )
            .unwrap();
        assert!(result.gap_before > 10000);
    }
    #[test]
    fn reference_echo_is_reduced_without_removing_unrelated_speech() {
        let far: Vec<_> = (0..3200)
            .map(|n| ((n * n % 7919) as f32 / 7919.0 - 0.5) * 0.5)
            .collect();
        let mut echo = EchoReference::default();
        echo.push(&far);
        let reflected: Vec<_> = far.iter().map(|s| s * 0.6).collect();
        assert!(echo.filter(&reflected).iter().all(|s| s.abs() < 1e-5));
        let near: Vec<_> = (0..3200).map(|n| (n as f32 * 0.052).sin() * 0.3).collect();
        assert_eq!(echo.filter(&near), near);
    }
    #[test]
    fn stop_clips_later_callback_without_stretching_the_audio() {
        let epoch = Instant::now();
        let mut mapper = ClockMapper::new(epoch);
        mapper.stop_at(epoch + Duration::from_millis(50));
        let pcm: Vec<_> = (0..1600).map(|n| n as f32 / 1600.0).collect();
        let result = mapper
            .map(
                TimedPcm {
                    at: epoch,
                    samples: pcm.clone(),
                },
                None,
            )
            .unwrap();
        assert_eq!(result.samples, pcm[..800]);
        assert!(mapper
            .map(
                TimedPcm {
                    at: epoch + Duration::from_millis(100),
                    samples: pcm
                },
                None
            )
            .unwrap()
            .samples
            .is_empty());
    }
    #[test]
    fn simulated_ninety_minute_device_drift_stays_on_common_clock() {
        // Accelerated deterministic timestamps; this is not a hardware rehearsal.
        let epoch = Instant::now();
        let mut mapper = ClockMapper::new(epoch);
        for i in 0..54000 {
            mapper
                .map(
                    TimedPcm {
                        at: epoch + Duration::from_micros(i * 100050),
                        samples: vec![0.1; 1600],
                    },
                    Some(epoch + Duration::from_micros((i + 1) * 100050)),
                )
                .unwrap();
        }
        assert_eq!(mapper.cursor, 86_443_200);
        assert_eq!(mapper.corrected_samples, 43_200);
    }
}
