//! Audio capture adapter and pipeline (design §4.2 items 1–2).
//!
//! The capture adapter produces timestamped frames from the microphone and
//! from system audio. It contains no Gemini or UI logic. The pipeline
//! resamples both sources to Gemini's mono 16 kHz PCM, mixes them, and keeps
//! only bounded in-memory buffers — audio never touches disk.

pub mod capture;
pub mod pipeline;
pub mod playback;

use serde::Serialize;

/// Target format required by the Gemini Live API input.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Mixed frames are emitted in 100 ms chunks.
pub const CHUNK_SAMPLES: usize = (TARGET_SAMPLE_RATE / 10) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AudioSource {
    Microphone,
    System,
}

/// Raw frame straight from a capture device, still in device format.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub source: AudioSource,
    /// Milliseconds on the monotonic session clock.
    pub t_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// 16 kHz mono chunk produced by the pipeline.
#[derive(Debug, Clone)]
pub struct MixedChunk {
    /// Monotonic sequence number used for gap reporting and alignment.
    pub seq: u64,
    pub t_ms: u64,
    /// Mixed mic + system samples, i16 little-endian order when serialized.
    pub mixed: Vec<i16>,
    /// System-only copy for diarization.
    pub system: Vec<i16>,
    /// Microphone-only copy, sent instead of `mixed` while readout audio is
    /// playing so the spoken translation is not translated again.
    pub mic: Vec<i16>,
    /// True when the microphone carried speech-level energy in this chunk.
    pub mic_active: bool,
}

/// Convert f32 samples in [-1, 1] to i16 with clamping.
pub fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Downmix interleaved multi-channel f32 samples to mono.
pub fn downmix(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// Stateful linear resampler: adequate for speech into a 16 kHz STT path and
/// dependency-free.
pub struct LinearResampler {
    from_rate: u32,
    to_rate: u32,
    /// Fractional read position carried across calls.
    pos: f64,
    /// Last sample of the previous buffer for interpolation continuity.
    prev: Option<f32>,
}

impl LinearResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            from_rate,
            to_rate,
            pos: 0.0,
            prev: None,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        if self.from_rate == self.to_rate {
            return input.to_vec();
        }
        let step = self.from_rate as f64 / self.to_rate as f64;
        // Virtual input includes the carried previous sample at index 0.
        let carried = self.prev.is_some();
        let get = |i: usize| -> f32 {
            if carried {
                if i == 0 {
                    self.prev.unwrap()
                } else {
                    input[i - 1]
                }
            } else {
                input[i]
            }
        };
        let virtual_len = input.len() + usize::from(carried);
        let mut out = Vec::with_capacity((input.len() as f64 / step) as usize + 2);
        let mut pos = self.pos;
        while pos + 1.0 < virtual_len as f64 {
            let i = pos as usize;
            let frac = (pos - i as f64) as f32;
            out.push(get(i) * (1.0 - frac) + get(i + 1) * frac);
            pos += step;
        }
        // Carry state into the next call.
        self.pos = pos - (virtual_len - 1) as f64;
        self.prev = Some(input[input.len() - 1]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        let stereo = [1.0, 0.0, 0.5, 0.5];
        assert_eq!(downmix(&stereo, 2), vec![0.5, 0.5]);
    }

    #[test]
    fn f32_conversion_clamps() {
        assert_eq!(f32_to_i16(2.0), 32767);
        assert_eq!(f32_to_i16(-2.0), -32767);
        assert_eq!(f32_to_i16(0.0), 0);
    }

    #[test]
    fn resampler_halves_rate() {
        let mut r = LinearResampler::new(32_000, 16_000);
        let input: Vec<f32> = (0..3200).map(|i| (i as f32 / 100.0).sin()).collect();
        let out = r.process(&input);
        // 3200 samples at 32k should give ~1600 at 16k.
        assert!((out.len() as i64 - 1600).abs() <= 2, "got {}", out.len());
    }

    #[test]
    fn resampler_is_continuous_across_calls() {
        let mut whole = LinearResampler::new(48_000, 16_000);
        let mut split = LinearResampler::new(48_000, 16_000);
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 / 50.0).sin()).collect();
        let a = whole.process(&input);
        let mut b = split.process(&input[..1000]);
        b.extend(split.process(&input[1000..]));
        assert!((a.len() as i64 - b.len() as i64).abs() <= 2);
        for (x, y) in a.iter().zip(b.iter()).take(300) {
            assert!((x - y).abs() < 1e-3);
        }
    }
}
