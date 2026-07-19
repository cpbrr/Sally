//! Diarization model manager. Sally uses the sherpa-onnx offline speaker
//! diarization pipeline: a pyannote segmentation model finds who-spoke-when
//! regions and a speaker-embedding model feeds agglomerative clustering.
//! Model files are downloaded once into `<data dir>/models/`; URLs are
//! overridable through `.env` for air-gapped setups (drop the files in
//! manually and Sally skips the download).

use crate::error::{Result, SallyError};
use std::path::{Path, PathBuf};

// Versioned filenames: changing a default model changes the name so
// existing installs re-download instead of loading the old weights.
pub const SEGMENTATION_FILE: &str = "segmentation_pyannote3.onnx";
pub const SPEAKER_FILE: &str = "speaker_titanet.onnx";

// pyannote segmentation-3.0 converted for sherpa-onnx, ~6 MB. The GitHub
// release asset is a tar.bz2; this mirror serves the bare .onnx so no
// archive extraction is needed.
const SEGMENTATION_URL: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx";
// NeMo TitaNet-small, ~38 MB. Measured on known-speaker audio
// (sr-data): same-speaker cosine 0.73, different-speaker 0.15–0.19 —
// a wide margin for the clustering threshold. CAM++ measured
// 0.61 vs 0.36–0.47 (different speakers crossed the threshold, which
// merged everyone into one cluster). (The upstream release tag really
// is spelled "recongition".)
const SPEAKER_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx";

#[derive(Debug, Clone)]
pub struct ModelPaths {
    pub segmentation: PathBuf,
    pub speaker: PathBuf,
}

pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Paths if both model files are already present.
pub fn existing_models(data_dir: &Path) -> Option<ModelPaths> {
    let dir = models_dir(data_dir);
    let paths = ModelPaths {
        segmentation: dir.join(SEGMENTATION_FILE),
        speaker: dir.join(SPEAKER_FILE),
    };
    if paths.segmentation.exists() && paths.speaker.exists() {
        Some(paths)
    } else {
        None
    }
}

/// Ensure both models exist, downloading whatever is missing. Slow on first
/// run (~44 MB total); callers should surface progress to the UI.
pub async fn ensure_models(
    data_dir: &Path,
    segmentation_url_override: &str,
    speaker_url_override: &str,
) -> Result<ModelPaths> {
    if let Some(paths) = existing_models(data_dir) {
        return Ok(paths);
    }
    let dir = models_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let segmentation_url = if segmentation_url_override.is_empty() {
        SEGMENTATION_URL
    } else {
        segmentation_url_override
    };
    let speaker_url = if speaker_url_override.is_empty() {
        SPEAKER_URL
    } else {
        speaker_url_override
    };
    let segmentation = dir.join(SEGMENTATION_FILE);
    let speaker = dir.join(SPEAKER_FILE);
    if !segmentation.exists() {
        download(segmentation_url, &segmentation).await?;
    }
    if !speaker.exists() {
        download(speaker_url, &speaker).await?;
    }
    Ok(ModelPaths {
        segmentation,
        speaker,
    })
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
