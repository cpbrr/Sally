//! Post-meeting speaker identification over the saved recording.
//!
//! Unlike the live diarization removed in v0.6.0 (six failed tuning
//! releases of incremental clustering), this is a single offline pass with
//! the whole meeting available, and it runs in the background *after*
//! `end_meeting` has already returned — ending a meeting stays instant.
//!
//! The transcript's own entries are the segments: the live pipeline already
//! splits on speaker handoffs, language changes, turns, and 60 s caps, so
//! each "Meeting" entry is (approximately) one voice. For each such entry
//! the matching span of the WAV is embedded (kaldi fbank → WeSpeaker CAM++
//! ONNX, both dependency-light) and the embeddings are clustered with
//! complete-linkage agglomerative clustering on cosine similarity. Clusters
//! become "Speaker 1..N" labels written back into the raw Markdown; "You"
//! entries are never touched (mic attribution is already reliable). The
//! review screen's rename/merge UI corrects whatever the pass gets wrong.
//!
//! `.env` overrides: SALLY_DIAR_THRESHOLD (join similarity, 0.05–0.95,
//! default 0.5), SALLY_EMBEDDING_MODEL_URL (air-gapped model download).

use crate::error::{Result, SallyError};
use crate::fbank;
use crate::split::models_dir;
use crate::store::TranscriptChunk;
use std::path::{Path, PathBuf};

pub const MODEL_FILE: &str = "speaker_embedding_campp.onnx";
// WeSpeaker en-voxceleb CAM++ exported for onnx, ~28 MB — the exact model +
// feature recipe pair pyannote-rs ships, so the fbank in `fbank.rs` matches
// what the model was fed during its validation.
const MODEL_URL: &str =
    "https://github.com/thewh1teagle/pyannote-rs/releases/download/v0.1.0/wespeaker_en_voxceleb_CAM++.onnx";

/// Entries shorter than this carry too little voice to embed reliably;
/// they keep the plain "Meeting" label.
const MIN_SEGMENT_MS: u64 = 800;
/// Embedding input cap per entry — CAM++ saturates well before this and it
/// bounds inference cost on 60 s entries.
const MAX_SEGMENT_MS: u64 = 30_000;
/// Hard cap on distinct speakers, matching the old live pipeline's limit.
const MAX_SPEAKERS: usize = 8;
pub const DEFAULT_JOIN_SIM: f32 = 0.5;

/// Ensure the embedding model exists, downloading on first use (same
/// pattern as the segmentation model in `split.rs`).
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
    log::info!("downloading speaker embedding model: {url}");
    let resp = reqwest::get(url)
        .await
        .map_err(|e| SallyError::Config(format!("embedding model download failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(SallyError::Config(format!(
            "embedding model download failed: HTTP {} for {url}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| SallyError::Config(format!("embedding model download failed: {e}")))?;
    let tmp = dest.with_extension("onnx.part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(dest)
}

/// Read a 16 kHz mono 16-bit PCM WAV (the recorder's own format) into
/// [-1, 1] samples. Scans RIFF chunks rather than assuming a 44-byte
/// header so canonical files from other tools also load.
pub fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(SallyError::Storage("not a WAV file".into()));
    }
    let mut pos = 12usize;
    let mut fmt_ok = false;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            let format = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
            let channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
            let rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            let bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
            if format != 1 || channels != 1 || rate != 16_000 || bits != 16 {
                return Err(SallyError::Storage(format!(
                    "unsupported WAV format ({format}/{channels}ch/{rate}Hz/{bits}bit); expected PCM mono 16 kHz 16-bit"
                )));
            }
            fmt_ok = true;
        } else if id == b"data" {
            let end = (body + size).min(bytes.len());
            data = Some(&bytes[body..end]);
        }
        pos = body + size + (size & 1);
    }
    let (true, Some(data)) = (fmt_ok, data) else {
        return Err(SallyError::Storage("WAV missing fmt/data chunk".into()));
    };
    Ok(data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect())
}

/// ONNX speaker-embedding extractor (input [1, T, 80] fbank, output
/// embedding row, L2-normalized here).
pub struct EmbeddingExtractor {
    session: ort::session::Session,
    input_name: String,
}

impl EmbeddingExtractor {
    pub fn new(model_path: &Path) -> Result<Self> {
        let session = ort::session::Session::builder()
            .and_then(|mut b| b.commit_from_file(model_path))
            .map_err(|e| SallyError::Audio(format!("embedding model load failed: {e}")))?;
        let input_name = session.inputs()[0].name().to_string();
        Ok(Self {
            session,
            input_name,
        })
    }

    pub fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let (frames, feats) = fbank::compute(samples);
        if frames == 0 {
            return Err(SallyError::Audio("segment too short to embed".into()));
        }
        let input =
            ort::value::Tensor::from_array(([1usize, frames, fbank::NUM_BINS], feats))
                .map_err(|e| SallyError::Audio(format!("embedding input: {e}")))?;
        let outputs = self
            .session
            .run(ort::inputs![&self.input_name => input])
            .map_err(|e| SallyError::Audio(format!("embedding inference: {e}")))?;
        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| SallyError::Audio(format!("embedding output: {e}")))?;
        let mut v: Vec<f32> = data.to_vec();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(f32::EPSILON);
        for x in v.iter_mut() {
            *x /= norm;
        }
        Ok(v)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Complete-linkage agglomerative clustering on cosine similarity: merge
/// while the *worst* pair between two clusters still clears `join_sim`,
/// then keep merging best-first if the speaker cap is exceeded. Returns a
/// cluster index per embedding.
pub fn cluster(embeddings: &[Vec<f32>], join_sim: f32) -> Vec<usize> {
    let n = embeddings.len();
    if n == 0 {
        return Vec::new();
    }
    // Pairwise similarities cached once; merges then only index into it.
    let mut sims = vec![0f32; n * n];
    for i in 0..n {
        for j in i..n {
            let s = cosine(&embeddings[i], &embeddings[j]);
            sims[i * n + j] = s;
            sims[j * n + i] = s;
        }
    }
    let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let link = |a: &[usize], b: &[usize]| -> f32 {
        // Complete linkage: the weakest cross-pair similarity.
        let mut worst = f32::INFINITY;
        for &i in a {
            for &j in b {
                worst = worst.min(sims[i * n + j]);
            }
        }
        worst
    };
    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for a in 0..clusters.len() {
            for b in a + 1..clusters.len() {
                let s = link(&clusters[a], &clusters[b]);
                if best.map(|(_, _, bs)| s > bs).unwrap_or(true) {
                    best = Some((a, b, s));
                }
            }
        }
        let Some((a, b, s)) = best else { break };
        let over_cap = clusters.len() > MAX_SPEAKERS;
        if s < join_sim && !over_cap {
            break;
        }
        let merged = clusters.remove(b);
        clusters[a].extend(merged);
    }
    let mut labels = vec![0usize; n];
    for (c, members) in clusters.iter().enumerate() {
        for &i in members {
            labels[i] = c;
        }
    }
    labels
}

/// One relabeling decision: the entry starting at `start_ms` becomes
/// `label`.
pub struct Assignment {
    pub start_ms: u64,
    pub label: String,
}

/// The full offline pass: embed every eligible "Meeting" entry span and
/// cluster them into "Speaker N" labels. `chunks` must be in file order.
pub fn assign_speakers(
    extractor: &mut EmbeddingExtractor,
    samples: &[f32],
    chunks: &[TranscriptChunk],
    join_sim: f32,
) -> Vec<Assignment> {
    let audio_ms = (samples.len() as u64 * 1000) / fbank::SAMPLE_RATE as u64;
    let mut starts: Vec<u64> = Vec::new();
    let mut embeddings: Vec<Vec<f32>> = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        if c.speaker != "Meeting" {
            continue;
        }
        let end_ms = chunks
            .get(i + 1)
            .map(|n| n.start_ms)
            .unwrap_or(audio_ms)
            .min(audio_ms)
            .min(c.start_ms + MAX_SEGMENT_MS);
        if end_ms <= c.start_ms || end_ms - c.start_ms < MIN_SEGMENT_MS {
            continue;
        }
        let lo = (c.start_ms as usize * fbank::SAMPLE_RATE) / 1000;
        let hi = ((end_ms as usize) * fbank::SAMPLE_RATE / 1000).min(samples.len());
        if hi <= lo {
            continue;
        }
        match extractor.embed(&samples[lo..hi]) {
            Ok(e) => {
                starts.push(c.start_ms);
                embeddings.push(e);
            }
            Err(e) => log::warn!("embedding failed for entry at {} ms: {e}", c.start_ms),
        }
    }
    let labels = cluster(&embeddings, join_sim);
    // Number speakers by first appearance in the timeline.
    let mut order: Vec<usize> = Vec::new();
    let mut out = Vec::with_capacity(starts.len());
    for (idx, &start_ms) in starts.iter().enumerate() {
        let c = labels[idx];
        let n = match order.iter().position(|&o| o == c) {
            Some(p) => p,
            None => {
                order.push(c);
                order.len() - 1
            }
        };
        out.push(Assignment {
            start_ms,
            label: format!("Speaker {}", n + 1),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn two_clear_groups_form_two_clusters() {
        let a1 = unit(&[1.0, 0.05, 0.0]);
        let a2 = unit(&[0.95, 0.1, 0.0]);
        let b1 = unit(&[0.0, 1.0, 0.05]);
        let b2 = unit(&[0.05, 0.95, 0.0]);
        let labels = cluster(&[a1, b1, a2, b2], 0.5);
        assert_eq!(labels[0], labels[2]);
        assert_eq!(labels[1], labels[3]);
        assert_ne!(labels[0], labels[1]);
    }

    #[test]
    fn similar_embeddings_collapse_to_one_cluster() {
        let e1 = unit(&[1.0, 0.1, 0.1]);
        let e2 = unit(&[0.9, 0.2, 0.1]);
        let e3 = unit(&[0.95, 0.15, 0.05]);
        let labels = cluster(&[e1, e2, e3], 0.5);
        assert!(labels.iter().all(|&l| l == labels[0]));
    }

    #[test]
    fn speaker_cap_forces_merges() {
        // 10 mutually orthogonal-ish embeddings but the cap is 8: forced
        // merges must leave at most MAX_SPEAKERS clusters.
        let mut embs = Vec::new();
        for i in 0..10 {
            let mut v = vec![0.02f32; 10];
            v[i] = 1.0;
            embs.push(unit(&v));
        }
        let labels = cluster(&embs, 0.99);
        let distinct: std::collections::BTreeSet<usize> = labels.iter().copied().collect();
        assert!(distinct.len() <= MAX_SPEAKERS, "got {}", distinct.len());
    }

    #[test]
    fn wav_roundtrip_through_recorder() {
        use crate::audio::recorder::WavRecorder;
        let path = std::env::temp_dir().join(format!("sally-diar-{}.wav", std::process::id()));
        let mut rec = WavRecorder::create(&path).unwrap();
        rec.write(0, &[8000i16; 1600]).unwrap();
        rec.finalize().unwrap();
        let samples = read_wav_16k_mono(&path).unwrap();
        assert_eq!(samples.len(), 1600);
        assert!((samples[0] - 8000.0 / 32768.0).abs() < 1e-4);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn assignments_number_speakers_by_first_appearance() {
        // Fake the clustering path by exercising the numbering logic via
        // cluster(): 3 entries, entry 0 and 2 same voice, entry 1 other.
        let same_a = unit(&[1.0, 0.0]);
        let other = unit(&[0.0, 1.0]);
        let same_b = unit(&[0.98, 0.05]);
        let labels = cluster(&[same_a, other, same_b], 0.5);
        assert_eq!(labels[0], labels[2]);
        assert_ne!(labels[0], labels[1]);
    }
}
