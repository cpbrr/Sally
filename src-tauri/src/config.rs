//! Application configuration backed by a plain-text `.env` file inside the
//! user-selected Sally data folder (design §8). The `.env` location is found
//! through a small pointer file in the OS app-config directory, because the
//! data folder itself is chosen by the user during setup.

use crate::error::{Result, SallyError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_LIVE_MODEL: &str = "gemini-3.5-live-translate-preview";
pub const DEFAULT_CLEANUP_MODEL: &str = "gemini-3.1-flash-lite";

const KEY_API: &str = "GEMINI_API_KEY";
const KEY_LIVE_MODEL: &str = "SALLY_LIVE_MODEL";
const KEY_CLEANUP_MODEL: &str = "SALLY_CLEANUP_MODEL";
const KEY_TARGET_LANG: &str = "SALLY_TARGET_LANGUAGE";
const KEY_UI_LANG: &str = "SALLY_UI_LANGUAGE";
const KEY_CAPTURE_APP: &str = "SALLY_CAPTURE_APP";
const KEY_ALWAYS_ON_TOP: &str = "SALLY_ALWAYS_ON_TOP";
const KEY_MIC_DEVICE: &str = "SALLY_MIC_DEVICE";
const KEY_SYSTEM_DEVICE: &str = "SALLY_SYSTEM_DEVICE";
const KEY_READOUT: &str = "SALLY_READOUT";
const KEY_LIVE_API_VERSION: &str = "SALLY_LIVE_API_VERSION";

// The documented WebSocket endpoint for live translation is v1beta; the
// session still auto-flips to v1alpha if setup keeps getting rejected.
pub const DEFAULT_LIVE_API_VERSION: &str = "v1beta";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub api_key: String,
    pub live_model: String,
    pub cleanup_model: String,
    pub target_language: String,
    pub ui_language: String,
    pub always_on_top: bool,
    pub mic_device: String,
    pub system_device: String,
    /// Capture system audio from a single application (executable name)
    /// via process loopback instead of the whole device. Windows only;
    /// empty = entire system.
    pub capture_app: String,
    /// Read translated audio aloud for passages not already in the target
    /// language. Off by default.
    pub readout_enabled: bool,
    /// Live API version (`v1alpha` or `v1beta`). Preview models usually live
    /// on v1alpha; the session flips automatically if setup is rejected.
    pub live_api_version: String,
}

impl AppConfig {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            api_key: String::new(),
            live_model: DEFAULT_LIVE_MODEL.into(),
            cleanup_model: DEFAULT_CLEANUP_MODEL.into(),
            target_language: "Vietnamese".into(),
            ui_language: "en".into(),
            // Off by default: staying pinned above everything during setup
            // proved annoying. The title-bar pin turns it on per window.
            always_on_top: false,
            mic_device: String::new(),
            system_device: String::new(),
            capture_app: String::new(),
            readout_enabled: false,
            live_api_version: DEFAULT_LIVE_API_VERSION.into(),
        }
    }

    pub fn env_path(&self) -> PathBuf {
        self.data_dir.join(".env")
    }

    pub fn meetings_dir(&self) -> PathBuf {
        self.data_dir.join("meetings")
    }

    pub fn recovery_dir(&self) -> PathBuf {
        self.data_dir.join(".recovery")
    }

    /// Load from the `.env` inside `data_dir`, falling back to defaults for
    /// missing keys. Unknown keys are preserved by `save`.
    pub fn load(data_dir: PathBuf) -> Result<Self> {
        let mut cfg = Self::new(data_dir);
        let path = cfg.env_path();
        if !path.exists() {
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&path)?;
        let map = parse_env(&text);
        let get = |k: &str| map.get(k).cloned().unwrap_or_default();
        if let Some(v) = map.get(KEY_API) {
            cfg.api_key = v.clone();
        }
        if let Some(v) = map.get(KEY_LIVE_MODEL) {
            if !v.is_empty() {
                cfg.live_model = v.clone();
            }
        }
        if let Some(v) = map.get(KEY_CLEANUP_MODEL) {
            if !v.is_empty() {
                cfg.cleanup_model = v.clone();
            }
        }
        if let Some(v) = map.get(KEY_TARGET_LANG) {
            if !v.is_empty() {
                cfg.target_language = v.clone();
            }
        }
        if let Some(v) = map.get(KEY_UI_LANG) {
            if !v.is_empty() {
                cfg.ui_language = v.clone();
            }
        }
        cfg.always_on_top = get(KEY_ALWAYS_ON_TOP) != "off";
        cfg.mic_device = get(KEY_MIC_DEVICE);
        cfg.system_device = get(KEY_SYSTEM_DEVICE);
        cfg.capture_app = get(KEY_CAPTURE_APP);
        cfg.readout_enabled = get(KEY_READOUT) == "on";
        let ver = get(KEY_LIVE_API_VERSION);
        if !ver.is_empty() {
            cfg.live_api_version = ver;
        }
        Ok(cfg)
    }

    /// Write `.env`, preserving unknown keys already present in the file.
    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.meetings_dir())?;
        std::fs::create_dir_all(self.recovery_dir())?;
        let path = self.env_path();
        let mut map = if path.exists() {
            parse_env(&std::fs::read_to_string(&path)?)
        } else {
            BTreeMap::new()
        };
        map.insert(KEY_API.into(), self.api_key.clone());
        map.insert(KEY_LIVE_MODEL.into(), self.live_model.clone());
        map.insert(KEY_CLEANUP_MODEL.into(), self.cleanup_model.clone());
        map.insert(KEY_TARGET_LANG.into(), self.target_language.clone());
        map.insert(KEY_UI_LANG.into(), self.ui_language.clone());
        map.insert(
            KEY_ALWAYS_ON_TOP.into(),
            if self.always_on_top { "on" } else { "off" }.into(),
        );
        map.insert(KEY_MIC_DEVICE.into(), self.mic_device.clone());
        map.insert(KEY_SYSTEM_DEVICE.into(), self.system_device.clone());
        map.insert(KEY_CAPTURE_APP.into(), self.capture_app.clone());
        map.insert(
            KEY_READOUT.into(),
            if self.readout_enabled { "on" } else { "off" }.into(),
        );
        map.insert(KEY_LIVE_API_VERSION.into(), self.live_api_version.clone());
        let mut out = String::from(
            "# Sally configuration. The API key is stored in plain text by design;\n\
             # anyone who can read this folder can obtain it.\n",
        );
        for (k, v) in &map {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
        std::fs::write(&path, out)?;
        Ok(())
    }

    /// Copy safe to send to the UI: the key itself never leaves the core.
    pub fn redacted(&self) -> RedactedConfig {
        RedactedConfig {
            data_dir: self.data_dir.clone(),
            has_api_key: !self.api_key.trim().is_empty(),
            live_model: self.live_model.clone(),
            cleanup_model: self.cleanup_model.clone(),
            target_language: self.target_language.clone(),
            ui_language: self.ui_language.clone(),
            always_on_top: self.always_on_top,
            mic_device: self.mic_device.clone(),
            system_device: self.system_device.clone(),
            capture_app: self.capture_app.clone(),
            readout_enabled: self.readout_enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedConfig {
    pub data_dir: PathBuf,
    pub has_api_key: bool,
    pub live_model: String,
    pub cleanup_model: String,
    pub target_language: String,
    pub ui_language: String,
    pub always_on_top: bool,
    pub mic_device: String,
    pub system_device: String,
    pub capture_app: String,
    pub readout_enabled: bool,
}

/// Remove every occurrence of the API key from a message before it can reach
/// logs or the UI (design §8, §10).
pub fn redact_key(message: &str, api_key: &str) -> String {
    let key = api_key.trim();
    if key.is_empty() {
        return message.to_string();
    }
    message.replace(key, "[REDACTED]")
}

fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// Pointer file in the OS config dir that records where the user's Sally
/// data folder lives.
pub fn data_dir_pointer_path(app_config_dir: &Path) -> PathBuf {
    app_config_dir.join("sally-data-dir.txt")
}

pub fn read_data_dir_pointer(app_config_dir: &Path) -> Option<PathBuf> {
    let p = data_dir_pointer_path(app_config_dir);
    let text = std::fs::read_to_string(p).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

pub fn write_data_dir_pointer(app_config_dir: &Path, data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(app_config_dir)?;
    std::fs::write(
        data_dir_pointer_path(app_config_dir),
        data_dir.to_string_lossy().as_bytes(),
    )
    .map_err(|e| SallyError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_roundtrip_preserves_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("sally-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env"), "CUSTOM_FLAG=1\nGEMINI_API_KEY=abc\n").unwrap();
        let mut cfg = AppConfig::load(dir.clone()).unwrap();
        assert_eq!(cfg.api_key, "abc");
        cfg.api_key = "xyz".into();
        cfg.save().unwrap();
        let text = std::fs::read_to_string(dir.join(".env")).unwrap();
        assert!(text.contains("CUSTOM_FLAG=1"));
        assert!(text.contains("GEMINI_API_KEY=xyz"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn redaction_removes_key() {
        let msg = "request to https://x?key=SECRET123 failed";
        assert!(!redact_key(msg, "SECRET123").contains("SECRET123"));
        assert_eq!(redact_key(msg, ""), msg);
    }

    #[test]
    fn defaults_applied_when_env_missing() {
        let cfg = AppConfig::load(std::env::temp_dir().join("sally-none")).unwrap();
        assert_eq!(cfg.live_model, DEFAULT_LIVE_MODEL);
        assert_eq!(cfg.cleanup_model, DEFAULT_CLEANUP_MODEL);
        assert!(!cfg.always_on_top, "always-on-top must default off");
        assert!(!cfg.readout_enabled);
        assert_eq!(cfg.live_api_version, DEFAULT_LIVE_API_VERSION);
    }
}
