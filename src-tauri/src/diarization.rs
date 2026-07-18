//! Speaker diarization (design §7), always on.
//!
//! Primary backend: sherpa-onnx — silero VAD segments remote speech, a
//! speaker-embedding model (3D-Speaker ERes2Net) turns each segment into a
//! voice vector, and online cosine clustering groups vectors into
//! `Speaker N` labels. Model files are fetched by `models::ensure_models`.
//!
//! If the models are unavailable (offline first run), a dependency-free
//! fallback (energy VAD + spectral-band profile) keeps meetings working
//! with coarser labels.
//!
//! sherpa-onnx handles hold raw FFI pointers and are not `Send`, so the
//! whole diarizer runs on its own thread; the session talks to it through
//! a channel and reads completed ranges from shared state.

use crate::audio::TARGET_SAMPLE_RATE;
use crate::models::ModelPaths;
use serde::Serialize;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Segments shorter than this are ignored as noise.
const MIN_SEGMENT_MS: u64 = 400;

// Fallback (energy VAD + spectral profile) tuning.
const FB_SILENCE_CLOSE_MS: u64 = 600;
const FB_SPEECH_RMS: f32 = 0.010;

// Thresholds are estimated per meeting from the pairwise-similarity
// distribution (Otsu split): fixed constants calibrated on clean audio do
// not transfer — compressed meeting audio shifts same-speaker cosines to
// ~0.8 and cross-speaker to ~0.5-0.6, far above any clean-audio setting.
/// Below this separation between the two similarity populations the audio
/// is treated as single-speaker (no reliable split exists).
const MIN_SPLIT_MARGIN: f32 = 0.12;
/// Sample cap for the O(n²) pairwise estimate.
const THRESHOLD_SAMPLE: usize = 256;
/// A cluster this light that also holds under 5% of total speech is noise:
/// dissolved on each pass, members reassigned or left uncertain.
const MIN_CLUSTER_WEIGHT: f32 = 4.0;
const MIN_CLUSTER_SHARE: f32 = 0.05;
const MAX_CLUSTERS: usize = 8;
/// Segments shorter than this yield unreliable embeddings and may never
/// seed a new speaker (they can still join existing ones).
const MIN_NEW_CLUSTER_MS: u64 = 800;
/// Re-cluster after this many new segments or this much audio time.
const RECLUSTER_EVERY_SEGMENTS: usize = 10;
const RECLUSTER_EVERY_MS: u64 = 10_000;
/// VAD segments are embedded in uniform sub-windows of this size rather
/// than whole. Long VAD segments span multiple speakers (observed 50 s+ on
/// YouTube dialog), and a mixed-voice embedding merges everyone into one
/// cluster — the standard fix (pyannote/VBx) is short uniform windows.
const EMBED_WINDOW_MS: u64 = 2_000;
/// A trailing remainder shorter than this merges into the previous window.
const EMBED_WINDOW_MIN_MS: u64 = 800;
/// Bound the retained segment set (a 4 h meeting stays well under this).
const MAX_SEGMENTS: usize = 6_000;

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

/// Timeline assembler looks speakers up through this so it does not care
/// which backend produced the ranges.
pub trait SpeakerLookup {
    fn label_for_span(&self, start_ms: u64, end_ms: u64) -> Option<SpeakerLabel>;
}

/// Label covering a time span. Overlapping distinct speakers yield
/// `MultipleSpeakers` unless one dominates (design §7).
pub fn label_for_span_in(
    ranges: &[SpeakerRange],
    start_ms: u64,
    end_ms: u64,
) -> Option<SpeakerLabel> {
    let mut overlaps: Vec<(&SpeakerRange, u64)> = ranges
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
    let distinct: std::collections::HashSet<String> =
        overlaps.iter().map(|(r, _)| r.label.display()).collect();
    if distinct.len() > 1 && (top_len as f64) < 0.7 * total as f64 {
        return Some(SpeakerLabel::MultipleSpeakers);
    }
    Some(top.label.clone())
}

// ---------------------------------------------------------------------------
// Embeddings and clustering
// ---------------------------------------------------------------------------

/// Voice-vector source. The sherpa backend and the fallback both implement
/// this; tests inject deterministic fakes.
pub trait EmbeddingExtractor: Send {
    fn embed(&mut self, samples: &[i16]) -> Vec<f32>;
}

/// Dependency-free fallback embedding: log-energy across a Goertzel filter
/// bank. Coarse, but keeps labels available offline.
pub struct SpectralBandExtractor {
    bands_hz: Vec<f32>,
}

impl Default for SpectralBandExtractor {
    fn default() -> Self {
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

/// One diarized speech segment retained for the life of the meeting.
/// 256 floats each — hours of audio stay in the kilobyte range.
struct SegRec {
    start_ms: u64,
    end_ms: u64,
    /// Unit-normalized voice embedding.
    embedding: Vec<f32>,
    /// Clustering weight (speech seconds, discounted for short segments).
    weight: f32,
    /// Long enough to seed a brand-new speaker.
    can_seed: bool,
}

/// Deferred-commitment clustering: every pass re-clusters *all* segments
/// globally, warm-started from the stable speaker centroids so speaker IDs
/// never flap. Past assignments are recomputed each pass — early mistakes
/// heal as context accumulates instead of poisoning the meeting.
pub struct GlobalClusterer {
    /// Speaker centroids; index = speaker ID - 1. May be renumbered by a
    /// merge/dissolve pass — safe because every pass relabels the entire
    /// timeline.
    centroids: Vec<Vec<f32>>,
    /// Last estimated threshold and population margin, for the debug log.
    pub last_threshold: f32,
    pub last_margin: f32,
}

/// Per-pass adaptive thresholds derived from the meeting's own similarity
/// distribution.
enum ThresholdEstimate {
    /// No reliable bimodal structure: treat as one speaker.
    Single,
    Split {
        join: f32,
        seed_below: f32,
        confident: f32,
    },
}

impl GlobalClusterer {
    fn new() -> Self {
        Self {
            centroids: Vec::new(),
            last_threshold: 0.0,
            last_margin: 0.0,
        }
    }

    /// Otsu-style split of pairwise similarities. Same-speaker and
    /// cross-speaker pairs form two populations; the threshold maximizing
    /// between-class variance sits in the gap — wherever this recording's
    /// channel put it.
    fn estimate_threshold(&mut self, segments: &[SegRec]) -> ThresholdEstimate {
        // Heaviest segments only; O(n²) pairs.
        let mut idx: Vec<usize> = (0..segments.len()).collect();
        idx.sort_by(|&a, &b| {
            segments[b]
                .weight
                .partial_cmp(&segments[a].weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        idx.truncate(THRESHOLD_SAMPLE);
        let mut sims: Vec<f32> = Vec::with_capacity(idx.len() * (idx.len() - 1) / 2);
        for i in 0..idx.len() {
            for j in (i + 1)..idx.len() {
                sims.push(cosine(
                    &segments[idx[i]].embedding,
                    &segments[idx[j]].embedding,
                ));
            }
        }
        if sims.len() < 3 {
            return ThresholdEstimate::Single;
        }
        sims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // Between-class variance maximization over sorted values.
        let total: f32 = sims.iter().sum();
        let n = sims.len() as f32;
        let mut best: Option<(usize, f32)> = None;
        let mut low_sum = 0.0f32;
        for (k, &s) in sims.iter().enumerate().take(sims.len() - 1) {
            low_sum += s;
            let n0 = (k + 1) as f32;
            let n1 = n - n0;
            let mu0 = low_sum / n0;
            let mu1 = (total - low_sum) / n1;
            let between = (n0 / n) * (n1 / n) * (mu1 - mu0) * (mu1 - mu0);
            if best.map(|(_, b)| between > b).unwrap_or(true) {
                best = Some((k, between));
            }
        }
        let Some((k, _)) = best else {
            return ThresholdEstimate::Single;
        };
        let n0 = k + 1;
        let mu0 = sims[..=k].iter().sum::<f32>() / n0 as f32;
        let mu1 = sims[k + 1..].iter().sum::<f32>() / (sims.len() - n0) as f32;
        let margin = mu1 - mu0;
        let share_low = n0 as f32 / n;
        self.last_margin = margin;
        // Weak separation, or one population is a sliver of noise pairs:
        // no trustworthy split.
        if margin < MIN_SPLIT_MARGIN || !(0.02..=0.98).contains(&share_low) {
            self.last_threshold = 0.0;
            return ThresholdEstimate::Single;
        }
        let t = (sims[k] + sims[k + 1]) / 2.0;
        self.last_threshold = t;
        ThresholdEstimate::Split {
            join: t,
            seed_below: t - 0.04,
            confident: (mu0 + 0.25 * margin).max(0.2),
        }
    }

    /// Global re-cluster. Returns one (label, confidence) per segment,
    /// in segment order.
    fn recluster(&mut self, segments: &[SegRec]) -> Vec<(SpeakerLabel, f32)> {
        if segments.is_empty() {
            return Vec::new();
        }
        let (join_t, seed_t, conf_t) = match self.estimate_threshold(segments) {
            ThresholdEstimate::Split {
                join,
                seed_below,
                confident,
            } => (join, seed_below, confident),
            ThresholdEstimate::Single => {
                // One speaker: weighted mean centroid, everything Speaker 1.
                let dims = segments[0].embedding.len();
                let mut mean = vec![0.0f32; dims];
                for seg in segments {
                    for (m, e) in mean.iter_mut().zip(&seg.embedding) {
                        *m += e * seg.weight;
                    }
                }
                normalize(&mut mean);
                let out = segments
                    .iter()
                    .map(|seg| (SpeakerLabel::Speaker(1), cosine(&seg.embedding, &mean)))
                    .collect();
                self.centroids = vec![mean];
                return out;
            }
        };
        // Warm start: k-means-style iterations seeded with the stable
        // centroids. Index identity is preserved across passes, which is
        // what keeps "Speaker 2" pointing at the same person all meeting.
        let mut working: Vec<Vec<f32>> = self.centroids.clone();
        let mut assignments: Vec<Option<usize>> = vec![None; segments.len()];

        // Heaviest segments first so new speakers are seeded from the most
        // reliable evidence.
        let mut order: Vec<usize> = (0..segments.len()).collect();
        order.sort_by(|&a, &b| {
            segments[b]
                .weight
                .partial_cmp(&segments[a].weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for _iter in 0..4 {
            let dims = segments[0].embedding.len();
            let mut sums: Vec<Vec<f32>> = vec![vec![0.0; dims]; working.len()];
            let mut weights: Vec<f32> = vec![0.0; working.len()];
            for &si in &order {
                let seg = &segments[si];
                let mut best: Option<(usize, f32)> = None;
                for (ci, c) in working.iter().enumerate() {
                    let sim = cosine(&seg.embedding, c);
                    if best.map(|(_, s)| sim > s).unwrap_or(true) {
                        best = Some((ci, sim));
                    }
                }
                let best_sim = best.map(|(_, s)| s).unwrap_or(-1.0);
                let assigned = match best {
                    Some((ci, sim)) if sim >= join_t => Some(ci),
                    // Hysteresis: seed a new speaker only when the segment is
                    // clearly unlike every existing one. In-between segments
                    // park on the nearest cluster (without moving it much —
                    // their weight is what it is) and confidence gating
                    // decides whether to trust the label.
                    _ if seg.can_seed
                        && best_sim < seed_t
                        && working.len() < MAX_CLUSTERS =>
                    {
                        working.push(seg.embedding.clone());
                        sums.push(vec![0.0; dims]);
                        weights.push(0.0);
                        Some(working.len() - 1)
                    }
                    Some((ci, _)) => Some(ci),
                    None => None,
                };
                assignments[si] = assigned;
                if let Some(ci) = assigned {
                    for (s, e) in sums[ci].iter_mut().zip(&seg.embedding) {
                        *s += e * seg.weight;
                    }
                    weights[ci] += seg.weight;
                }
            }
            // Update centroids; clusters with no evidence keep their old
            // centroid (a silent speaker must not be forgotten).
            for (ci, c) in working.iter_mut().enumerate() {
                if weights[ci] > 0.0 {
                    let mut nc = sums[ci].clone();
                    normalize(&mut nc);
                    if nc.iter().any(|v| *v != 0.0) {
                        *c = nc;
                    }
                }
            }
        }

        // Cluster weights from the final assignment.
        let mut totals: Vec<f32> = vec![0.0; working.len()];
        for (si, a) in assignments.iter().enumerate() {
            if let Some(ci) = a {
                totals[*ci] += segments[si].weight;
            }
        }

        // Merge pass over ALL centroid pairs (not just new-vs-old): without
        // it the cluster count only ever ratchets upward as near-duplicate
        // speakers accumulate across passes.
        let mut parent: Vec<usize> = (0..working.len()).collect();
        let resolve = |parent: &Vec<usize>, mut i: usize| -> usize {
            while parent[i] != i {
                i = parent[i];
            }
            i
        };
        loop {
            let mut best_pair: Option<(usize, usize, f32)> = None;
            for i in 0..working.len() {
                if resolve(&parent, i) != i {
                    continue;
                }
                for j in (i + 1)..working.len() {
                    if resolve(&parent, j) != j {
                        continue;
                    }
                    let sim = cosine(&working[i], &working[j]);
                    if sim >= join_t
                        && best_pair.map(|(_, _, s)| sim > s).unwrap_or(true)
                    {
                        best_pair = Some((i, j, sim));
                    }
                }
            }
            let Some((i, j, _)) = best_pair else { break };
            // Weighted merge of j into i.
            let (wi, wj) = (totals[i].max(1e-3), totals[j].max(1e-3));
            let merged: Vec<f32> = working[i]
                .iter()
                .zip(&working[j])
                .map(|(a, b)| a * wi + b * wj)
                .collect();
            let mut merged = merged;
            normalize(&mut merged);
            working[i] = merged;
            totals[i] += totals[j];
            parent[j] = i;
        }

        // Dissolve noise clusters: too little speech in absolute terms AND
        // a tiny share of the meeting. Their members fall back to the
        // nearest surviving cluster or to uncertainty.
        let grand: f32 = totals.iter().sum::<f32>().max(1e-3);
        let mut dissolved: Vec<bool> = vec![false; working.len()];
        for i in 0..working.len() {
            if resolve(&parent, i) != i {
                continue;
            }
            if totals[i] < MIN_CLUSTER_WEIGHT && totals[i] / grand < MIN_CLUSTER_SHARE {
                dissolved[i] = true;
            }
        }
        // Never dissolve everything.
        if (0..working.len()).all(|i| resolve(&parent, i) != i || dissolved[i]) {
            dissolved.iter_mut().for_each(|d| *d = false);
        }

        // Compact survivors into the new stable centroid list.
        let mut kept: Vec<Vec<f32>> = Vec::new();
        let mut final_index: Vec<Option<usize>> = vec![None; working.len()];
        for i in 0..working.len() {
            if resolve(&parent, i) == i && !dissolved[i] {
                final_index[i] = Some(kept.len());
                kept.push(working[i].clone());
            }
        }
        self.centroids = kept;

        segments
            .iter()
            .zip(&assignments)
            .map(|(seg, a)| {
                let root = (*a).map(|ci| resolve(&parent, ci));
                let fi = match root.and_then(|r| final_index[r]) {
                    Some(fi) => Some(fi),
                    // Dissolved/unassigned: nearest surviving cluster.
                    None => {
                        let mut best: Option<(usize, f32)> = None;
                        for (ci, c) in self.centroids.iter().enumerate() {
                            let sim = cosine(&seg.embedding, c);
                            if best.map(|(_, s)| sim > s).unwrap_or(true) {
                                best = Some((ci, sim));
                            }
                        }
                        best.filter(|(_, s)| *s >= conf_t)
                            .map(|(ci, _)| ci)
                    }
                };
                match fi {
                    Some(fi) => {
                        let sim = cosine(&seg.embedding, &self.centroids[fi]);
                        if sim >= conf_t {
                            (SpeakerLabel::Speaker(fi as u32 + 1), sim)
                        } else {
                            (SpeakerLabel::Meeting, sim.max(0.0))
                        }
                    }
                    None => (SpeakerLabel::Meeting, 0.0),
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Core (runs on the diarizer thread)
// ---------------------------------------------------------------------------

/// Maps VAD sample offsets back to session-clock milliseconds even when
/// chunks are withheld (e.g. during readout suppression).
struct SampleClock {
    /// (stream sample offset, session t_ms) checkpoints, one per chunk.
    marks: std::collections::VecDeque<(u64, u64)>,
    consumed: u64,
}

impl SampleClock {
    fn new() -> Self {
        Self {
            marks: std::collections::VecDeque::new(),
            consumed: 0,
        }
    }

    fn push_chunk(&mut self, len: usize, t_ms: u64) {
        self.marks.push_back((self.consumed, t_ms));
        self.consumed += len as u64;
        // ~1 h of 100 ms checkpoints is plenty.
        while self.marks.len() > 36_000 {
            self.marks.pop_front();
        }
    }

    fn to_ms(&self, sample_offset: u64) -> u64 {
        // Latest checkpoint at or before the offset.
        let mut base = (0u64, 0u64);
        for &(off, ms) in self.marks.iter() {
            if off <= sample_offset {
                base = (off, ms);
            } else {
                break;
            }
        }
        base.1 + (sample_offset - base.0) * 1000 / TARGET_SAMPLE_RATE as u64
    }
}

enum Backend {
    Sherpa {
        vad: sherpa_rs::silero_vad::SileroVad,
        extractor: sherpa_rs::speaker_id::EmbeddingExtractor,
        clock: SampleClock,
    },
    Fallback {
        extractor: Box<dyn EmbeddingExtractor>,
        seg_samples: Vec<i16>,
        seg_start_ms: Option<u64>,
        seg_last_speech_ms: u64,
    },
}

pub struct DiarizerCore {
    backend: Backend,
    clusterer: GlobalClusterer,
    segments: Vec<SegRec>,
    ranges: Arc<Mutex<Vec<SpeakerRange>>>,
    /// Bumped after every re-cluster; the session refreshes past transcript
    /// labels when it changes.
    version: Arc<std::sync::atomic::AtomicU64>,
    segments_since_recluster: usize,
    last_recluster_audio_ms: u64,
    /// Optional per-meeting trace (`diar-debug.log` in the data folder):
    /// one line per segment with the similarity score, for tuning. Never
    /// contains audio or transcript text.
    debug: Option<std::fs::File>,
}

impl DiarizerCore {
    fn new_sherpa(paths: &ModelPaths, ranges: Arc<Mutex<Vec<SpeakerRange>>>) -> anyhow::Result<Self> {
        // Short max-speech and silence windows: long VAD segments span
        // multiple speakers and produce one mixed embedding, which is what
        // made every voice cluster into "Speaker 1".
        let vad_config = sherpa_rs::silero_vad::SileroVadConfig {
            model: paths.vad.to_string_lossy().into_owned(),
            threshold: 0.5,
            min_silence_duration: 0.35,
            min_speech_duration: 0.25,
            max_speech_duration: 8.0,
            sample_rate: TARGET_SAMPLE_RATE,
            window_size: 512,
            ..Default::default()
        };
        let vad = sherpa_rs::silero_vad::SileroVad::new(vad_config, 60.0)
            .map_err(|e| anyhow::anyhow!("silero vad init: {e}"))?;
        let extractor =
            sherpa_rs::speaker_id::EmbeddingExtractor::new(sherpa_rs::speaker_id::ExtractorConfig {
                model: paths.speaker.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .map_err(|e| anyhow::anyhow!("speaker embedding init: {e}"))?;
        Ok(Self {
            backend: Backend::Sherpa {
                vad,
                extractor,
                clock: SampleClock::new(),
            },
            clusterer: GlobalClusterer::new(),
            segments: Vec::new(),
            ranges,
            version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            segments_since_recluster: 0,
            last_recluster_audio_ms: 0,
            debug: None,
        })
    }

    pub fn new_fallback(
        extractor: Box<dyn EmbeddingExtractor>,
        ranges: Arc<Mutex<Vec<SpeakerRange>>>,
    ) -> Self {
        Self {
            backend: Backend::Fallback {
                extractor,
                seg_samples: Vec::new(),
                seg_start_ms: None,
                seg_last_speech_ms: 0,
            },
            clusterer: GlobalClusterer::new(),
            segments: Vec::new(),
            ranges,
            version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            segments_since_recluster: 0,
            last_recluster_audio_ms: 0,
            debug: None,
        }
    }

    /// Add one diarized segment and re-cluster when due. Deferred
    /// commitment: labels for *all* segments are recomputed, so early
    /// assignments heal as more of each voice is heard.
    fn add_segment(&mut self, start_ms: u64, end_ms: u64, embedding: Vec<f32>) {
        let dur_ms = end_ms.saturating_sub(start_ms);
        let mut embedding = embedding;
        normalize(&mut embedding);
        let can_seed = dur_ms >= MIN_NEW_CLUSTER_MS;
        let weight = (dur_ms as f32 / 1000.0).min(10.0) * if can_seed { 1.0 } else { 0.25 };
        if let Some(f) = self.debug.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "seg {}..{}ms w={:.2} seed={}", start_ms, end_ms, weight, can_seed);
        }
        if self.segments.len() >= MAX_SEGMENTS {
            self.segments.remove(0);
        }
        self.segments.push(SegRec {
            start_ms,
            end_ms,
            embedding,
            weight,
            can_seed,
        });
        self.segments_since_recluster += 1;
        let due = self.segments_since_recluster >= RECLUSTER_EVERY_SEGMENTS
            || end_ms.saturating_sub(self.last_recluster_audio_ms) >= RECLUSTER_EVERY_MS;
        if due {
            self.recluster(end_ms);
        }
    }

    fn recluster(&mut self, audio_ms: u64) {
        self.segments_since_recluster = 0;
        self.last_recluster_audio_ms = audio_ms;
        let labels = self.clusterer.recluster(&self.segments);
        let rebuilt: Vec<SpeakerRange> = self
            .segments
            .iter()
            .zip(labels)
            .map(|(seg, (label, confidence))| SpeakerRange {
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                label,
                confidence,
            })
            .collect();
        if let Some(f) = self.debug.as_mut() {
            use std::io::Write;
            let speakers = self.clusterer.centroids.len();
            // Similarity distribution: decisive for threshold tuning.
            let mut sims: Vec<f32> = rebuilt.iter().map(|r| r.confidence).collect();
            sims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let (min, med, max) = (
                sims.first().copied().unwrap_or(0.0),
                sims.get(sims.len() / 2).copied().unwrap_or(0.0),
                sims.last().copied().unwrap_or(0.0),
            );
            let _ = writeln!(
                f,
                "recluster: {} segments -> {} speakers (sim min={min:.2} med={med:.2} max={max:.2})",
                rebuilt.len(),
                speakers
            );
        }
        *self.ranges.lock().unwrap() = rebuilt;
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn push_chunk(&mut self, samples: &[i16], t_ms: u64) {
        match &mut self.backend {
            Backend::Sherpa { vad, clock, .. } => {
                clock.push_chunk(samples.len(), t_ms);
                let f32s: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
                vad.accept_waveform(f32s);
                self.drain_sherpa_segments();
            }
            Backend::Fallback { .. } => self.fallback_push(samples, t_ms),
        }
    }

    pub fn finish(&mut self) {
        match &mut self.backend {
            Backend::Sherpa { vad, .. } => {
                vad.flush();
                self.drain_sherpa_segments();
            }
            Backend::Fallback { seg_start_ms, .. } => {
                if let Some(start) = *seg_start_ms {
                    self.fallback_close_segment(start);
                }
            }
        }
        // Final reconciliation: one full pass over everything.
        let last = self.segments.last().map(|s| s.end_ms).unwrap_or(0);
        self.recluster(last);
    }

    fn drain_sherpa_segments(&mut self) {
        let mut sealed: Vec<(u64, u64, Vec<f32>)> = Vec::new();
        {
            let Backend::Sherpa {
                vad,
                extractor,
                clock,
            } = &mut self.backend
            else {
                return;
            };
            while !vad.is_empty() {
                let segment = vad.front();
                vad.pop();
                let n = segment.samples.len() as u64;
                let start_ms = clock.to_ms(segment.start.max(0) as u64);
                let end_ms = start_ms + n * 1000 / TARGET_SAMPLE_RATE as u64;
                if end_ms.saturating_sub(start_ms) < MIN_SEGMENT_MS {
                    continue;
                }
                // Uniform sub-windows: one embedding per ~2 s so a long
                // multi-speaker VAD segment yields per-voice embeddings.
                let window = (EMBED_WINDOW_MS * TARGET_SAMPLE_RATE as u64 / 1000) as usize;
                let min_window =
                    (EMBED_WINDOW_MIN_MS * TARGET_SAMPLE_RATE as u64 / 1000) as usize;
                let samples = &segment.samples;
                let mut offset = 0usize;
                while offset < samples.len() {
                    let mut end = (offset + window).min(samples.len());
                    // Absorb a short trailing remainder into this window.
                    if samples.len() - end < min_window {
                        end = samples.len();
                    }
                    let w_start_ms =
                        start_ms + (offset as u64) * 1000 / TARGET_SAMPLE_RATE as u64;
                    let w_end_ms = start_ms + (end as u64) * 1000 / TARGET_SAMPLE_RATE as u64;
                    match extractor.compute_speaker_embedding(
                        samples[offset..end].to_vec(),
                        TARGET_SAMPLE_RATE,
                    ) {
                        Ok(e) => sealed.push((w_start_ms, w_end_ms, e)),
                        Err(e) => log::warn!("speaker embedding failed: {e}"),
                    }
                    offset = end;
                }
            }
        }
        for (start_ms, end_ms, embedding) in sealed {
            self.add_segment(start_ms, end_ms, embedding);
        }
    }

    fn fallback_push(&mut self, samples: &[i16], t_ms: u64) {
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
        let Backend::Fallback {
            seg_samples,
            seg_start_ms,
            seg_last_speech_ms,
            ..
        } = &mut self.backend
        else {
            return;
        };
        if rms > FB_SPEECH_RMS {
            if seg_start_ms.is_none() {
                *seg_start_ms = Some(t_ms);
            }
            seg_samples.extend_from_slice(samples);
            *seg_last_speech_ms = t_ms + chunk_ms;
        } else if let Some(start) = *seg_start_ms {
            if t_ms.saturating_sub(*seg_last_speech_ms) >= FB_SILENCE_CLOSE_MS {
                self.fallback_close_segment(start);
            }
        }
    }

    fn fallback_close_segment(&mut self, start_ms: u64) {
        let sealed = {
            let Backend::Fallback {
                extractor,
                seg_samples,
                seg_start_ms,
                seg_last_speech_ms,
            } = &mut self.backend
            else {
                return;
            };
            let end_ms = *seg_last_speech_ms;
            let samples = std::mem::take(seg_samples);
            *seg_start_ms = None;
            if end_ms.saturating_sub(start_ms) < MIN_SEGMENT_MS {
                return;
            }
            (start_ms, end_ms, extractor.embed(&samples))
        };
        self.add_segment(sealed.0, sealed.1, sealed.2);
    }
}

// ---------------------------------------------------------------------------
// Thread handle (owned by the session)
// ---------------------------------------------------------------------------

enum DiarCmd {
    Chunk(Vec<i16>, u64),
    Finish(mpsc::SyncSender<()>),
}

pub struct DiarizerHandle {
    tx: Option<mpsc::Sender<DiarCmd>>,
    ranges: Arc<Mutex<Vec<SpeakerRange>>>,
    version: Arc<std::sync::atomic::AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// True when running on sherpa-onnx models, false on the fallback.
    pub sherpa_active: bool,
}

impl DiarizerHandle {
    /// Spawn the diarizer thread. Uses sherpa-onnx when `models` is
    /// available and initialization succeeds; otherwise the fallback.
    /// `debug_log` enables the per-segment similarity trace.
    pub fn spawn(models: Option<ModelPaths>, debug_log: Option<std::path::PathBuf>) -> Self {
        let ranges: Arc<Mutex<Vec<SpeakerRange>>> = Arc::new(Mutex::new(Vec::new()));
        let version = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let version_thread = version.clone();
        let (tx, rx) = mpsc::channel::<DiarCmd>();
        let ranges_thread = ranges.clone();
        let (init_tx, init_rx) = mpsc::sync_channel::<bool>(1);
        let thread = std::thread::spawn(move || {
            let mut core = match models
                .as_ref()
                .map(|m| DiarizerCore::new_sherpa(m, ranges_thread.clone()))
            {
                Some(Ok(core)) => {
                    let _ = init_tx.send(true);
                    core
                }
                Some(Err(e)) => {
                    log::error!("sherpa diarizer init failed, using fallback: {e}");
                    let _ = init_tx.send(false);
                    DiarizerCore::new_fallback(
                        Box::new(SpectralBandExtractor::default()),
                        ranges_thread.clone(),
                    )
                }
                None => {
                    let _ = init_tx.send(false);
                    DiarizerCore::new_fallback(
                        Box::new(SpectralBandExtractor::default()),
                        ranges_thread.clone(),
                    )
                }
            };
            if let Some(path) = debug_log {
                core.debug = std::fs::File::create(path).ok();
            }
            core.version = version_thread;
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    DiarCmd::Chunk(samples, t_ms) => core.push_chunk(&samples, t_ms),
                    DiarCmd::Finish(ack) => {
                        core.finish();
                        let _ = ack.send(());
                        return;
                    }
                }
            }
        });
        let sherpa_active = init_rx.recv().unwrap_or(false);
        Self {
            tx: Some(tx),
            ranges,
            version,
            thread: Some(thread),
            sherpa_active,
        }
    }

    /// Monotonic counter bumped on every re-cluster. When it changes,
    /// previously sealed transcript entries should have their speaker
    /// labels recomputed.
    pub fn version(&self) -> u64 {
        self.version.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn push_chunk(&self, samples: &[i16], t_ms: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(DiarCmd::Chunk(samples.to_vec(), t_ms));
        }
    }

    /// (completed range count, label of the newest range). The session uses
    /// this to split the open timeline entry when the speaker changes.
    pub fn latest_range(&self) -> Option<(usize, SpeakerLabel)> {
        let ranges = self.ranges.lock().unwrap();
        ranges.last().map(|r| (ranges.len(), r.label.clone()))
    }

    /// Close the open segment and wait for the thread to drain.
    pub fn finish(&mut self) {
        if let Some(tx) = self.tx.take() {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            if tx.send(DiarCmd::Finish(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(std::time::Duration::from_secs(20));
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl SpeakerLookup for DiarizerHandle {
    fn label_for_span(&self, start_ms: u64, end_ms: u64) -> Option<SpeakerLabel> {
        label_for_span_in(&self.ranges.lock().unwrap(), start_ms, end_ms)
    }
}

impl Drop for DiarizerHandle {
    fn drop(&mut self) {
        self.finish();
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

    fn fallback_core() -> (DiarizerCore, Arc<Mutex<Vec<SpeakerRange>>>) {
        let ranges: Arc<Mutex<Vec<SpeakerRange>>> = Arc::new(Mutex::new(Vec::new()));
        (
            DiarizerCore::new_fallback(Box::new(FakeExtractor), ranges.clone()),
            ranges,
        )
    }

    fn speech_chunk(level: i16) -> Vec<i16> {
        (0..CHUNK_SAMPLES)
            .map(|i| if i % 2 == 0 { level } else { -level })
            .collect()
    }

    fn silence_chunk() -> Vec<i16> {
        vec![0; CHUNK_SAMPLES]
    }

    fn run_segments(d: &mut DiarizerCore, levels: &[i16]) {
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
        let (mut d, ranges) = fallback_core();
        run_segments(&mut d, &[5000, 5000]);
        d.finish();
        let ranges = ranges.lock().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].label, ranges[1].label);
    }

    #[test]
    fn different_voices_get_different_labels() {
        let (mut d, ranges) = fallback_core();
        run_segments(&mut d, &[2000, 20000, 2000]);
        d.finish();
        let ranges = ranges.lock().unwrap();
        assert_eq!(ranges.len(), 3);
        assert_ne!(ranges[0].label, ranges[1].label);
        assert_eq!(ranges[0].label, ranges[2].label);
    }

    #[test]
    fn short_blips_ignored() {
        let (mut d, ranges) = fallback_core();
        d.push_chunk(&speech_chunk(5000), 0);
        d.push_chunk(&speech_chunk(5000), 100);
        for i in 0..10 {
            d.push_chunk(&silence_chunk(), 200 + i * 100);
        }
        d.finish();
        assert!(ranges.lock().unwrap().is_empty());
    }

    #[test]
    fn span_labeling_picks_dominant() {
        let (mut d, ranges) = fallback_core();
        run_segments(&mut d, &[5000]);
        d.finish();
        let ranges = ranges.lock().unwrap();
        let label = label_for_span_in(&ranges, 0, 1000).expect("label");
        assert_eq!(label, SpeakerLabel::Speaker(1));
        assert!(label_for_span_in(&ranges, 500_000, 501_000).is_none());
    }

    #[test]
    fn overlap_of_distinct_speakers_is_multiple() {
        let ranges = vec![
            SpeakerRange {
                start_ms: 0,
                end_ms: 1000,
                label: SpeakerLabel::Speaker(1),
                confidence: 1.0,
            },
            SpeakerRange {
                start_ms: 400,
                end_ms: 1400,
                label: SpeakerLabel::Speaker(2),
                confidence: 1.0,
            },
        ];
        assert_eq!(
            label_for_span_in(&ranges, 0, 1400),
            Some(SpeakerLabel::MultipleSpeakers)
        );
    }

    #[test]
    fn sample_clock_survives_gaps() {
        let mut clock = SampleClock::new();
        clock.push_chunk(1600, 0); // 0..100 ms
        clock.push_chunk(1600, 100); // 100..200 ms
        // 5 s gap (suppressed chunks), stream continues at 5200 ms.
        clock.push_chunk(1600, 5200);
        assert_eq!(clock.to_ms(0), 0);
        assert_eq!(clock.to_ms(1600), 100);
        assert_eq!(clock.to_ms(3200), 5200);
        assert_eq!(clock.to_ms(4000), 5250);
    }

    /// Ground-truth check of the sherpa embedding path with real speaker
    /// audio. Run explicitly:
    /// `SALLY_TEST_MODEL=<path to speaker onnx> SALLY_TEST_WAVS=<sr-data dir> cargo test verify_embedding -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn verify_embedding_discrimination() {
        let model = std::env::var("SALLY_TEST_MODEL").expect("SALLY_TEST_MODEL");
        let wavs = std::path::PathBuf::from(
            std::env::var("SALLY_TEST_WAVS").expect("SALLY_TEST_WAVS"),
        );
        let mut extractor = sherpa_rs::speaker_id::EmbeddingExtractor::new(
            sherpa_rs::speaker_id::ExtractorConfig {
                model,
                ..Default::default()
            },
        )
        .expect("extractor");
        let mut embed = |name: &str| -> Vec<f32> {
            let (samples, rate) =
                sherpa_rs::read_audio_file(&wavs.join(name).to_string_lossy())
                    .expect("read wav");
            let mut e = extractor
                .compute_speaker_embedding(samples, rate)
                .expect("embedding");
            normalize(&mut e);
            e
        };
        let f1 = embed("enroll/fangjun-sr-1.wav");
        let f2 = embed("enroll/fangjun-sr-2.wav");
        let l1 = embed("enroll/leijun-sr-1.wav");
        let d1 = embed("enroll/liudehua-sr-1.wav");
        let same = cosine(&f1, &f2);
        let diff1 = cosine(&f1, &l1);
        let diff2 = cosine(&f1, &d1);
        let diff3 = cosine(&l1, &d1);
        println!("same-speaker sim: {same:.3}");
        println!("diff-speaker sims: {diff1:.3} {diff2:.3} {diff3:.3}");
        assert!(same > 0.5, "same-speaker similarity too low: {same}");
        // Thresholds are adaptive now; require a usable margin between the
        // populations instead of comparing to a fixed constant.
        let max_diff = diff1.max(diff2).max(diff3);
        assert!(
            same - max_diff > MIN_SPLIT_MARGIN,
            "same/different margin too small: {same} vs {max_diff}"
        );
    }

    #[test]
    fn handle_runs_fallback_thread() {
        let mut handle = DiarizerHandle::spawn(None, None);
        assert!(!handle.sherpa_active);
        let mut t = 0u64;
        for _ in 0..10 {
            handle.push_chunk(&speech_chunk(5000), t);
            t += 100;
        }
        for _ in 0..10 {
            handle.push_chunk(&silence_chunk(), t);
            t += 100;
        }
        handle.finish();
        assert_eq!(
            handle.label_for_span(0, 1000),
            Some(SpeakerLabel::Speaker(1))
        );
        assert!(handle.version() >= 1, "finish must run a reconciliation pass");
    }

    #[test]
    fn recluster_is_global_and_labels_stay_stable() {
        let (mut d, ranges) = fallback_core();
        // Voice A, voice B, then voice A again across many segments.
        run_segments(&mut d, &[5000, 20000, 5000, 20000, 5000]);
        d.finish();
        let ranges = ranges.lock().unwrap();
        assert_eq!(ranges.len(), 5);
        assert_eq!(ranges[0].label, ranges[2].label);
        assert_eq!(ranges[2].label, ranges[4].label);
        assert_eq!(ranges[1].label, ranges[3].label);
        assert_ne!(ranges[0].label, ranges[1].label);
    }
}
