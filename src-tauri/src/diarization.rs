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
const FB_JOIN_SIMILARITY: f32 = 0.86;
const FB_CONFIDENT_SIMILARITY: f32 = 0.55;

// sherpa speaker-embedding tuning (ERes2Net cosine scores).
const SHERPA_JOIN_SIMILARITY: f32 = 0.55;
const SHERPA_CONFIDENT_SIMILARITY: f32 = 0.30;

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

struct Cluster {
    centroid: Vec<f32>,
    count: u32,
}

/// Online centroid clustering with per-backend thresholds.
pub struct ClusterEngine {
    clusters: Vec<Cluster>,
    join_threshold: f32,
    confident_threshold: f32,
}

impl ClusterEngine {
    pub fn new(join_threshold: f32, confident_threshold: f32) -> Self {
        Self {
            clusters: Vec::new(),
            join_threshold,
            confident_threshold,
        }
    }

    pub fn assign(&mut self, embedding: &[f32]) -> (SpeakerLabel, f32) {
        // Normalize so centroid averaging is not biased toward loud/long
        // segments (raw model embeddings are not unit-length).
        let mut embedding = embedding.to_vec();
        normalize(&mut embedding);
        let embedding = &embedding[..];
        let mut best: Option<(usize, f32)> = None;
        for (i, c) in self.clusters.iter().enumerate() {
            let sim = cosine(embedding, &c.centroid);
            if best.map(|(_, s)| sim > s).unwrap_or(true) {
                best = Some((i, sim));
            }
        }
        match best {
            Some((i, sim)) if sim >= self.join_threshold => {
                let c = &mut self.clusters[i];
                let n = c.count as f32;
                for (cx, ex) in c.centroid.iter_mut().zip(embedding) {
                    *cx = (*cx * n + ex) / (n + 1.0);
                }
                c.count += 1;
                (SpeakerLabel::Speaker(i as u32 + 1), sim)
            }
            Some((_, sim)) if sim < self.confident_threshold && !self.clusters.is_empty() => {
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
    engine: ClusterEngine,
    ranges: Arc<Mutex<Vec<SpeakerRange>>>,
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
            engine: ClusterEngine::new(SHERPA_JOIN_SIMILARITY, SHERPA_CONFIDENT_SIMILARITY),
            ranges,
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
            engine: ClusterEngine::new(FB_JOIN_SIMILARITY, FB_CONFIDENT_SIMILARITY),
            ranges,
        }
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
    }

    fn drain_sherpa_segments(&mut self) {
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
            let embedding = match extractor
                .compute_speaker_embedding(segment.samples, TARGET_SAMPLE_RATE)
            {
                Ok(e) => e,
                Err(e) => {
                    log::warn!("speaker embedding failed: {e}");
                    continue;
                }
            };
            let (label, confidence) = self.engine.assign(&embedding);
            self.ranges.lock().unwrap().push(SpeakerRange {
                start_ms,
                end_ms,
                label,
                confidence,
            });
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
        let embedding = extractor.embed(&samples);
        let (label, confidence) = self.engine.assign(&embedding);
        self.ranges.lock().unwrap().push(SpeakerRange {
            start_ms,
            end_ms,
            label,
            confidence,
        });
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
    thread: Option<std::thread::JoinHandle<()>>,
    /// True when running on sherpa-onnx models, false on the fallback.
    pub sherpa_active: bool,
}

impl DiarizerHandle {
    /// Spawn the diarizer thread. Uses sherpa-onnx when `models` is
    /// available and initialization succeeds; otherwise the fallback.
    pub fn spawn(models: Option<ModelPaths>) -> Self {
        let ranges: Arc<Mutex<Vec<SpeakerRange>>> = Arc::new(Mutex::new(Vec::new()));
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
            thread: Some(thread),
            sherpa_active,
        }
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
        run_segments(&mut d, &[2000, 20000]);
        d.finish();
        let ranges = ranges.lock().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_ne!(ranges[0].label, ranges[1].label);
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

    #[test]
    fn handle_runs_fallback_thread() {
        let mut handle = DiarizerHandle::spawn(None);
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
    }
}
