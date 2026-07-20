//! Kaldi-compatible log-mel filterbank features, pure Rust.
//!
//! Matches kaldi-native-fbank with the options the WeSpeaker embedding
//! models were trained against (and that pyannote-rs feeds them): 16 kHz,
//! 25 ms frames / 10 ms shift, dither off, DC removal, pre-emphasis 0.97,
//! povey window, 512-point FFT power spectrum, 80 mel bins (20 Hz–8 kHz,
//! kaldi mel scale), natural log, then per-utterance mean normalization.
//! Pure Rust (rustfft) so the portable build gains no native toolchain
//! dependency.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

pub const SAMPLE_RATE: usize = 16_000;
pub const NUM_BINS: usize = 80;
const FRAME_LEN: usize = 400; // 25 ms
const FRAME_SHIFT: usize = 160; // 10 ms
const NFFT: usize = 512; // next power of two ≥ FRAME_LEN
const PREEMPH: f32 = 0.97;
const MEL_LOW_HZ: f32 = 20.0;
const MEL_HIGH_HZ: f32 = 8_000.0;

fn mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Povey window: hann^0.85, kaldi's default.
fn povey_window() -> Vec<f32> {
    (0..FRAME_LEN)
        .map(|i| {
            let hann =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FRAME_LEN - 1) as f32).cos();
            hann.powf(0.85)
        })
        .collect()
}

/// 80 triangular filters over FFT bins 0..NFFT/2, triangles computed in the
/// mel domain exactly like kaldi's MelBanks.
fn mel_banks() -> Vec<Vec<(usize, f32)>> {
    let mel_low = mel(MEL_LOW_HZ);
    let mel_high = mel(MEL_HIGH_HZ);
    let delta = (mel_high - mel_low) / (NUM_BINS + 1) as f32;
    let fft_bin_mel: Vec<f32> = (0..NFFT / 2)
        .map(|k| mel(k as f32 * SAMPLE_RATE as f32 / NFFT as f32))
        .collect();
    (0..NUM_BINS)
        .map(|b| {
            let left = mel_low + b as f32 * delta;
            let center = left + delta;
            let right = center + delta;
            let mut taps = Vec::new();
            for (k, &m) in fft_bin_mel.iter().enumerate() {
                let w = if m > left && m <= center {
                    (m - left) / (center - left)
                } else if m > center && m < right {
                    (right - m) / (right - center)
                } else {
                    0.0
                };
                if w > 0.0 {
                    taps.push((k, w));
                }
            }
            taps
        })
        .collect()
}

/// Compute mean-normalized log-mel features. Input samples in [-1, 1]
/// (scale cancels in the log + mean-normalization; consistency is what
/// matters). Returns (frames, flattened [frames × NUM_BINS]).
pub fn compute(samples: &[f32]) -> (usize, Vec<f32>) {
    if samples.len() < FRAME_LEN {
        return (0, Vec::new());
    }
    let num_frames = 1 + (samples.len() - FRAME_LEN) / FRAME_SHIFT; // snip_edges
    let window = povey_window();
    let banks = mel_banks();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(NFFT);
    let mut feats = vec![0f32; num_frames * NUM_BINS];

    let mut buf = vec![Complex::new(0f32, 0f32); NFFT];
    let mut frame = vec![0f32; FRAME_LEN];
    for t in 0..num_frames {
        let start = t * FRAME_SHIFT;
        frame.copy_from_slice(&samples[start..start + FRAME_LEN]);
        // DC removal.
        let mean = frame.iter().sum::<f32>() / FRAME_LEN as f32;
        for s in frame.iter_mut() {
            *s -= mean;
        }
        // Pre-emphasis, in reverse like kaldi (first sample against itself).
        for i in (1..FRAME_LEN).rev() {
            frame[i] -= PREEMPH * frame[i - 1];
        }
        frame[0] -= PREEMPH * frame[0];
        // Window + zero-padded FFT.
        for i in 0..NFFT {
            buf[i] = if i < FRAME_LEN {
                Complex::new(frame[i] * window[i], 0.0)
            } else {
                Complex::new(0.0, 0.0)
            };
        }
        fft.process(&mut buf);
        let power: Vec<f32> = buf[..NFFT / 2].iter().map(|c| c.norm_sqr()).collect();
        for (b, taps) in banks.iter().enumerate() {
            let e: f32 = taps.iter().map(|&(k, w)| power[k] * w).sum();
            feats[t * NUM_BINS + b] = e.max(f32::EPSILON).ln();
        }
    }

    // Per-utterance cepstral mean normalization (what the embedding models
    // are fed downstream of kaldi-native-fbank).
    for b in 0..NUM_BINS {
        let mean = (0..num_frames).map(|t| feats[t * NUM_BINS + b]).sum::<f32>()
            / num_frames as f32;
        for t in 0..num_frames {
            feats[t * NUM_BINS + b] -= mean;
        }
    }
    (num_frames, feats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, secs: f32) -> Vec<f32> {
        (0..(SAMPLE_RATE as f32 * secs) as usize)
            .map(|i| {
                0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect()
    }

    #[test]
    fn frame_count_matches_snip_edges() {
        let (frames, feats) = compute(&sine(440.0, 1.0));
        // 16000 samples: 1 + (16000-400)/160 = 98 frames.
        assert_eq!(frames, 98);
        assert_eq!(feats.len(), 98 * NUM_BINS);
    }

    #[test]
    fn features_are_mean_normalized() {
        let (frames, feats) = compute(&sine(1000.0, 1.0));
        for b in 0..NUM_BINS {
            let mean: f32 =
                (0..frames).map(|t| feats[t * NUM_BINS + b]).sum::<f32>() / frames as f32;
            assert!(mean.abs() < 1e-3, "bin {b} mean {mean}");
        }
    }

    #[test]
    fn same_tone_frames_more_similar_than_cross_tone() {
        // The property clustering depends on: frames of one signal stay
        // closer to each other than to frames of a spectrally different
        // signal. Concatenate two tones so CMN does not flatten them.
        let mut samples = sine(500.0, 0.5);
        samples.extend(sine(3000.0, 0.5));
        let (frames, feats) = compute(&samples);
        let row = |t: usize| &feats[t * NUM_BINS..(t + 1) * NUM_BINS];
        let cos = |a: &[f32], b: &[f32]| {
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            dot / (na * nb).max(f32::EPSILON)
        };
        // Two frames well inside each half.
        let (a1, a2) = (frames / 5, frames * 2 / 5);
        let (b1, b2) = (frames * 3 / 5, frames * 4 / 5);
        let same = cos(row(a1), row(a2)).min(cos(row(b1), row(b2)));
        let cross = cos(row(a1), row(b1)).max(cos(row(a2), row(b2)));
        assert!(
            same > cross,
            "same-tone similarity {same} must exceed cross-tone {cross}"
        );
    }

    #[test]
    fn too_short_input_yields_no_frames() {
        let (frames, feats) = compute(&vec![0.1; 100]);
        assert_eq!(frames, 0);
        assert!(feats.is_empty());
    }
}
