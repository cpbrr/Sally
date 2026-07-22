//! Audio capture adapter and pipeline (design §4.2 items 1–2).
//!
//! The capture adapter produces timestamped frames from the microphone and
//! from system audio. No Gemini or UI logic lives here. The pipeline
//! resamples both sources to Gemini's mono 16 kHz PCM, mixes them, and keeps
//! only bounded in-memory buffers. The optional recorder (Settings → "Save
//! meeting audio") is the single place audio may touch disk, and only
//! locally.

#[cfg(windows)]
pub mod app_capture;
pub mod capture;
#[cfg(target_os = "macos")]
pub mod coreaudio_tap;
#[cfg(target_os = "macos")]
pub mod sck_capture;
pub mod pipeline;
pub mod playback;
pub mod recorder;

use crate::error::{Result, SallyError};
use serde::Serialize;

/// Spawn `body` on its own OS thread and block the caller until it reports
/// startup success or failure via the `Sender` it's given — so `spawn_*`
/// functions fail fast on startup errors instead of returning a handle to a
/// thread that's already dead. `context` names the failure in the error
/// message if the thread panics or drops the sender without a reply.
fn spawn_with_ready_signal(
    context: &'static str,
    body: impl FnOnce(std::sync::mpsc::Sender<Result<()>>) + Send + 'static,
) -> Result<std::thread::JoinHandle<()>> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let handle = std::thread::spawn(move || body(ready_tx));
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => Err(SallyError::Audio(format!("{context} thread died during startup"))),
    }
}

/// Target format required by the Gemini Live API input.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Mixed frames are emitted in 50 ms chunks: halves client-side buffering
/// latency vs the old 100 ms without changing any downstream math (the
/// split detector rings its own 10 s window and Gemini accepts any chunk
/// size).
pub const CHUNK_SAMPLES: usize = (TARGET_SAMPLE_RATE / 20) as usize;

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
    /// Microphone-only copy, sent instead of `mixed` while readout audio is
    /// playing so the spoken translation is not translated again.
    pub mic: Vec<i16>,
    /// True when the microphone carried speech-level energy in this chunk.
    pub mic_active: bool,
    /// True when system audio carried speech-level energy in this chunk.
    pub system_active: bool,
    /// System-lane-only copy for the speaker-change detector.
    pub system: Vec<f32>,
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

/// Stateful resampler using Catmull-Rom cubic interpolation: still O(1) per
/// output sample and dependency-free, but band-limited enough to avoid the
/// imaging/aliasing a bare linear interpolator produces on non-integer rate
/// ratios (e.g. 44.1 kHz <-> 16 kHz, or 24 kHz <-> 44.1 kHz — both of which
/// come up resampling into/out of Gemini's 16 kHz/24 kHz PCM depending on
/// what rate the OS default audio device negotiates).
pub struct CubicResampler {
    from_rate: u32,
    to_rate: u32,
    /// Fractional read position carried across calls, indexed into the
    /// virtual buffer described by `history`.
    pos: f64,
    /// Last two samples of the previous buffer, carried so the 4-point
    /// interpolation window (one sample before, two after) stays valid
    /// right at the start of the next call.
    history: [f32; 2],
    has_history: bool,
}

impl CubicResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            from_rate,
            to_rate,
            pos: 0.0,
            history: [0.0, 0.0],
            has_history: false,
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
        // Virtual buffer: two carried history samples (from the previous
        // call) followed by this call's input, so the interpolation window
        // has a sample before index 0 of `input` to draw on. The very
        // first call (no history yet) is just `input` itself.
        let offset = if self.has_history { 2usize } else { 0usize };
        let len = input.len() + offset;
        let history = self.history;
        let has_history = self.has_history;
        let get = |i: usize| -> f32 {
            if has_history {
                match i {
                    0 => history[0],
                    1 => history[1],
                    _ => input[i - 2],
                }
            } else {
                input[i]
            }
        };
        let last = (len - 1) as isize;
        let sample_at = |i: isize| -> f32 { get(i.clamp(0, last) as usize) };

        let mut out = Vec::with_capacity((input.len() as f64 / step) as usize + 2);
        let mut pos = self.pos + offset as f64;
        // Stop once i + 2 (the furthest lookahead sample) would fall
        // outside the buffer, deferring the remainder to the next call
        // via the carried `pos` instead of clamping mid-stream.
        while pos + 2.0 < len as f64 {
            let i = pos as usize;
            let t = (pos - i as f64) as f32;
            let p0 = sample_at(i as isize - 1);
            let p1 = sample_at(i as isize);
            let p2 = sample_at(i as isize + 1);
            let p3 = sample_at(i as isize + 2);
            out.push(catmull_rom(p0, p1, p2, p3, t));
            pos += step;
        }
        // Carry state into the next call: two history samples now sit at
        // virtual indices 0 and 1, so the stored position rebases by one
        // more than a single-sample carry would.
        self.pos = pos - len as f64 + 2.0;
        if input.len() >= 2 {
            self.history = [input[input.len() - 2], input[input.len() - 1]];
        } else {
            self.history = [history[1], input[0]];
        }
        self.has_history = true;
        out
    }
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
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
        let mut r = CubicResampler::new(32_000, 16_000);
        let input: Vec<f32> = (0..3200).map(|i| (i as f32 / 100.0).sin()).collect();
        let out = r.process(&input);
        // 3200 samples at 32k should give ~1600 at 16k.
        assert!((out.len() as i64 - 1600).abs() <= 2, "got {}", out.len());
    }

    #[test]
    fn resampler_is_continuous_across_calls() {
        let mut whole = CubicResampler::new(48_000, 16_000);
        let mut split = CubicResampler::new(48_000, 16_000);
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 / 50.0).sin()).collect();
        let a = whole.process(&input);
        let mut b = split.process(&input[..1000]);
        b.extend(split.process(&input[1000..]));
        assert!((a.len() as i64 - b.len() as i64).abs() <= 2);
        for (x, y) in a.iter().zip(b.iter()).take(300) {
            assert!((x - y).abs() < 1e-3);
        }
    }

    #[test]
    fn cubic_reduces_error_vs_linear_on_non_integer_ratio() {
        // 44.1kHz -> 24kHz has no clean integer step, the case that most
        // exposes a bare linear interpolator's imaging/aliasing (and the
        // most likely OS-specific mismatch behind mac-only readout
        // glitches: cpal negotiates whatever the default device reports,
        // commonly 44.1kHz on macOS vs. 48kHz on Windows).
        let from = 44_100u32;
        let to = 24_000u32;
        let freq = 440.0f64;
        let input: Vec<f32> = (0..4410)
            .map(|i| ((i as f64 / from as f64) * 2.0 * std::f64::consts::PI * freq).sin() as f32)
            .collect();

        let mut cubic = CubicResampler::new(from, to);
        let cubic_out = cubic.process(&input);

        // Minimal linear-interpolation baseline mirroring the old
        // production implementation, kept only here to prove the upgrade
        // actually reduced error on this ratio.
        let step = from as f64 / to as f64;
        let mut linear_out = Vec::new();
        let mut pos = 0.0f64;
        while pos + 1.0 < input.len() as f64 {
            let i = pos as usize;
            let frac = (pos - i as f64) as f32;
            linear_out.push(input[i] * (1.0 - frac) + input[i + 1] * frac);
            pos += step;
        }

        let exact = |n: usize| -> f32 {
            ((n as f64 / to as f64) * 2.0 * std::f64::consts::PI * freq).sin() as f32
        };
        let max_err = |out: &[f32]| -> f32 {
            out.iter()
                .enumerate()
                .map(|(n, &s)| (s - exact(n)).abs())
                .fold(0.0f32, f32::max)
        };

        let cubic_err = max_err(&cubic_out);
        let linear_err = max_err(&linear_out);
        assert!(
            cubic_err < linear_err,
            "cubic error {cubic_err} should be lower than linear error {linear_err}"
        );
    }
}
