//! Speaker diarization (design §7), always on.
//!
//! Primary backend: the sherpa-onnx *offline speaker diarization* pipeline —
//! a pyannote segmentation model finds who-spoke-when regions (including
//! overlapped speech), a speaker-embedding model (NeMo TitaNet) turns each
//! region into a voice vector, and complete-linkage agglomerative clustering
//! with a fixed cosine-distance threshold groups the vectors into
//! `Speaker N` labels. Model files are fetched by `models::ensure_models`.
//!
//! Live meetings are diarized by re-running the pipeline over the whole
//! buffered audio on a self-tuning cadence (each pass's cost sets the next
//! interval), plus one final pass at meeting end. Every pass relabels the
//! entire timeline, so early mistakes heal as context accumulates; speaker
//! identities are kept stable across passes by overlap matching against the
//! previous pass's ranges.
//!
//! If the models are unavailable (offline first run), a dependency-free
//! fallback (energy VAD + spectral-band profile + online fixed-threshold
//! clustering) keeps meetings working with coarser labels.
//!
//! sherpa-onnx handles hold raw FFI pointers, so the whole diarizer runs on
//! its own thread; the session talks to it through a channel and reads
//! completed ranges from shared state.

use crate::audio::TARGET_SAMPLE_RATE;
use crate::models::ModelPaths;
use serde::Serialize;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Segments shorter than this are ignored as noise (fallback backend).
const MIN_SEGMENT_MS: u64 = 400;

// Fallback (energy VAD + spectral profile) tuning.
const FB_SILENCE_CLOSE_MS: u64 = 600;
const FB_SPEECH_RMS: f32 = 0.010;
/// Fallback online clustering: join the nearest cluster at or above this
/// cosine similarity, otherwise start a new one. Spectral-band profiles of
/// the same voice correlate strongly; distinct voices land well below.
const FB_JOIN_SIM: f32 = 0.80;
const MAX_CLUSTERS: usize = 8;

// Sherpa pipeline tuning.
/// Default clustering cosine-*distance* threshold (larger = fewer
/// speakers). TitaNet measures same-speaker distance ~0.27 and
/// cross-speaker ~0.81 on clean audio; 0.5 sits between them.
/// Overridable via `SALLY_DIAR_THRESHOLD` for unusual channels.
pub const DEFAULT_CLUSTER_THRESHOLD: f32 = 0.5;
/// pyannote post-processing: speech shorter than this is dropped, silences
/// shorter than this are bridged (values from the sherpa-onnx examples).
const SEG_MIN_DURATION_ON: f32 = 0.3;
const SEG_MIN_DURATION_OFF: f32 = 0.5;
/// Diarization passes never run closer together than this...
const MIN_PASS_INTERVAL_MS: u64 = 10_000;
/// ...nor further apart than this.
const MAX_PASS_INTERVAL_MS: u64 = 300_000;
/// Each pass re-processes the whole meeting, so its cost grows with
/// meeting length. The next pass is scheduled this many times the last
/// pass's wall-clock cost away, keeping the diarizer's duty cycle bounded.
const PASS_COST_FACTOR: u64 = 4;
/// Retained-audio cap (4 h at 16 kHz mono i16 ≈ 460 MB). Diarization
/// stops taking new audio past this; the meeting itself is unaffected.
const MAX_BUFFER_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 3600 * 4;

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
// Speaker identity stability across passes
// ---------------------------------------------------------------------------

/// Map one pass's raw speaker ids to stable `Speaker N` labels by greedy
/// best-overlap matching against the previous pass's ranges. The pipeline
/// numbers clusters arbitrarily on every pass; without this, "Speaker 2"
/// could point at a different person after each re-cluster. Unmatched ids
/// (genuinely new voices) get fresh labels in order of first appearance.
fn remap_speakers(
    prev: &[SpeakerRange],
    segs: &[(u64, u64, i32)],
) -> std::collections::HashMap<i32, u32> {
    use std::collections::{HashMap, HashSet};
    let mut votes: HashMap<(i32, u32), u64> = HashMap::new();
    for &(s, e, id) in segs {
        for r in prev {
            if let SpeakerLabel::Speaker(k) = r.label {
                let os = r.start_ms.max(s);
                let oe = r.end_ms.min(e);
                if oe > os {
                    *votes.entry((id, k)).or_default() += oe - os;
                }
            }
        }
    }
    let mut pairs: Vec<((i32, u32), u64)> = votes.into_iter().collect();
    // Secondary key keeps ties deterministic.
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut mapping: HashMap<i32, u32> = HashMap::new();
    let mut used: HashSet<u32> = HashSet::new();
    for ((id, k), _) in pairs {
        if !mapping.contains_key(&id) && !used.contains(&k) {
            mapping.insert(id, k);
            used.insert(k);
        }
    }
    let mut next = prev
        .iter()
        .filter_map(|r| match r.label {
            SpeakerLabel::Speaker(k) => Some(k),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;
    // segs arrive sorted by start time, so fresh labels follow first
    // appearance: the first voice heard becomes Speaker 1.
    for &(_, _, id) in segs {
        if let std::collections::hash_map::Entry::Vacant(e) = mapping.entry(id) {
            e.insert(next);
            next += 1;
        }
    }
    mapping
}

// ---------------------------------------------------------------------------
// Fallback embeddings and clustering
// ---------------------------------------------------------------------------

/// Voice-vector source for the fallback; tests inject deterministic fakes.
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

/// Fallback online clustering: nearest centroid above a fixed similarity
/// joins, anything else seeds a new speaker (capped). Labels are 1-based.
struct OnlineClusterer {
    centroids: Vec<Vec<f32>>,
    weights: Vec<f32>,
}

impl OnlineClusterer {
    fn new() -> Self {
        Self {
            centroids: Vec::new(),
            weights: Vec::new(),
        }
    }

    fn assign(&mut self, embedding: &[f32]) -> (u32, f32) {
        let mut best: Option<(usize, f32)> = None;
        for (ci, c) in self.centroids.iter().enumerate() {
            let sim = cosine(embedding, c);
            if best.map(|(_, s)| sim > s).unwrap_or(true) {
                best = Some((ci, sim));
            }
        }
        match best {
            Some((ci, sim)) if sim >= FB_JOIN_SIM || self.centroids.len() >= MAX_CLUSTERS => {
                let w = self.weights[ci];
                for (c, e) in self.centroids[ci].iter_mut().zip(embedding) {
                    *c = (*c * w + e) / (w + 1.0);
                }
                self.weights[ci] += 1.0;
                normalize(&mut self.centroids[ci]);
                (ci as u32 + 1, sim.max(0.0))
            }
            _ => {
                self.centroids.push(embedding.to_vec());
                self.weights.push(1.0);
                (self.centroids.len() as u32, 1.0)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Core (runs on the diarizer thread)
// ---------------------------------------------------------------------------

/// Maps buffered sample offsets back to session-clock milliseconds even when
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
        // Every pass maps the whole buffer, so checkpoints must span the
        // entire retained audio: ~5.5 h of 100 ms chunks (a few MB).
        while self.marks.len() > 200_000 {
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
        diarize: sherpa_rs::diarize::Diarize,
        /// Whole-meeting remote audio, 16 kHz mono. Each pass re-diarizes
        /// all of it (deferred commitment — labels heal retroactively).
        buffer: Vec<i16>,
        clock: SampleClock,
    },
    Fallback {
        extractor: Box<dyn EmbeddingExtractor>,
        clusterer: OnlineClusterer,
        seg_samples: Vec<i16>,
        seg_start_ms: Option<u64>,
        seg_last_speech_ms: u64,
    },
}

pub struct DiarizerCore {
    backend: Backend,
    ranges: Arc<Mutex<Vec<SpeakerRange>>>,
    /// Bumped after every pass; the session refreshes past transcript
    /// labels when it changes.
    version: Arc<std::sync::atomic::AtomicU64>,
    /// Audio duration at the last pipeline pass.
    last_pass_audio_ms: u64,
    /// Self-tuning gap between passes (grows with pass cost).
    pass_interval_ms: u64,
    buffer_full_warned: bool,
    /// Optional per-meeting trace (`diar-debug.log` in the data folder):
    /// one line per pass with timing and speaker counts, for tuning. Never
    /// contains audio or transcript text.
    debug: Option<std::fs::File>,
}

impl DiarizerCore {
    fn new_sherpa(
        paths: &ModelPaths,
        ranges: Arc<Mutex<Vec<SpeakerRange>>>,
        cluster_threshold: f32,
    ) -> anyhow::Result<Self> {
        let diarize = sherpa_rs::diarize::Diarize::new(
            &paths.segmentation,
            &paths.speaker,
            sherpa_rs::diarize::DiarizeConfig {
                // <= 0 means "unknown speaker count": cut the cluster tree
                // at the distance threshold instead.
                num_clusters: Some(-1),
                threshold: Some(cluster_threshold),
                min_duration_on: Some(SEG_MIN_DURATION_ON),
                min_duration_off: Some(SEG_MIN_DURATION_OFF),
                provider: None,
                debug: false,
            },
        )
        .map_err(|e| anyhow::anyhow!("offline speaker diarization init: {e}"))?;
        Ok(Self {
            backend: Backend::Sherpa {
                diarize,
                buffer: Vec::new(),
                clock: SampleClock::new(),
            },
            ranges,
            version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_pass_audio_ms: 0,
            pass_interval_ms: MIN_PASS_INTERVAL_MS,
            buffer_full_warned: false,
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
                clusterer: OnlineClusterer::new(),
                seg_samples: Vec::new(),
                seg_start_ms: None,
                seg_last_speech_ms: 0,
            },
            ranges,
            version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_pass_audio_ms: 0,
            pass_interval_ms: MIN_PASS_INTERVAL_MS,
            buffer_full_warned: false,
            debug: None,
        }
    }

    pub fn push_chunk(&mut self, samples: &[i16], t_ms: u64) {
        let mut due = false;
        match &mut self.backend {
            Backend::Sherpa { buffer, clock, .. } => {
                clock.push_chunk(samples.len(), t_ms);
                if buffer.len() + samples.len() <= MAX_BUFFER_SAMPLES {
                    buffer.extend_from_slice(samples);
                } else if !self.buffer_full_warned {
                    self.buffer_full_warned = true;
                    log::warn!("diarization audio buffer full; labels frozen from here on");
                }
                let audio_ms = buffer.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64;
                due = audio_ms.saturating_sub(self.last_pass_audio_ms) >= self.pass_interval_ms;
            }
            Backend::Fallback { .. } => self.fallback_push(samples, t_ms),
        }
        if due {
            self.run_sherpa_pass();
        }
    }

    pub fn finish(&mut self) {
        match &mut self.backend {
            Backend::Sherpa { .. } => {
                // Final reconciliation over everything heard.
                self.run_sherpa_pass();
            }
            Backend::Fallback { seg_start_ms, .. } => {
                if let Some(start) = *seg_start_ms {
                    self.fallback_close_segment(start);
                }
            }
        }
        // Always signal the session to refresh labels once at the end.
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// One full pipeline pass over the buffered audio: segmentation,
    /// embeddings, clustering, then wholesale range replacement with
    /// pass-to-pass identity remapping.
    fn run_sherpa_pass(&mut self) {
        let Backend::Sherpa {
            diarize,
            buffer,
            clock,
        } = &mut self.backend
        else {
            return;
        };
        if buffer.is_empty() {
            return;
        }
        let audio_ms = buffer.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64;
        self.last_pass_audio_ms = audio_ms;
        let started = std::time::Instant::now();
        let samples: Vec<f32> = buffer.iter().map(|&s| s as f32 / 32768.0).collect();
        let result = diarize.compute(samples, None);
        let cost_ms = started.elapsed().as_millis() as u64;
        // Duty-cycle bound: a pass that took N seconds schedules the next
        // one at least 4N away, so late-meeting passes stay affordable.
        self.pass_interval_ms =
            (cost_ms * PASS_COST_FACTOR).clamp(MIN_PASS_INTERVAL_MS, MAX_PASS_INTERVAL_MS);
        match result {
            Ok(raw) => {
                let segs: Vec<(u64, u64, i32)> = raw
                    .iter()
                    .filter_map(|s| {
                        let a = clock
                            .to_ms((s.start.max(0.0) as f64 * TARGET_SAMPLE_RATE as f64) as u64);
                        let b = clock
                            .to_ms((s.end.max(0.0) as f64 * TARGET_SAMPLE_RATE as f64) as u64);
                        (b > a).then_some((a, b, s.speaker))
                    })
                    .collect();
                let mut ranges = self.ranges.lock().unwrap();
                let mapping = remap_speakers(&ranges, &segs);
                let speakers: std::collections::HashSet<u32> =
                    mapping.values().copied().collect();
                *ranges = segs
                    .iter()
                    .map(|&(a, b, id)| SpeakerRange {
                        start_ms: a,
                        end_ms: b,
                        label: SpeakerLabel::Speaker(mapping[&id]),
                        confidence: 1.0,
                    })
                    .collect();
                drop(ranges);
                if let Some(f) = self.debug.as_mut() {
                    use std::io::Write;
                    let _ = writeln!(
                        f,
                        "pass: {:.1}s audio -> {} segments, {} speakers in {}ms (next in {}s)",
                        audio_ms as f64 / 1000.0,
                        segs.len(),
                        speakers.len(),
                        cost_ms,
                        self.pass_interval_ms / 1000,
                    );
                }
                self.version
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Err(e) => {
                // Zero detected segments (early silence) also lands here;
                // keep the previous ranges either way.
                if let Some(f) = self.debug.as_mut() {
                    use std::io::Write;
                    let _ = writeln!(
                        f,
                        "pass: {:.1}s audio -> no result in {}ms ({e})",
                        audio_ms as f64 / 1000.0,
                        cost_ms,
                    );
                }
            }
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
            clusterer,
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
        let (label, confidence) = clusterer.assign(&embedding);
        if let Some(f) = self.debug.as_mut() {
            use std::io::Write;
            let _ = writeln!(
                f,
                "fallback seg {start_ms}..{end_ms}ms -> Speaker {label} (sim {confidence:.2})"
            );
        }
        self.ranges.lock().unwrap().push(SpeakerRange {
            start_ms,
            end_ms,
            label: SpeakerLabel::Speaker(label),
            confidence,
        });
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    /// `debug_log` enables the per-pass trace; `cluster_threshold` is the
    /// clustering cosine-distance cut (larger = fewer speakers).
    pub fn spawn(
        models: Option<ModelPaths>,
        debug_log: Option<std::path::PathBuf>,
        cluster_threshold: f32,
    ) -> Self {
        let ranges: Arc<Mutex<Vec<SpeakerRange>>> = Arc::new(Mutex::new(Vec::new()));
        let version = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let version_thread = version.clone();
        let (tx, rx) = mpsc::channel::<DiarCmd>();
        let ranges_thread = ranges.clone();
        let (init_tx, init_rx) = mpsc::sync_channel::<bool>(1);
        let thread = std::thread::spawn(move || {
            let mut core = match models
                .as_ref()
                .map(|m| DiarizerCore::new_sherpa(m, ranges_thread.clone(), cluster_threshold))
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

    /// Monotonic counter bumped on every pass. When it changes, previously
    /// sealed transcript entries should have their speaker labels
    /// recomputed.
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

    /// Close the open segment and wait for the thread to drain. The final
    /// pass re-diarizes the whole meeting, so the wait scales with meeting
    /// length (bounded by the timeout).
    pub fn finish(&mut self) {
        if let Some(tx) = self.tx.take() {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            if tx.send(DiarCmd::Finish(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(std::time::Duration::from_secs(300));
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

    // ---- Pass-to-pass speaker identity ----------------------------------

    fn range(start_ms: u64, end_ms: u64, n: u32) -> SpeakerRange {
        SpeakerRange {
            start_ms,
            end_ms,
            label: SpeakerLabel::Speaker(n),
            confidence: 1.0,
        }
    }

    #[test]
    fn first_pass_labels_by_appearance_order() {
        // No previous ranges: pipeline ids (arbitrary numbers) map to
        // Speaker 1, 2, ... in order of first appearance.
        let mapping = remap_speakers(
            &[],
            &[(0, 2000, 7), (2000, 4000, 3), (4000, 6000, 7)],
        );
        assert_eq!(mapping[&7], 1);
        assert_eq!(mapping[&3], 2);
    }

    #[test]
    fn renumbered_pipeline_ids_keep_stable_labels() {
        // Previous pass: Speaker 1 owned 0-10 s, Speaker 2 owned 10-20 s.
        // New pass returns the same people but with swapped raw ids; the
        // overlap vote must keep each person's label.
        let prev = vec![range(0, 10_000, 1), range(10_000, 20_000, 2)];
        let mapping = remap_speakers(
            &prev,
            &[(0, 9_000, 1), (10_000, 19_000, 0), (20_000, 22_000, 1)],
        );
        assert_eq!(mapping[&1], 1, "person in 0-10s must stay Speaker 1");
        assert_eq!(mapping[&0], 2, "person in 10-20s must stay Speaker 2");
    }

    #[test]
    fn new_voice_gets_fresh_label() {
        let prev = vec![range(0, 10_000, 1), range(10_000, 20_000, 2)];
        let mapping = remap_speakers(
            &prev,
            &[(0, 9_000, 0), (10_000, 19_000, 1), (20_000, 25_000, 2)],
        );
        assert_eq!(mapping[&0], 1);
        assert_eq!(mapping[&1], 2);
        assert_eq!(mapping[&2], 3, "unseen voice must get the next label");
    }

    #[test]
    fn fallback_labels_stay_stable_across_alternation() {
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
        // Same-speaker cosine distance must sit clearly inside the
        // clustering threshold and cross-speaker clearly outside it.
        let max_diff = diff1.max(diff2).max(diff3);
        assert!(
            1.0 - same < DEFAULT_CLUSTER_THRESHOLD,
            "same-speaker distance crosses the clustering threshold: {same}"
        );
        assert!(
            1.0 - max_diff > DEFAULT_CLUSTER_THRESHOLD,
            "cross-speaker distance inside the clustering threshold: {max_diff}"
        );
    }

    #[test]
    fn handle_runs_fallback_thread() {
        let mut handle = DiarizerHandle::spawn(None, None, DEFAULT_CLUSTER_THRESHOLD);
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
}
