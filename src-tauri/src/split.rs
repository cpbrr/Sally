//! Speaker-change line splitting (segmentation only, no identity).
//!
//! A pyannote segmentation model runs over a sliding 10 s window of the
//! system-audio lane and reports *boundaries* — moments where one remote
//! voice hands off to another. The session rotates the open timeline entry
//! at each boundary so consecutive remote speakers land on separate lines.
//!
//! This is deliberately not diarization: no embeddings, no clustering, no
//! persistent speaker identities, no end-of-meeting reconciliation. Those
//! were removed in v0.6.0 after six failed tuning releases. The model's
//! per-window local speaker tracks are used only to answer "did the voice
//! change here", and every remote line keeps the label "Meeting".

use crate::error::{Result, SallyError};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

pub const MODEL_FILE: &str = "segmentation_pyannote3.onnx";
// pyannote segmentation-3.0 converted for onnx inference, ~6 MB. Same
// mirror the sherpa era used: serves the bare .onnx, no archive.
const MODEL_URL: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx";

const SAMPLE_RATE: usize = 16_000;
const WINDOW_MS: u64 = 10_000;
const WINDOW_SAMPLES: usize = SAMPLE_RATE * (WINDOW_MS as usize / 1000);
/// Re-run the model after this much new audio.
const HOP_MS: u64 = 500;
const HOP_SAMPLES: usize = SAMPLE_RATE * HOP_MS as usize / 1000;
/// Windows quieter than this RMS are skipped (nobody is talking).
const WINDOW_RMS_GATE: f32 = 0.004;

/// Both sides of a boundary must be a single sustained voice this long.
/// Short interjections ("mm-hm") stay inside the current line.
const MIN_RUN_MS: f64 = 800.0;
/// Silence or overlapped speech allowed between the two voices; longer
/// pauses are left to Gemini's own turn handling.
const MAX_GAP_MS: f64 = 900.0;
/// Minimum spacing between emitted boundaries. Also deduplicates the same
/// boundary re-detected by later overlapping windows.
const REFRACTORY_MS: u64 = 2_500;
/// Majority-filter width in frames (~17 ms each), ~150 ms.
const SMOOTH_FRAMES: usize = 9;

pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Ensure the segmentation model exists, downloading it on first use.
/// `url_override` supports air-gapped setups exactly like the old
/// SALLY_SEGMENTATION_MODEL_URL did: drop the file in manually and no
/// download happens.
pub async fn ensure_model(data_dir: &Path, url_override: &str) -> Result<PathBuf> {
    let dir = models_dir(data_dir);
    let dest = dir.join(MODEL_FILE);
    if dest.exists() {
        return Ok(dest);
    }
    std::fs::create_dir_all(&dir)?;
    let url = if url_override.is_empty() {
        MODEL_URL
    } else {
        url_override
    };
    log::info!("downloading segmentation model: {url}");
    let resp = reqwest::get(url)
        .await
        .map_err(|e| SallyError::Config(format!("model download failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(SallyError::Config(format!(
            "model download failed: HTTP {} for {url}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| SallyError::Config(format!("model download failed: {e}")))?;
    // Temp file + rename so a cut connection never leaves a truncated model
    // that would fail to load forever after.
    let tmp = dest.with_extension("onnx.part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(dest)
}

/// One 100 ms slice of the system lane. `suppress` replaces the samples
/// with silence (used while readout playback is audible, so our own spoken
/// translation in the loopback is never mistaken for a new speaker).
pub struct Feed {
    pub samples: Vec<f32>,
    pub t_ms: u64,
    pub suppress: bool,
}

/// Handle owned by the session. Dropping it stops the worker thread.
pub struct SplitDetector {
    pub audio_tx: std::sync::mpsc::Sender<Feed>,
    pub boundary_rx: mpsc::Receiver<u64>,
}

impl SplitDetector {
    /// Spawn the worker thread and load the model on it. Returns once the
    /// model loaded (or failed to).
    pub fn start(model_path: &Path) -> Result<SplitDetector> {
        let (audio_tx, audio_rx) = std::sync::mpsc::channel::<Feed>();
        let (boundary_tx, boundary_rx) = mpsc::channel::<u64>(8);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let path = model_path.to_path_buf();
        std::thread::spawn(move || {
            let mut session = match ort::session::Session::builder()
                .and_then(|mut b| b.commit_from_file(&path))
            {
                Ok(s) => {
                    let _ = ready_tx.send(Ok(()));
                    s
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(SallyError::Audio(format!(
                        "segmentation model load failed: {e}"
                    ))));
                    return;
                }
            };
            let input_name = session.inputs()[0].name().to_string();
            let mut ring: VecDeque<f32> = VecDeque::with_capacity(WINDOW_SAMPLES);
            let mut since_infer: usize = 0;
            let mut last_boundary_ms: Option<u64> = None;
            while let Ok(feed) = audio_rx.recv() {
                let len = feed.samples.len();
                if feed.suppress {
                    ring.extend(std::iter::repeat(0.0).take(len));
                } else {
                    ring.extend(feed.samples);
                }
                while ring.len() > WINDOW_SAMPLES {
                    ring.pop_front();
                }
                let end_t_ms = feed.t_ms + (len * 1000 / SAMPLE_RATE) as u64;
                since_infer += len;
                if ring.len() < WINDOW_SAMPLES || since_infer < HOP_SAMPLES {
                    continue;
                }
                since_infer = 0;
                let window: Vec<f32> = ring.iter().copied().collect();
                let rms = (window.iter().map(|s| s * s).sum::<f32>()
                    / window.len() as f32)
                    .sqrt();
                if rms < WINDOW_RMS_GATE {
                    continue;
                }
                let frames = match run_model(&mut session, &input_name, window) {
                    Ok(f) => f,
                    Err(e) => {
                        log::warn!("segmentation inference failed: {e}");
                        continue;
                    }
                };
                let states = smooth(&decode(&frames), SMOOTH_FRAMES);
                let step_ms = WINDOW_MS as f64 / states.len().max(1) as f64;
                let window_start = end_t_ms.saturating_sub(WINDOW_MS);
                if let Some(b) =
                    find_boundary(&states, step_ms, window_start, last_boundary_ms)
                {
                    last_boundary_ms = Some(b);
                    // try_send: a full queue only delays a cosmetic split.
                    let _ = boundary_tx.try_send(b);
                }
            }
        });
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(SplitDetector {
                audio_tx,
                boundary_rx,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(SallyError::Audio(
                "segmentation worker died during startup".into(),
            )),
        }
    }
}

fn run_model(
    session: &mut ort::session::Session,
    input_name: &str,
    window: Vec<f32>,
) -> std::result::Result<Vec<[f32; 7]>, ort::Error> {
    let n = window.len();
    let input = ort::value::Tensor::from_array(([1usize, 1, n], window))?;
    let outputs = session.run(ort::inputs![input_name => input])?;
    let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
    let frames = shape[1] as usize;
    let classes = shape[2] as usize;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut row = [0f32; 7];
        for c in 0..classes.min(7) {
            row[c] = data[f * classes + c];
        }
        out.push(row);
    }
    Ok(out)
}

/// Per-frame local speaker state within one window. Powerset class order of
/// pyannote segmentation-3.0: [none, s0, s1, s2, s0+s1, s0+s2, s1+s2].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameState {
    Silence,
    /// One voice active: local index 0–2, consistent within a window only.
    Solo(u8),
    Overlap,
}

pub fn decode(frames: &[[f32; 7]]) -> Vec<FrameState> {
    frames
        .iter()
        .map(|row| {
            let mut best = 0usize;
            for (c, v) in row.iter().enumerate() {
                if *v > row[best] {
                    best = c;
                }
            }
            match best {
                0 => FrameState::Silence,
                1..=3 => FrameState::Solo((best - 1) as u8),
                _ => FrameState::Overlap,
            }
        })
        .collect()
}

/// Majority filter over an odd window: removes single-frame flicker that
/// would otherwise fabricate tiny runs.
pub fn smooth(states: &[FrameState], win: usize) -> Vec<FrameState> {
    if states.is_empty() || win <= 1 {
        return states.to_vec();
    }
    let half = win / 2;
    (0..states.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(states.len());
            // Count occurrences of each state in the window; ties keep the
            // center frame.
            let mut counts: Vec<(FrameState, usize)> = Vec::new();
            for s in &states[lo..hi] {
                match counts.iter_mut().find(|(k, _)| k == s) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((*s, 1)),
                }
            }
            let center = states[i];
            let best = counts
                .iter()
                .max_by_key(|(_, n)| *n)
                .map(|(s, n)| (*s, *n))
                .unwrap_or((center, 0));
            let center_count = counts
                .iter()
                .find(|(s, _)| *s == center)
                .map(|(_, n)| *n)
                .unwrap_or(0);
            if center_count == best.1 {
                center
            } else {
                best.0
            }
        })
        .collect()
}

/// Latest speaker-to-speaker handoff in the window, if any qualifies:
/// a sustained solo voice, at most a short gap (silence or overlap), then a
/// different sustained solo voice. Returns the absolute time of the new
/// voice's first frame.
pub fn find_boundary(
    states: &[FrameState],
    step_ms: f64,
    window_start_ms: u64,
    last_boundary_ms: Option<u64>,
) -> Option<u64> {
    // Collapse into runs of identical state.
    let mut runs: Vec<(FrameState, usize, usize)> = Vec::new(); // (state, start, len)
    for (i, s) in states.iter().enumerate() {
        match runs.last_mut() {
            Some((state, _, len)) if state == s => *len += 1,
            _ => runs.push((*s, i, 1)),
        }
    }
    let dur = |len: usize| len as f64 * step_ms;

    // Newest qualifying handoff wins: scan run pairs from the end.
    for bi in (0..runs.len()).rev() {
        let (b_state, b_start, b_len) = runs[bi];
        let FrameState::Solo(b_spk) = b_state else { continue };
        if dur(b_len) < MIN_RUN_MS {
            continue;
        }
        // Walk backwards over an optional short gap to the previous solo.
        let mut gap_ms = 0.0;
        let mut ai = bi;
        let a = loop {
            if ai == 0 {
                break None;
            }
            ai -= 1;
            let (a_state, _, a_len) = runs[ai];
            match a_state {
                FrameState::Solo(a_spk) => break Some((a_spk, a_len)),
                _ => {
                    gap_ms += dur(a_len);
                    if gap_ms > MAX_GAP_MS {
                        break None;
                    }
                }
            }
        };
        let Some((a_spk, a_len)) = a else { continue };
        if a_spk == b_spk || dur(a_len) < MIN_RUN_MS {
            continue;
        }
        let boundary = window_start_ms + (b_start as f64 * step_ms) as u64;
        if let Some(last) = last_boundary_ms {
            if boundary < last.saturating_add(REFRACTORY_MS) {
                return None; // older handoffs are older still
            }
        }
        return Some(boundary);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use FrameState::{Overlap, Silence, Solo};

    /// ~17 ms per frame like the real model output.
    const STEP: f64 = 17.0;

    fn run(state: FrameState, ms: f64) -> Vec<FrameState> {
        vec![state; (ms / STEP).ceil() as usize]
    }

    #[test]
    fn decode_maps_powerset_classes() {
        let mut silence = [0f32; 7];
        silence[0] = 1.0;
        let mut solo1 = [0f32; 7];
        solo1[2] = 1.0;
        let mut overlap = [0f32; 7];
        overlap[5] = 1.0;
        assert_eq!(
            decode(&[silence, solo1, overlap]),
            vec![Silence, Solo(1), Overlap]
        );
    }

    #[test]
    fn smooth_removes_single_frame_flicker() {
        let mut states = vec![Solo(0); 20];
        states[10] = Solo(1);
        assert_eq!(smooth(&states, 9), vec![Solo(0); 20]);
    }

    #[test]
    fn clean_handoff_is_a_boundary() {
        let mut s = run(Solo(0), 2000.0);
        let b_start_frame = s.len();
        s.extend(run(Solo(1), 2000.0));
        let b = find_boundary(&s, STEP, 10_000, None).expect("boundary");
        assert_eq!(b, 10_000 + (b_start_frame as f64 * STEP) as u64);
    }

    #[test]
    fn handoff_across_short_silence_is_a_boundary() {
        let mut s = run(Solo(0), 2000.0);
        s.extend(run(Silence, 400.0));
        s.extend(run(Solo(2), 1500.0));
        assert!(find_boundary(&s, STEP, 0, None).is_some());
    }

    #[test]
    fn long_silence_is_not_a_boundary() {
        let mut s = run(Solo(0), 2000.0);
        s.extend(run(Silence, 3000.0));
        s.extend(run(Solo(1), 2000.0));
        assert!(find_boundary(&s, STEP, 0, None).is_none());
    }

    #[test]
    fn short_interjection_is_not_a_boundary() {
        let mut s = run(Solo(0), 3000.0);
        s.extend(run(Solo(1), 300.0)); // "mm-hm"
        s.extend(run(Solo(0), 3000.0));
        assert!(find_boundary(&s, STEP, 0, None).is_none());
    }

    #[test]
    fn same_speaker_resuming_is_not_a_boundary() {
        let mut s = run(Solo(1), 2000.0);
        s.extend(run(Silence, 500.0));
        s.extend(run(Solo(1), 2000.0));
        assert!(find_boundary(&s, STEP, 0, None).is_none());
    }

    #[test]
    fn handoff_through_overlap_is_a_boundary() {
        let mut s = run(Solo(0), 2000.0);
        s.extend(run(Overlap, 500.0)); // interruption
        s.extend(run(Solo(1), 1500.0));
        assert!(find_boundary(&s, STEP, 0, None).is_some());
    }

    #[test]
    fn refractory_suppresses_rapid_boundaries() {
        let mut s = run(Solo(0), 2000.0);
        s.extend(run(Solo(1), 2000.0));
        let b = find_boundary(&s, STEP, 0, None).expect("boundary");
        assert!(find_boundary(&s, STEP, 0, Some(b)).is_none(), "same boundary again");
    }

    #[test]
    fn silence_only_window_has_no_boundary() {
        let s = run(Silence, 10_000.0);
        assert!(find_boundary(&s, STEP, 0, None).is_none());
    }
}
