//! Meeting session orchestrator (design §5).
//!
//! Owns capture, pipeline, the Gemini Live connection, the timeline
//! assembler, and the meeting store for one meeting. Communicates with the
//! UI through Tauri events and with commands through a control channel. The
//! session clock is a monotonic `Instant` (design §5).

use crate::audio::{capture, pipeline::Pipeline, playback::Player, recorder::WavRecorder, RawFrame};
use crate::config::AppConfig;
use crate::error::{Result, SallyError};
use crate::gemini::live::{self, LiveEvent};
use crate::readout::ReadoutGate;
use crate::split::{self, SplitDetector};
use crate::store::{MeetingStore, RecoveryJournal};
use crate::timeline::Assembler;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};

/// Gaps shorter than this after a reconnect are not marked (noise).
const GAP_MARK_THRESHOLD_MS: u64 = 2_000;
/// Reconnect backoff: 1s doubling to this cap, retried while the meeting
/// runs (bounded exponential backoff, design §11).
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Journal snapshot cadence.
const JOURNAL_INTERVAL: Duration = Duration::from_secs(5);
/// A turn left open this long without new fragments is force-finalized so
/// panels do not accumulate an ever-growing provisional entry.
const TURN_IDLE_FLUSH_MS: u64 = 7_000;
/// A speaker-change boundary only splits an entry that already carries this
/// much original text; anything shorter reads fine on one line.
const MIN_SPLIT_CHARS: usize = 12;
/// A turn open this long without a speaker-change or model turn-complete
/// boundary is split anyway, so one uninterrupted speaker never accumulates
/// an unreadably long block ("split every minute").
const MAX_TURN_DURATION_MS: u64 = 60_000;

pub enum Control {
    Pause,
    Resume,
    Stop,
    /// Toggle translated-voice readout mid-meeting.
    SetReadout(bool),
}

/// Returned to the command layer when the meeting ends.
pub struct ReviewData {
    pub store: MeetingStore,
    pub speakers: Vec<String>,
}

pub struct SessionHandle {
    pub control_tx: mpsc::Sender<Control>,
    pub done_rx: Option<oneshot::Receiver<Result<ReviewData>>>,
}

#[derive(Serialize, Clone)]
struct StatusPayload {
    state: String,
    detail: String,
}

fn emit_status(app: &AppHandle, state: &str, detail: &str) {
    let _ = app.emit(
        "sally://status",
        StatusPayload {
            state: state.into(),
            detail: detail.into(),
        },
    );
}

pub fn start(app: AppHandle, config: AppConfig) -> Result<SessionHandle> {
    if config.api_key.trim().is_empty() {
        return Err(SallyError::Config(
            "missing Gemini API key; finish setup first".into(),
        ));
    }
    let store = MeetingStore::create(
        config.meetings_dir(),
        config.recovery_dir(),
        chrono::Local::now(),
        &config.target_language,
    )?;

    let session_start = Instant::now();
    let (frame_tx, frame_rx) = mpsc::channel::<RawFrame>(256);
    let capture_handle = capture::start_capture(
        &config.mic_device,
        &config.system_device,
        &config.capture_app,
        session_start,
        frame_tx,
    )?;
    if !config.capture_app.is_empty() && !capture_handle.app_capture_active {
        let _ = app.emit(
            "sally://warning",
            format!(
                "'{}' has no active audio session — capturing the entire system instead. \
                 Start audio in that app and restart the meeting to capture it alone.",
                config.capture_app
            ),
        );
    }

    let (control_tx, control_rx) = mpsc::channel::<Control>(8);
    let (done_tx, done_rx) = oneshot::channel::<Result<ReviewData>>();

    tokio::spawn(run_session(
        app,
        config,
        store,
        frame_rx,
        control_rx,
        done_tx,
        capture_handle,
    ));

    Ok(SessionHandle {
        control_tx,
        done_rx: Some(done_rx),
    })
}

/// Spawn a background task that keeps trying to connect (bounded backoff)
/// until it succeeds or the session ends, then delivers the connection.
/// `initial_delay` escalates across attempts in the orchestrator so a server
/// that accepts the socket but rejects setup (close right after connect)
/// cannot cause rapid live/reconnecting flapping.
fn spawn_reconnect(
    app: AppHandle,
    config: AppConfig,
    alive: Arc<AtomicBool>,
    api_version: String,
    initial_delay: Duration,
) -> oneshot::Receiver<live::LiveConnection> {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        tokio::time::sleep(initial_delay).await;
        let mut backoff = Duration::from_secs(1);
        loop {
            if !alive.load(Ordering::SeqCst) {
                return;
            }
            match live::connect(
                &config.api_key,
                &config.live_model,
                &config.target_language,
                &api_version,
            )
            .await
            {
                Ok(conn) => {
                    let _ = tx.send(conn);
                    return;
                }
                Err(e) => {
                    emit_status(&app, "reconnecting", &e.to_string());
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });
    rx
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    app: AppHandle,
    config: AppConfig,
    mut store: MeetingStore,
    mut frame_rx: mpsc::Receiver<RawFrame>,
    mut control_rx: mpsc::Receiver<Control>,
    done_tx: oneshot::Sender<Result<ReviewData>>,
    capture_handle: capture::CaptureHandle,
) {
    let mut pipeline = Pipeline::new();
    let mut assembler = Assembler::new();
    // Full meeting timeline kept in memory for the end-of-meeting speaker
    // list and review data.
    let mut sealed_entries: Vec<crate::timeline::TimelineEntry> = Vec::new();
    let mut speakers: BTreeSet<String> = BTreeSet::new();
    let mut paused = false;
    let mut last_chunk_ms: u64 = 0;
    let mut last_fragment_ms: u64 = 0;
    let mut journal_tick = tokio::time::interval(JOURNAL_INTERVAL);
    let alive = Arc::new(AtomicBool::new(true));

    // Readout: translated audio plays only for passages whose source
    // language differs from the target (never Vietnamese-to-Vietnamese).
    let target_code = crate::lang::bcp47(&config.target_language).to_string();
    let mut readout_enabled = config.readout_enabled;
    let mut gate = ReadoutGate::new(&target_code);
    let mut player: Option<Player> = None;
    // Last moment readout audio was audible; original-transcription
    // fragments in this window that are already target-language are our own
    // played translation coming back through loopback, and are dropped.
    let mut last_playback_at: Option<Instant> = None;

    // Optional local recording: mixed 16 kHz chunks streamed to a WAV whose
    // positions line up with transcript timestamps. A recorder failure
    // degrades to "no recording" with a warning — it never ends the meeting.
    let mut recorder: Option<WavRecorder> = if config.save_audio {
        match WavRecorder::create(&store.audio_path()) {
            Ok(r) => Some(r),
            Err(e) => {
                log::error!("meeting recording unavailable: {e}");
                let _ = app.emit(
                    "sally://warning",
                    format!("Meeting recording unavailable: {e}"),
                );
                None
            }
        }
    } else {
        None
    };

    // Language of the currently open entry's original text, for splitting
    // when the spoken language changes mid-stream. Only script-detectable
    // languages participate (Latin-only text stays None and never splits).
    let mut open_lang: Option<&'static str> = None;

    // Speaker-change splitting: the detector arrives asynchronously once
    // the model is ensured (first run downloads ~6 MB). The meeting never
    // waits for it; until it lands, entries just split on turns alone.
    let mut split_det: Option<SplitDetector> = None;
    let mut split_rx: Option<oneshot::Receiver<SplitDetector>> = None;
    if config.speaker_split_enabled {
        let (tx, rx) = oneshot::channel();
        split_rx = Some(rx);
        let data_dir = config.data_dir.clone();
        let url = config.segmentation_model_url.clone();
        tokio::spawn(async move {
            match split::ensure_model(&data_dir, &url).await {
                // Model load happens on the worker thread; start() blocks
                // until it reports, so keep it off the async runtime.
                Ok(path) => match tokio::task::spawn_blocking(move || {
                    SplitDetector::start(&path)
                })
                .await
                {
                    Ok(Ok(d)) => {
                        let _ = tx.send(d);
                    }
                    Ok(Err(e)) => log::warn!("speaker split disabled: {e}"),
                    Err(e) => log::warn!("speaker split disabled: {e}"),
                },
                Err(e) => log::warn!("speaker split disabled: {e}"),
            }
        });
    }

    emit_status(&app, "connecting", "");
    let mut conn: Option<live::LiveConnection> = None;
    let mut api_version = config.live_api_version.clone();
    let mut reconnect_delay = Duration::ZERO;
    let mut connected_at: Option<Instant> = None;
    let mut early_closes = 0u32;
    let mut reconnect_rx: Option<oneshot::Receiver<live::LiveConnection>> =
        Some(spawn_reconnect(
            app.clone(),
            config.clone(),
            alive.clone(),
            api_version.clone(),
            reconnect_delay,
        ));
    let mut gap_start_ms: Option<u64> = None;

    loop {
        tokio::select! {
            // Control from the command layer.
            ctrl = control_rx.recv() => {
                match ctrl {
                    Some(Control::Pause) => {
                        paused = true;
                        if let Some(p) = player.as_ref() {
                            p.clear();
                        }
                        emit_status(&app, "paused", "");
                    }
                    Some(Control::Resume) => {
                        paused = false;
                        emit_status(&app, if conn.is_some() { "live" } else { "reconnecting" }, "");
                    }
                    Some(Control::SetReadout(enabled)) => {
                        readout_enabled = enabled;
                        if !enabled {
                            if let Some(p) = player.as_ref() {
                                p.clear();
                            }
                        }
                    }
                    Some(Control::Stop) | None => break,
                }
            }

            // The speaker-change detector finished loading.
            det = async { split_rx.as_mut().unwrap().await }, if split_rx.is_some() => {
                split_rx = None;
                if let Ok(d) = det {
                    log::info!("speaker split active");
                    split_det = Some(d);
                }
            }

            // A remote voice handed off to a different one: rotate the turn
            // so the next words start a new "Meeting" line. Translation for
            // the frozen entry keeps streaming into it (rotate_turn).
            boundary = async { split_det.as_mut().unwrap().boundary_rx.recv().await },
                if split_det.is_some() => {
                match boundary {
                    Some(_t_ms) => {
                        if !paused
                            && !assembler.open_mic_dominated()
                            && assembler.open_original_len() >= MIN_SPLIT_CHARS
                        {
                            if let Some(sealed) = assembler.rotate_turn() {
                                emit_sealed(&app, sealed, &mut store, &config,
                                            &mut speakers, &mut sealed_entries);
                            }
                            open_lang = None;
                            emit_partial(&app, &assembler);
                        }
                    }
                    None => {
                        // Worker thread ended (it never does unless the
                        // session is tearing down): stop selecting on it.
                        split_det = None;
                    }
                }
            }

            // A background reconnect attempt succeeded.
            newconn = async { reconnect_rx.as_mut().unwrap().await }, if reconnect_rx.is_some() => {
                reconnect_rx = None;
                match newconn {
                    Ok(c) => {
                        conn = Some(c);
                        connected_at = Some(Instant::now());
                        // "live" is emitted on setupComplete (Ready), not
                        // here: a socket that opens but gets its setup
                        // rejected must not flash the live status.
                    }
                    Err(_) => { /* reconnect task ended with the session */ }
                }
            }

            // Raw audio frames from capture.
            frame = frame_rx.recv() => {
                let Some(frame) = frame else { break };
                if paused {
                    continue;
                }
                pipeline.push(frame);
                while let Some(chunk) = pipeline.next_chunk() {
                    last_chunk_ms = chunk.t_ms;
                    // While readout audio is playing, system audio also
                    // contains our own spoken translation (loopback). Audio
                    // still flows to Gemini uninterrupted — muting it made
                    // the whole pipeline stall until playback finished. The
                    // echo is neutralized downstream instead:
                    // echoTargetLanguage=false keeps the model silent for
                    // it, and target-language transcription fragments inside
                    // the playback window are dropped.
                    let readout_playing = player
                        .as_mut()
                        .map(|p| p.is_active())
                        .unwrap_or(false);
                    if readout_playing {
                        last_playback_at = Some(Instant::now());
                    }
                    let record_err = recorder
                        .as_mut()
                        .and_then(|rec| rec.write(chunk.t_ms, &chunk.mixed).err());
                    if let Some(e) = record_err {
                        log::error!("meeting recording stopped: {e}");
                        let _ = app.emit(
                            "sally://warning",
                            format!("Meeting recording stopped: {e}"),
                        );
                        recorder = None;
                    }
                    assembler.push_activity(chunk.mic_active, chunk.system_active, chunk.t_ms);
                    if let Some(d) = split_det.as_ref() {
                        // Readout playback is our own translated voice in
                        // the loopback: fed as silence so the detector
                        // never calls it a new speaker.
                        let _ = d.audio_tx.send(split::Feed {
                            samples: chunk.system,
                            t_ms: chunk.t_ms,
                            suppress: readout_playing,
                        });
                    }
                    if let Some(c) = conn.as_ref() {
                        // try_send: a stalled socket must not block audio.
                        if c.audio_tx.try_send(chunk.mixed).is_err() {
                            log::warn!("live audio queue full; dropping chunk {}", chunk.seq);
                        }
                    }
                }
                if pipeline.take_drop_flag() {
                    log::warn!("audio buffer overflow; oldest audio dropped");
                }
                // Idle turn flush keeps provisional entries bounded.
                if last_fragment_ms > 0
                    && last_chunk_ms.saturating_sub(last_fragment_ms) > TURN_IDLE_FLUSH_MS
                {
                    if readout_enabled {
                        let original = assembler
                            .partial()
                            .map(|p| p.original)
                            .unwrap_or_default();
                        let tail = gate.end_turn(&original);
                        play(&mut player, &mut readout_enabled, &tail, config.readout_speed);
                    }
                    finalize_entry(&app, &mut assembler, &mut store,
                                   &config, &mut speakers, &mut sealed_entries);
                    open_lang = None;
                    last_fragment_ms = 0;
                } else if !paused
                    && assembler.open_original_len() >= MIN_SPLIT_CHARS
                    && assembler
                        .open_start_ms()
                        .map(|start| last_chunk_ms.saturating_sub(start) >= MAX_TURN_DURATION_MS)
                        .unwrap_or(false)
                {
                    // Long uninterrupted turn: split on a time boundary the
                    // same way a speaker-change boundary would, so trailing
                    // translation can still land in the closing entry.
                    if let Some(sealed) = assembler.rotate_turn() {
                        emit_sealed(&app, sealed, &mut store, &config,
                                    &mut speakers, &mut sealed_entries);
                    }
                    open_lang = None;
                    emit_partial(&app, &assembler);
                }
            }

            // Events from the Gemini Live connection.
            event = async { conn.as_mut().unwrap().events_rx.recv().await }, if conn.is_some() => {
                match event {
                    Some(LiveEvent::Ready) => {
                        early_closes = 0;
                        reconnect_delay = Duration::ZERO;
                        if let Some(start) = gap_start_ms.take() {
                            let now = last_chunk_ms.max(start);
                            if now.saturating_sub(start) >= GAP_MARK_THRESHOLD_MS {
                                let gap = assembler.gap(start, now);
                                append_and_emit(&app, &mut store, &config, &gap);
                                sealed_entries.push(gap);
                            }
                        }
                        if !paused {
                            emit_status(&app, "live", "");
                        }
                    }
                    Some(LiveEvent::Original(text)) => {
                        // Drop our own readout echo: target-language
                        // fragments while (or just after) the translated
                        // voice was audible are the played translation
                        // coming back through loopback capture.
                        // Wide window: the echo's own transcription can trail
                        // the played audio by several seconds.
                        let echo_window = last_playback_at
                            .map(|t| t.elapsed() < Duration::from_millis(4000))
                            .unwrap_or(false);
                        let frag_lang = crate::lang::detect(&text);
                        if echo_window
                            && frag_lang.map(|l| l == target_code).unwrap_or(false)
                        {
                            continue;
                        }
                        // Language changed mid-stream (e.g. Japanese hand-off
                        // to Vietnamese): split so each entry stays in one
                        // language. Latin-generic text detects as None and
                        // never triggers this.
                        if let (Some(f), Some(o)) = (frag_lang, open_lang) {
                            if f != o
                                && assembler.open_original_len() >= MIN_SPLIT_CHARS
                            {
                                if let Some(sealed) = assembler.rotate_turn() {
                                    emit_sealed(&app, sealed, &mut store, &config,
                                                &mut speakers, &mut sealed_entries);
                                }
                            }
                        }
                        if frag_lang.is_some() {
                            open_lang = frag_lang;
                        }
                        assembler.push_original(&text, last_chunk_ms);
                        last_fragment_ms = last_chunk_ms;
                        emit_partial(&app, &assembler);
                    }
                    Some(LiveEvent::Translated(text)) => {
                        assembler.push_translated(&text, last_chunk_ms);
                        last_fragment_ms = last_chunk_ms;
                        emit_partial(&app, &assembler);
                    }
                    Some(LiveEvent::Audio(samples)) => {
                        if readout_enabled && !paused {
                            let original = assembler
                                .partial()
                                .map(|p| p.original)
                                .unwrap_or_default();
                            let playable = gate.push_audio(samples, &original);
                            play(&mut player, &mut readout_enabled, &playable, config.readout_speed);
                        }
                    }
                    Some(LiveEvent::TurnComplete) => {
                        if readout_enabled {
                            let original = assembler
                                .partial()
                                .map(|p| p.original)
                                .unwrap_or_default();
                            let tail = gate.end_turn(&original);
                            play(&mut player, &mut readout_enabled, &tail, config.readout_speed);
                        }
                        // Rotate instead of sealing immediately: the entry
                        // stays open one more turn so trailing translation
                        // fragments can land in it.
                        if let Some(sealed) = assembler.rotate_turn() {
                            emit_sealed(&app, sealed, &mut store, &config,
                                        &mut speakers, &mut sealed_entries);
                        }
                        open_lang = None;
                        last_fragment_ms = 0;
                    }
                    other => {
                        // Closed event, or `None` when the reader task ended
                        // without a Close frame: reconnect and mark the gap.
                        let reason = match other {
                            Some(LiveEvent::Closed(r)) => r,
                            _ => "connection lost".to_string(),
                        };
                        log::warn!("live connection closed: {reason}");
                        conn = None;
                        if gap_start_ms.is_none() {
                            gap_start_ms = Some(last_chunk_ms);
                        }
                        // A close shortly after connecting means setup was
                        // rejected, not a network drop. After three of those
                        // in a row, try the other API version — preview
                        // models move between v1alpha and v1beta.
                        let early = connected_at
                            .map(|t| t.elapsed() < Duration::from_secs(5))
                            .unwrap_or(false);
                        connected_at = None;
                        // 1007 schema errors ("Invalid JSON payload…Unknown
                        // name") mean this API version rejects our setup
                        // shape — flip immediately instead of after three
                        // tries.
                        let schema_reject = reason.contains("Unknown name")
                            || reason.contains("Invalid JSON payload");
                        if early {
                            early_closes += 1;
                            if schema_reject || early_closes >= 3 {
                                early_closes = 0;
                                api_version = if api_version == "v1alpha" {
                                    "v1beta".into()
                                } else {
                                    "v1alpha".into()
                                };
                                log::warn!(
                                    "live setup rejected; trying API version {api_version}"
                                );
                            }
                        }
                        reconnect_delay = (reconnect_delay * 2 + Duration::from_secs(1))
                            .min(MAX_BACKOFF);
                        emit_status(&app, "reconnecting", &reason);
                        if reconnect_rx.is_none() {
                            reconnect_rx = Some(spawn_reconnect(
                                app.clone(),
                                config.clone(),
                                alive.clone(),
                                api_version.clone(),
                                reconnect_delay,
                            ));
                        }
                    }
                }
            }

            // Periodic recovery journal (design §8.2).
            _ = journal_tick.tick() => {
                let partial = assembler.partial();
                let journal = RecoveryJournal {
                    open_original: partial.as_ref().map(|p| p.original.clone()).unwrap_or_default(),
                    open_translated: partial.as_ref().map(|p| p.translated.clone()).unwrap_or_default(),
                    open_start_ms: partial.as_ref().map(|p| p.start_ms).unwrap_or(0),
                    ..Default::default()
                };
                if let Err(e) = store.write_journal(&journal) {
                    emit_status(&app, "storage-error", &e.to_string());
                }
            }
        }
    }

    // Meeting end: finalize open entries and close the store. The raw file
    // was appended as entries sealed, so no rewrite is needed — ending is
    // immediate.
    alive.store(false, Ordering::SeqCst);
    capture_handle.stop();
    finalize_entry(&app, &mut assembler, &mut store, &config,
                   &mut speakers, &mut sealed_entries);
    if let Some(rec) = recorder.as_mut() {
        if let Err(e) = rec.finalize() {
            log::error!("recording finalize failed: {e}");
        }
    }
    drop(conn);
    if let Err(e) = store.finalize() {
        log::error!("finalize failed: {e}");
    }
    emit_status(&app, "ended", "");
    // Speaker list from the reconciled timeline, not the provisional seals.
    let mut speakers: BTreeSet<String> = sealed_entries
        .iter()
        .filter(|e| !e.speaker.is_empty())
        .map(|e| e.speaker.clone())
        .collect();
    speakers.insert("You".into());
    let _ = done_tx.send(Ok(ReviewData {
        store,
        speakers: speakers.into_iter().collect(),
    }));
}

/// Send gated samples to the output device, opening it lazily. If no output
/// device exists, readout turns itself off instead of failing the session.
fn play(player: &mut Option<Player>, readout_enabled: &mut bool, samples: &[i16], speed: f32) {
    if samples.is_empty() {
        return;
    }
    if player.is_none() {
        match Player::new(speed) {
            Ok(p) => *player = Some(p),
            Err(e) => {
                log::error!("readout unavailable: {e}");
                *readout_enabled = false;
                return;
            }
        }
    }
    if let Some(p) = player.as_mut() {
        p.push(samples);
    }
}

/// Post-process and persist one sealed entry.
fn emit_sealed(
    app: &AppHandle,
    mut entry: crate::timeline::TimelineEntry,
    store: &mut MeetingStore,
    config: &AppConfig,
    speakers: &mut BTreeSet<String>,
    sealed_entries: &mut Vec<crate::timeline::TimelineEntry>,
) {
    // echoTargetLanguage=false means passages already in the target
    // language get no model translation; mirror the original so the
    // translation panel stays complete.
    if entry.translated.is_empty()
        && crate::lang::detect(&entry.original)
            .map(|l| l == crate::lang::bcp47(&config.target_language))
            .unwrap_or(false)
    {
        entry.translated = entry.original.clone();
    }
    speakers.insert(entry.speaker.clone());
    append_and_emit(app, store, config, &entry);
    sealed_entries.push(entry);
}

fn finalize_entry(
    app: &AppHandle,
    assembler: &mut Assembler,
    store: &mut MeetingStore,
    config: &AppConfig,
    speakers: &mut BTreeSet<String>,
    sealed_entries: &mut Vec<crate::timeline::TimelineEntry>,
) {
    for entry in assembler.finalize_turn() {
        emit_sealed(app, entry, store, config, speakers, sealed_entries);
    }
}

fn append_and_emit(
    app: &AppHandle,
    store: &mut MeetingStore,
    config: &AppConfig,
    entry: &crate::timeline::TimelineEntry,
) {
    if let Err(e) = store.append_entry(entry, &config.target_language) {
        emit_status(app, "storage-error", &e.to_string());
    }
    let _ = app.emit("sally://entry", entry.clone());
    let _ = app.emit("sally://partial", json!(null));
}

fn emit_partial(app: &AppHandle, assembler: &Assembler) {
    if let Some(p) = assembler.partial() {
        let _ = app.emit("sally://partial", p);
    }
}
