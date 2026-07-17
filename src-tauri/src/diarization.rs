//! Best-effort local diarization (design §7).
//!
//! Structure: VAD finds remote-speech segments, an embedding extractor turns
//! each segment into a voice vector, online clustering groups vectors into
//! `Speaker N` labels. Low confidence falls back to `Meeting`.
//!
//! The production VAD and speaker-embedding ONNX models are selected during
//! implementation (design §7); they plug in through [`EmbeddingExtractor`]
//! without touching transcript storage or UI code. The built-in extractor is
//! a dependency-free spectral-band profile that provides coarse best-effort
//! separation in the meantime. The whole service can be disabled in settings.

use crate::audio::TARGET_SAMPLE_RATE;
use serde::Serialize;

/// Frames shorter than this are ignored as noise.
const MIN_SEGMENT_MS: u64 = 400;
/// Silence longer than this closes the current segment.
const SILENCE_CLOSE_MS: u64 = 600;
/// Energy threshold (RMS over a 100 ms chunk) that counts as speech.
const SPEECH_RMS: f32 = 0.010;
/// Cosine similarity required to join an existing speaker cluster.
const JOIN_SIMILARITY: f32 = 0.86;
/// Similarity below which a segment is labeled `Meeting` instead of a speaker.
const CONFIDENT_SIMILARITY: f32 = 0.55;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum SpeakerLabel {
    You,
    Speaker(u32),
    Meeting,
    MultipleSpeakers,
}

impl SpeakerLabel {
    pub fn display(&self) -> String {
        match self {
            SpeakerLabel::You => "You".into(),
            SpeakerLabel::Speaker(n) => format!("Speaker {n}"),
            SpeakerLabel::Meeting => "Meeting".into(),
            SpeakerLabel::MultipleSpeakers => "Multiple speakers".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeakerRange {
    pub start_ms: u64,
    pub end_ms: u64,
    pub label: SpeakerLabel,
    pub confidence: f32,
}

/// Boundary where real ONNX VAD/embedding models plug in later.
pub trait EmbeddingExtractor: Send {
    /// 16 kHz mono segment in, fixed-size voice vector out.
    fn embed(&mut self, samples: &[i16]) -> Vec<f32>;
}

/// Dependency-free fallback: average log-energy across frequency bands,
/// computed with a Goertzel-style filter bank. Captures coarse voice timbre.
pub struct SpectralBandExtractor {
    bands_hz: Vec<f32>,
}

impl Default for SpectralBandExtractor {
    fn default() -> Self {
        // Speech-relevant band centers up to ~4 kHz.
        Self {
            bands_hz: vec![
                120.0, 220.0, 350.0, 500.0, 700.0, 950.0, 1250.0, 1600.0, 2000.0, 2500.0, 3100.0,
                3800.0,
            ],
        }
    }
}

impl EmbeddingExtractor for SpectralBandExtractor {
    fn embed(&mut self, samples: &[i16]) -> Vec<f32> {
        const WINDOW: usize = 400; // 25 ms at 16 kHz
        let mut acc = vec![0.0f32; self.bands_hz.len()];
        let mut windows = 0usize;
        for chunk in samples.chunks(WINDOW) {
            if chunk.len() < WINDOW / 2 {
                continue;
            }
            for (bi, &hz) in self.bands_hz.iter().enumerate() {
                acc[bi] += goertzel_power(chunk, hz, TARGET_SAMPLE_RATE as f32);
            }
            windows += 1;
        }
        if windows == 0 {
            return vec![0.0; self.bands_hz.len()];
        }
        let mut v: Vec<f32> = acc
            .iter()
            .map(|p| (p / windows as f32 + 1e-9).ln())
            .collect();
        normalize(&mut v);
        v
    }
}

fn goertzel_power(samples: &[i16], freq_hz: f32, sample_rate: f32) -> f32 {
    let k = 2.0 * std::f32::consts::PI * freq_hz / sample_rate;
    let coeff = 2.0 * k.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &x in samples {
        let s0 = x as f32 / 32768.0 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na < 1e-9 || nb < 1e-9 {
        0.0
    } else {
        dot / (na * nb)
    }
}

struct Cluster {
    centroid: Vec<f32>,
    count: u32,
}

/// Online diarizer over 16 kHz mono system-audio chunks.
pub struct Diarizer {
    extractor: Box<dyn EmbeddingExtractor>,
    clusters: Vec<Cluster>,
    ranges: Vec<SpeakerRange>,
    // Current open segment: samples plus its time span.
    seg_samples: Vec<i16>,
    seg_start_ms: Option<u64>,
    seg_last_speech_ms: u64,
    /// Retained (embedding, range index) pairs for final reconciliation.
    embeddings: Vec<(Vec<f32>, usize)>,
}

impl Diarizer {
    pub fn new() -> Self {
        Self::with_extractor(Box::new(SpectralBandExtractor::default()))
    }

    pub fn with_extractor(extractor: Box<dyn EmbeddingExtractor>) -> Self {
        Self {
            extractor,
            clusters: Vec::new(),
            ranges: Vec::new(),
            seg_samples: Vec::new(),
            seg_start_ms: None,
            seg_last_speech_ms: 0,
            embeddings: Vec::new(),
        }
    }

    /// Feed one pipeline chunk of system audio. `t_ms` is the chunk start on
    /// the session clock. Source audio is not retained beyond the open
    /// segment; only embeddings and ranges survive (design §7).
    pub fn push_chunk(&mut self, samples: &[i16], t_ms: u64) {
        let rms = (samples
            .iter()
            .map(|&s| {
                let f = s as f32 / 32768.0;
                f * f
            })
            .sum::<f32>()
            / samples.len().max(1) as f32)
            .sqrt();
        let chunk_ms = (samples.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64;

        if rms > SPEECH_RMS {
            if self.seg_start_ms.is_none() {
                self.seg_start_ms = Some(t_ms);
            }
            self.seg_samples.extend_from_slice(samples);
            self.seg_last_speech_ms = t_ms + chunk_ms;
        } else if let Some(start) = self.seg_start_ms {
            if t_ms.saturating_sub(self.seg_last_speech_ms) >= SILENCE_CLOSE_MS {
                self.close_segment(start);
            }
        }
    }

    fn close_segment(&mut self, start_ms: u64) {
        let end_ms = self.seg_last_speech_ms;
        let samples = std::mem::take(&mut self.seg_samples);
        self.seg_start_ms = None;
        if end_ms.saturating_sub(start_ms) < MIN_SEGMENT_MS {
            return;
        }
        let embedding = self.extractor.embed(&samples);
        // Segment audio is dropped here; only the embedding survives.
        drop(samples);
        let (label, confidence) = self.assign(&embedding);
        let idx = self.ranges.len();
        self.ranges.push(SpeakerRange {
            start_ms,
            end_ms,
            label,
            confidence,
        });
        self.embeddings.push((embedding, idx));
    }

    fn assign(&mut self, embedding: &[f32]) -> (SpeakerLabel, f32) {
        let mut best: Option<(usize, f32)> = None;
        for (i, c) in self.clusters.iter().enumerate() {
            let sim = cosine(embedding, &c.centroid);
            if best.map(|(_, s)| sim > s).unwrap_or(true) {
                best = Some((i, sim));
            }
        }
        match best {
            Some((i, sim)) if sim >= JOIN_SIMILARITY => {
                let c = &mut self.clusters[i];
                let n = c.count as f32;
                for (cx, ex) in c.centroid.iter_mut().zip(embedding) {
                    *cx = (*cx * n + ex) / (n + 1.0);
                }
                c.count += 1;
                (SpeakerLabel::Speaker(i as u32 + 1), sim)
            }
            Some((_, sim)) if sim < CONFIDENT_SIMILARITY && !self.clusters.is_empty() => {
                // Too dissimilar to join, too weak to trust as a new voice.
                (SpeakerLabel::Meeting, sim.max(0.0))
            }
            _ => {
                self.clusters.push(Cluster {
                    centroid: embedding.to_vec(),
                    count: 1,
                });
                (SpeakerLabel::Speaker(self.clusters.len() as u32), 1.0)
            }
        }
    }

    /// Close any open segment (call at meeting end) and return all ranges.
    pub fn finish(&mut self) -> Vec<SpeakerRange> {
        if let Some(start) = self.seg_start_ms {
            self.close_segment(start);
        }
        self.ranges.clone()
    }

    /// Label covering the given time span, for the timeline assembler.
    /// Overlapping distinct speakers yield `MultipleSpeakers` unless one
    /// dominates (design §7).
    pub fn label_for_span(&self, start_ms: u64, end_ms: u64) -> Option<SpeakerLabel> {
        let mut overlaps: Vec<(&SpeakerRange, u64)> = self
            .ranges
            .iter()
            .filter_map(|r| {
                let s = r.start_ms.max(start_ms);
                let e = r.end_ms.min(end_ms);
                if e > s {
                    Some((r, e - s))
                } else {
                    None
                }
            })
            .collect();
        if overlaps.is_empty() {
            return None;
        }
        overlaps.sort_by_key(|(_, len)| std::cmp::Reverse(*len));
        let total: u64 = overlaps.iter().map(|(_, l)| l).sum();
        let (top, top_len) = overlaps[0];
        let distinct: std::collections::HashSet<String> = overlaps
            .iter()
            .map(|(r, _)| r.label.display())
            .collect();
        if distinct.len() > 1 && (top_len as f64) < 0.7 * total as f64 {
            return Some(SpeakerLabel::MultipleSpeakers);
        }
        Some(top.label.clone())
    }
}

impl Default for Diarizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::CHUNK_SAMPLES;

    /// Deterministic fake extractor for cluster-behavior tests.
    struct FakeExtractor;
    impl EmbeddingExtractor for FakeExtractor {
        fn embed(&mut self, samples: &[i16]) -> Vec<f32> {
            // Amplitude bucket as a stand-in voice signature: quiet and loud
            // "voices" map to orthogonal vectors.
            let m = samples.iter().map(|&s| s.abs() as f32).sum::<f32>() / samples.len() as f32;
            if m < 10_000.0 {
                vec![1.0, 0.0, 0.0]
            } else {
                vec![0.0, 1.0, 0.0]
            }
        }
    }

    fn speech_chunk(level: i16) -> Vec<i16> {
        (0..CHUNK_SAMPLES)
            .map(|i| if i % 2 == 0 { level } else { -level })
            .collect()
    }

    fn silence_chunk() -> Vec<i16> {
        vec![0; CHUNK_SAMPLES]
    }

    fn run_segments(d: &mut Diarizer, levels: &[i16]) {
        let mut t = 0u64;
        for &lvl in levels {
            for _ in 0..10 {
                d.push_chunk(&speech_chunk(lvl), t);
                t += 100;
            }
            for _ in 0..10 {
                d.push_chunk(&silence_chunk(), t);
                t += 100;
            }
        }
    }

    #[test]
    fn same_voice_gets_same_label() {
        let mut d = Diarizer::with_extractor(Box::new(FakeExtractor));
        run_segments(&mut d, &[5000, 5000]);
        let ranges = d.finish();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].label, ranges[1].label);
    }

    #[test]
    fn different_voices_get_different_labels() {
        let mut d = Diarizer::with_extractor(Box::new(FakeExtractor));
        run_segments(&mut d, &[2000, 20000]);
        let ranges = d.finish();
        assert_eq!(ranges.len(), 2);
        assert_ne!(ranges[0].label, ranges[1].label);
    }

    #[test]
    fn short_blips_ignored() {
        let mut d = Diarizer::with_extractor(Box::new(FakeExtractor));
        // 200 ms of speech, below MIN_SEGMENT_MS.
        d.push_chunk(&speech_chunk(5000), 0);
        d.push_chunk(&speech_chunk(5000), 100);
        for i in 0..10 {
            d.push_chunk(&silence_chunk(), 200 + i * 100);
        }
        assert!(d.finish().is_empty());
    }

    #[test]
    fn span_labeling_picks_dominant() {
        let mut d = Diarizer::with_extractor(Box::new(FakeExtractor));
        run_segments(&mut d, &[5000]);
        d.finish();
        let label = d.label_for_span(0, 1000).expect("label");
        assert_eq!(label, SpeakerLabel::Speaker(1));
        assert!(d.label_for_span(500_000, 501_000).is_none());
    }
}
