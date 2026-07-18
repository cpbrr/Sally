//! Diarization model manager. Sally uses sherpa-onnx (silero VAD + a
//! speaker-embedding model) for speaker diarization. Model files are
//! downloaded once into `<data dir>/models/` from the official sherpa-onnx
//! release assets; URLs are overridable through `.env` for air-gapped
//! setups (drop the files in manually and Sally skips the download).

use crate::error::{Result, SallyError};
use std::path::{Path, PathBuf};

pub const VAD_FILE: &str = "silero_vad.onnx";
// Versioned filename: changing the default model changes the name so
// existing installs re-download instead of loading the old weights.
pub const SPEAKER_FILE: &str = "speaker_camplus.onnx";

const VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";
// WeSpeaker CAM++ (VoxCeleb): stronger speaker separation than the previous
// ERes2Net zh-cn model, ~28 MB. (The upstream release tag really is spelled
// "recongition".)
const SPEAKER_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_CAM%2B%2B.onnx";

#[derive(Debug, Clone)]
pub struct ModelPaths {
    pub vad: PathBuf,
    pub speaker: PathBuf,
}

pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Paths if both model files are already present.
pub fn existing_models(data_dir: &Path) -> Option<ModelPaths> {
    let dir = models_dir(data_dir);
    let paths = ModelPaths {
        vad: dir.join(VAD_FILE),
        speaker: dir.join(SPEAKER_FILE),
    };
    if paths.vad.exists() && paths.speaker.exists() {
        Some(paths)
    } else {
        None
    }
}

/// Ensure both models exist, downloading whatever is missing. Slow on first
/// run (~28 MB total); callers should surface progress to the UI.
pub async fn ensure_models(
    data_dir: &Path,
    vad_url_override: &str,
    speaker_url_override: &str,
) -> Result<ModelPaths> {
    if let Some(paths) = existing_models(data_dir) {
        return Ok(paths);
    }
    let dir = models_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let vad_url = if vad_url_override.is_empty() { VAD_URL } else { vad_url_override };
    let speaker_url = if speaker_url_override.is_empty() {
        SPEAKER_URL
    } else {
        speaker_url_override
    };
    let vad = dir.join(VAD_FILE);
    let speaker = dir.join(SPEAKER_FILE);
    if !vad.exists() {
        download(vad_url, &vad).await?;
    }
    if !speaker.exists() {
        download(speaker_url, &speaker).await?;
    }
    Ok(ModelPaths { vad, speaker })
}

async fn download(url: &str, dest: &Path) -> Result<()> {
    log::info!("downloading diarization model: {url}");
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
    // Write via a temp file so a cut connection never leaves a truncated
    // model that would fail to load forever after.
    let tmp = dest.with_extension("onnx.part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}
