//! Tauri command layer: the only boundary the UI talks to (design §4.2
//! item 8 — the UI never captures audio, calls Gemini, or writes files).

use crate::config::{write_data_dir_pointer, AppConfig, RedactedConfig};
use crate::error::{Result, SallyError};
use crate::gemini::cleanup::{render_polished, split_sections, CleanupClient, SECTION_BUDGET};
use crate::session::{Control, ReviewData, SessionHandle};
use crate::store::MeetingStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct AppState {
    pub config: Mutex<Option<AppConfig>>,
    pub session: Mutex<Option<SessionHandle>>,
    pub last_meeting: Mutex<Option<ReviewData>>,
}

fn app_config_dir(app: &AppHandle) -> Result<PathBuf> {
    app.path()
        .app_config_dir()
        .map_err(|e| SallyError::Config(e.to_string()))
}

async fn require_config(state: &State<'_, AppState>) -> Result<AppConfig> {
    state
        .config
        .lock()
        .await
        .clone()
        .ok_or_else(|| SallyError::Config("setup not completed".into()))
}

/// Lock the config, apply `mutate` to it, optionally persist, and return the
/// redacted copy. Shared by the single-field mid-meeting setters
/// (`set_readout`, `set_readout_volume`); `save_settings` has a genuinely
/// different shape (creates a new config when none exists) and is not
/// folded in here.
async fn mutate_config(
    state: &State<'_, AppState>,
    persist: bool,
    mutate: impl FnOnce(&mut AppConfig),
) -> Result<RedactedConfig> {
    let mut guard = state.config.lock().await;
    let cfg = guard
        .as_mut()
        .ok_or_else(|| SallyError::Config("setup not completed".into()))?;
    mutate(cfg);
    if persist {
        cfg.save()?;
    }
    Ok(cfg.redacted())
}

#[derive(Serialize)]
pub struct BootInfo {
    pub config: Option<RedactedConfig>,
    pub needs_setup: bool,
    pub pending_recoveries: usize,
}

#[tauri::command]
pub async fn get_boot_info(app: AppHandle, state: State<'_, AppState>) -> Result<BootInfo> {
    let cfg = state.config.lock().await.clone();
    let pending = cfg
        .as_ref()
        .map(|c| MeetingStore::pending_recoveries(&c.recovery_dir()).len())
        .unwrap_or(0);
    let needs_setup = cfg
        .as_ref()
        .map(|c| c.api_key.trim().is_empty())
        .unwrap_or(true);
    let _ = app;
    Ok(BootInfo {
        config: cfg.map(|c| c.redacted()),
        needs_setup,
        pending_recoveries: pending,
    })
}

#[derive(Deserialize)]
pub struct SettingsPayload {
    pub data_dir: Option<String>,
    pub api_key: Option<String>,
    pub live_model: Option<String>,
    pub cleanup_model: Option<String>,
    pub target_language: Option<String>,
    pub ui_language: Option<String>,
    pub always_on_top: Option<bool>,
    pub mic_device: Option<String>,
    pub system_device: Option<String>,
    pub capture_app: Option<String>,
    pub mac_capture_method: Option<String>,
    pub readout_enabled: Option<bool>,
    pub split_line_count: Option<u32>,
}

/// Create or update configuration. Used by both first-run setup and the
/// settings screen. Persists to `.env` in the data folder (design §8).
#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    payload: SettingsPayload,
) -> Result<RedactedConfig> {
    let mut guard = state.config.lock().await;
    let mut cfg = match (guard.clone(), &payload.data_dir) {
        (Some(existing), None) => existing,
        (Some(mut existing), Some(dir)) => {
            existing.data_dir = PathBuf::from(dir);
            existing
        }
        (None, Some(dir)) => AppConfig::new(PathBuf::from(dir)),
        (None, None) => {
            return Err(SallyError::Config(
                "select a Sally data folder first".into(),
            ))
        }
    };
    if let Some(v) = payload.api_key {
        cfg.api_key = v;
    }
    if let Some(v) = payload.live_model {
        if !v.trim().is_empty() {
            cfg.live_model = v;
        }
    }
    if let Some(v) = payload.cleanup_model {
        if !v.trim().is_empty() {
            cfg.cleanup_model = v;
        }
    }
    if let Some(v) = payload.target_language {
        cfg.target_language = v;
    }
    if let Some(v) = payload.ui_language {
        cfg.ui_language = v;
    }
    if let Some(v) = payload.always_on_top {
        cfg.always_on_top = v;
    }
    if let Some(v) = payload.mic_device {
        cfg.mic_device = v;
    }
    if let Some(v) = payload.system_device {
        cfg.system_device = v;
    }
    if let Some(v) = payload.capture_app {
        cfg.capture_app = v;
    }
    if let Some(v) = payload.mac_capture_method {
        cfg.mac_capture_method = v;
    }
    if let Some(v) = payload.readout_enabled {
        cfg.readout_enabled = v;
    }
    if let Some(v) = payload.split_line_count {
        cfg.split_line_count = v;
    }
    cfg.save()?;
    write_data_dir_pointer(&app_config_dir(&app)?, &cfg.data_dir)?;
    let redacted = cfg.redacted();
    *guard = Some(cfg);
    Ok(redacted)
}

/// Return the stored API key for display in Settings. The key stays
/// redacted in logs and error messages; showing the user their own key in
/// the settings screen is intentional (plain-text `.env` design, §8) and
/// avoids the empty-box confusion after saving.
#[tauri::command]
pub async fn get_api_key(state: State<'_, AppState>) -> Result<String> {
    Ok(state
        .config
        .lock()
        .await
        .as_ref()
        .map(|c| c.api_key.clone())
        .unwrap_or_default())
}

#[derive(Serialize)]
pub struct AudioDevices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// Applications currently playing audio, for the per-app capture picker.
/// Windows: any app with an active audio session. macOS: prefers the Core
/// Audio tap's process list (kAudioHardwarePropertyProcessObjectList) —
/// permission-free, unlike ScreenCaptureKit — falling back to
/// ScreenCaptureKit's enumeration (needs Screen Recording permission) only
/// when the tap path finds nothing, e.g. macOS before 14.2 where that HAL
/// surface doesn't exist. Empty on other platforms.
#[tauri::command]
pub async fn list_audio_apps() -> Result<Vec<String>> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(|| {
            crate::audio::app_capture::list_audio_apps()
                .into_iter()
                .map(|a| a.name)
                .collect()
        })
        .await
        .map_err(|e| SallyError::Audio(e.to_string()))
    }
    #[cfg(target_os = "macos")]
    {
        tokio::task::spawn_blocking(|| {
            // Dedup: an app with several audio-capable helper processes
            // (e.g. Chrome, one Renderer per tab) resolves each of them to
            // the same display name (see resolve_app_display_name) — only
            // show it once.
            let tap_apps: std::collections::BTreeSet<String> =
                crate::audio::coreaudio_tap::list_audio_processes()
                    .into_iter()
                    .map(|p| p.name)
                    .collect();
            if !tap_apps.is_empty() {
                tap_apps.into_iter().collect()
            } else {
                crate::audio::sck_capture::list_audio_apps()
            }
        })
        .await
        .map_err(|e| SallyError::Audio(e.to_string()))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(Vec::new())
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<AudioDevices> {
    // Device enumeration can block; run it off the async runtime threads.
    tokio::task::spawn_blocking(|| AudioDevices {
        inputs: crate::audio::capture::list_input_devices(),
        outputs: crate::audio::capture::list_output_devices(),
    })
    .await
    .map_err(|e| SallyError::Audio(e.to_string()))
}

/// Setup wizard "permissions" step: trigger the OS mic-permission prompt
/// deliberately, ahead of the first meeting, so it doesn't collide with
/// the separate Screen Recording prompt (macOS) that fires later when the
/// user first reaches the main screen. Fire-and-forget from the UI's
/// perspective — errors are swallowed inside `warm_up_mic_permission`
/// itself, so this always returns Ok.
#[tauri::command]
pub async fn request_mic_permission() -> Result<()> {
    tokio::task::spawn_blocking(crate::audio::capture::warm_up_mic_permission)
        .await
        .map_err(|e| SallyError::Audio(e.to_string()))
}

/// Setup step 6: verify the API key and network path without starting a
/// live session (design §6.3).
#[tauri::command]
pub async fn test_connectivity(state: State<'_, AppState>) -> Result<bool> {
    let cfg = require_config(&state).await?;
    if cfg.api_key.trim().is_empty() {
        return Err(SallyError::Config("no API key configured".into()));
    }
    let url = format!(
        "{}/models?key={}&pageSize=1",
        crate::gemini::REST_BASE,
        cfg.api_key
    );
    let resp = reqwest::get(&url).await.map_err(|e| {
        SallyError::Gemini(crate::config::redact_key(
            &format!("connectivity test failed: {e}"),
            &cfg.api_key,
        ))
    })?;
    if resp.status().is_success() {
        Ok(true)
    } else {
        Err(SallyError::Gemini(format!(
            "API key rejected (HTTP {})",
            resp.status()
        )))
    }
}

#[tauri::command]
pub async fn start_meeting(
    app: AppHandle,
    state: State<'_, AppState>,
    target_language: Option<String>,
) -> Result<()> {
    let mut session_guard = state.session.lock().await;
    if session_guard.is_some() {
        return Err(SallyError::Session("a meeting is already running".into()));
    }
    let mut cfg = require_config(&state).await?;
    if let Some(lang) = target_language {
        if !lang.trim().is_empty() && lang != cfg.target_language {
            cfg.target_language = lang;
            cfg.save()?;
            *state.config.lock().await = Some(cfg.clone());
        }
    }
    let handle = crate::session::start(app, cfg)?;
    *session_guard = Some(handle);
    Ok(())
}

async fn send_control(state: &State<'_, AppState>, ctrl: Control) -> Result<()> {
    let guard = state.session.lock().await;
    let session = guard
        .as_ref()
        .ok_or_else(|| SallyError::Session("no active meeting".into()))?;
    session
        .control_tx
        .send(ctrl)
        .await
        .map_err(|_| SallyError::Session("meeting already ended".into()))
}

#[tauri::command]
pub async fn pause_meeting(state: State<'_, AppState>) -> Result<()> {
    send_control(&state, Control::Pause).await
}

/// Toggle translated-voice readout. Persists to `.env` and applies live to a
/// running meeting. Playback stays gated per passage: only source languages
/// that differ from the target are read aloud.
#[tauri::command]
pub async fn set_readout(state: State<'_, AppState>, enabled: bool) -> Result<RedactedConfig> {
    let redacted = mutate_config(&state, true, |cfg| cfg.readout_enabled = enabled).await?;
    // Forward to the running session, if any; no meeting running is fine.
    let guard = state.session.lock().await;
    if let Some(session) = guard.as_ref() {
        let _ = session.control_tx.send(Control::SetReadout(enabled)).await;
    }
    Ok(redacted)
}

#[tauri::command]
pub async fn resume_meeting(state: State<'_, AppState>) -> Result<()> {
    send_control(&state, Control::Resume).await
}

/// Restart microphone capture on a different device — the prompt shown
/// when `sally://mic-lost` fires (e.g. the previous device was unplugged
/// mid-meeting) calls this once the user picks a replacement. Persists the
/// choice like `save_settings` does, and forwards it live to the running
/// session; no meeting running is fine (still saved for the next one).
#[tauri::command]
pub async fn switch_mic(state: State<'_, AppState>, device: String) -> Result<RedactedConfig> {
    let redacted = mutate_config(&state, true, |cfg| cfg.mic_device = device.clone()).await?;
    let guard = state.session.lock().await;
    if let Some(session) = guard.as_ref() {
        let _ = session.control_tx.send(Control::SwitchMic(device)).await;
    }
    Ok(redacted)
}

/// Change the captured app (empty = whole system) live, without ending the
/// meeting — mirrors `switch_mic`.
#[tauri::command]
pub async fn switch_capture_app(
    state: State<'_, AppState>,
    capture_app: String,
) -> Result<RedactedConfig> {
    let redacted =
        mutate_config(&state, true, |cfg| cfg.capture_app = capture_app.clone()).await?;
    let guard = state.session.lock().await;
    if let Some(session) = guard.as_ref() {
        let _ = session
            .control_tx
            .send(Control::SwitchCaptureApp(capture_app))
            .await;
    }
    Ok(redacted)
}

/// Set translated-voice readout volume (0.0–1.0), applied live to a
/// running meeting. `persist=false` for drag ticks (in-memory + live
/// only); `persist=true` on release writes `.env`.
#[tauri::command]
pub async fn set_readout_volume(
    state: State<'_, AppState>,
    volume: f32,
    persist: bool,
) -> Result<RedactedConfig> {
    let clamped = volume.clamp(0.0, 1.0);
    let redacted = mutate_config(&state, persist, |cfg| cfg.readout_volume = clamped).await?;
    let guard = state.session.lock().await;
    if let Some(session) = guard.as_ref() {
        let _ = session
            .control_tx
            .send(Control::SetReadoutVolume(clamped))
            .await;
    }
    Ok(redacted)
}

#[derive(Serialize)]
pub struct ReviewInfo {
    pub raw_path: String,
    pub raw_dir: String,
    pub polished_dir: String,
    /// Local WAV recording for this meeting, when one exists on disk.
    pub audio_path: Option<String>,
}

fn review_info(review: &ReviewData) -> ReviewInfo {
    let audio = review.store.audio_path();
    ReviewInfo {
        raw_path: review.store.raw_path().to_string_lossy().into_owned(),
        raw_dir: review.store.raw_dir().to_string_lossy().into_owned(),
        polished_dir: review.store.polished_dir().to_string_lossy().into_owned(),
        audio_path: audio
            .exists()
            .then(|| audio.to_string_lossy().into_owned()),
    }
}

/// Clickable transcript blocks (timestamp, speaker, original text) for the
/// review audio player, parsed from the last opened meeting's raw file.
#[tauri::command]
pub async fn meeting_chunks(state: State<'_, AppState>) -> Result<Vec<crate::store::TranscriptChunk>> {
    let raw_path = {
        let guard = state.last_meeting.lock().await;
        let review = guard
            .as_ref()
            .ok_or_else(|| SallyError::Session("no meeting open for review".into()))?;
        review.store.raw_path().to_path_buf()
    };
    let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(raw_path))
        .await
        .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))??;
    Ok(crate::store::parse_transcript_chunks(&text))
}

/// End the meeting and enter review (design §6.4). The raw transcript is
/// already preserved before this returns.
#[tauri::command]
pub async fn end_meeting(state: State<'_, AppState>) -> Result<ReviewInfo> {
    let mut handle = {
        let mut guard = state.session.lock().await;
        guard
            .take()
            .ok_or_else(|| SallyError::Session("no active meeting".into()))?
    };
    let _ = handle.control_tx.send(Control::Stop).await;
    let done_rx = handle
        .done_rx
        .take()
        .ok_or_else(|| SallyError::Session("meeting already collected".into()))?;
    let review = done_rx
        .await
        .map_err(|_| SallyError::Session("session task dropped".into()))??;
    let info = review_info(&review);
    *state.last_meeting.lock().await = Some(review);
    Ok(info)
}

/// Re-open the last finished meeting in the processing screen.
#[tauri::command]
pub async fn get_last_meeting(state: State<'_, AppState>) -> Result<Option<ReviewInfo>> {
    Ok(state.last_meeting.lock().await.as_ref().map(review_info))
}

#[derive(Serialize)]
pub struct MeetingFile {
    pub name: String,
    pub path: String,
}

/// Raw meeting transcripts available for processing, newest first.
#[tauri::command]
pub async fn list_meetings(state: State<'_, AppState>) -> Result<Vec<MeetingFile>> {
    let cfg = require_config(&state).await?;
    let raw_dir = cfg.meetings_dir().join("raw");
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&raw_dir) else {
            return Vec::new();
        };
        let mut files: Vec<(std::time::SystemTime, MeetingFile)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.ends_with(".md") && !name.ends_with("-no-timestamps.md")
            })
            .map(|e| {
                let modified = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let path = e.path();
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    modified,
                    MeetingFile {
                        name,
                        path: path.to_string_lossy().into_owned(),
                    },
                )
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0));
        files.into_iter().map(|(_, f)| f).collect()
    })
    .await
    .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))
}

/// Open a past meeting's raw transcript for processing.
#[tauri::command]
pub async fn open_meeting(state: State<'_, AppState>, raw_path: String) -> Result<ReviewInfo> {
    let cfg = require_config(&state).await?;
    let meetings_dir = cfg.meetings_dir();
    let recovery_dir = cfg.recovery_dir();
    let store = tokio::task::spawn_blocking(move || {
        MeetingStore::attach(meetings_dir, recovery_dir, PathBuf::from(&raw_path))
    })
    .await
    .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))??;
    let review = ReviewData { store };
    let info = review_info(&review);
    *state.last_meeting.lock().await = Some(review);
    Ok(info)
}

/// Apply review actions: optional meeting rename (design §6.4, §8).
#[tauri::command]
pub async fn apply_review(
    state: State<'_, AppState>,
    meeting_title: Option<String>,
) -> Result<ReviewInfo> {
    let review = {
        let mut guard = state.last_meeting.lock().await;
        guard
            .take()
            .ok_or_else(|| SallyError::Session("no finished meeting to review".into()))?
    };
    let (review, rename_result) = tokio::task::spawn_blocking(move || {
        let mut review = review;
        let result = match &meeting_title {
            Some(title) if !title.trim().is_empty() => review.store.rename_meeting(title),
            _ => Ok(()),
        };
        (review, result)
    })
    .await
    .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))?;
    let info = review_info(&review);
    *state.last_meeting.lock().await = Some(review);
    rename_result?;
    Ok(info)
}

/// Timestamp-free copy; raw file untouched (design §2).
#[tauri::command]
pub async fn export_without_timestamps(state: State<'_, AppState>) -> Result<String> {
    let review = {
        let mut guard = state.last_meeting.lock().await;
        guard
            .take()
            .ok_or_else(|| SallyError::Session("no finished meeting".into()))?
    };
    let (review, result) = tokio::task::spawn_blocking(move || {
        let result = review.store.export_without_timestamps();
        (review, result)
    })
    .await
    .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))?;
    *state.last_meeting.lock().await = Some(review);
    let path = result?;
    Ok(path.to_string_lossy().into_owned())
}

/// Manual, optional cleanup (design §9). Writes the polished file only after
/// every section and the consolidation succeed; never touches the raw file.
#[tauri::command]
pub async fn clean_and_summarize(
    state: State<'_, AppState>,
    include_timestamps: bool,
    include_original: bool,
) -> Result<String> {
    let cfg = require_config(&state).await?;
    let (raw_path, polished_path) = {
        let guard = state.last_meeting.lock().await;
        let review = guard
            .as_ref()
            .ok_or_else(|| SallyError::Session("no finished meeting".into()))?;
        (review.store.raw_path().to_path_buf(), review.store.polished_path())
    };
    let raw_text = tokio::task::spawn_blocking(move || std::fs::read_to_string(raw_path))
        .await
        .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))??;
    let title = raw_text
        .lines()
        .next()
        .unwrap_or("# Meeting")
        .trim_start_matches('#')
        .trim()
        .to_string();

    let client = CleanupClient::new(cfg.api_key.clone(), cfg.cleanup_model.clone());
    let sections = split_sections(&raw_text, SECTION_BUDGET);
    let mut cleaned_parts: Vec<String> = Vec::with_capacity(sections.len());
    for section in &sections {
        // Tail of the previous cleaned section keeps Gemini's inferred
        // speaker labels consistent across section boundaries.
        let context: String = cleaned_parts
            .last()
            .map(|p| {
                let chars: Vec<char> = p.chars().collect();
                let start = chars.len().saturating_sub(600);
                chars[start..].iter().collect()
            })
            .unwrap_or_default();
        cleaned_parts.push(
            client
                .clean_section(section, include_timestamps, include_original, &context)
                .await?,
        );
    }
    let cleaned = cleaned_parts.join("\n\n");
    // The meeting name gives the summary its context (the user typed it in
    // review; the raw file's heading carries it after rename).
    let summary = client
        .summarize(
            &format!("Meeting title: {title}\n\n{cleaned}"),
            &cfg.ui_language,
        )
        .await?;
    let polished = render_polished(&title, &summary, &cleaned, &cfg.ui_language);

    // Publish atomically only after all stages succeeded (design §9).
    let tmp = polished_path.with_extension("md.tmp");
    let result_path = polished_path.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::write(&tmp, polished)?;
        std::fs::rename(&tmp, &polished_path)?;
        Ok(())
    })
    .await
    .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))??;
    Ok(result_path.to_string_lossy().into_owned())
}

/// Recover interrupted meetings from journals into Markdown (design §8.2).
#[tauri::command]
pub async fn recover_meetings(state: State<'_, AppState>) -> Result<Vec<String>> {
    let cfg = require_config(&state).await?;
    let recovery_dir = cfg.recovery_dir();
    let recovered = tokio::task::spawn_blocking(move || MeetingStore::recover(&recovery_dir))
        .await
        .map_err(|e| SallyError::Session(format!("blocking task failed: {e}")))??;
    Ok(recovered
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}
