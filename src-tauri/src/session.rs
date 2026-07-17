//! Meeting session orchestrator (design §5).
//!
//! Owns capture, pipeline, diarization, the Gemini Live connection, the
//! timeline assembler, and the meeting store for one meeting. Communicates
//! with the UI through Tauri events and with commands through a control
//! channel. The session clock is a monotonic `Instant` (design §5).

use crate::audio::{capture, pipeline::Pipeline, RawFrame};
use crate::config::AppConfig;
use crate::diarization::Diarizer;
use crate::error::{Result, SallyError};
use crate::gemini::live::{self, LiveEvent};
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

pub enum Control {
    Pause,
    Resume,
    Stop,
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
        session_start,
        frame_tx,
    )?;

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
fn spawn_reconnect(
    app: AppHandle,
    config: AppConfig,
    alive: Arc<AtomicBool>,
) -> oneshot::Receiver<live::LiveConnection> {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            if !alive.load(Ordering::SeqCst) {
                return;
            }
            match live::connect(&config.api_key, &config.live_model, &config.target_language)
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
    let mut diarizer = config.diarization_enabled.then(Diarizer::new);
    let mut speakers: BTreeSet<String> = BTreeSet::new();
    let mut paused = false;
    let mut last_chunk_ms: u64 = 0;
    let mut last_fragment_ms: u64 = 0;
    let mut journal_tick = tokio::time::interval(JOURNAL_INTERVAL);
    let alive = Arc::new(AtomicBool::new(true));

    emit_status(&app, "connecting", "");
    let mut conn: Option<live::LiveConnection> = None;
    let mut reconnect_rx: Option<oneshot::Receiver<live::LiveConnection>> =
        Some(spawn_reconnect(app.clone(), config.clone(), alive.clone()));
    let mut gap_start_ms: Option<u64> = None;

    loop {
        tokio::select! {
            // Control from the command layer.
            ctrl = control_rx.recv() => {
                match ctrl {
                    Some(Control::Pause) => {
                        paused = true;
                        emit_status(&app, "paused", "");
                    }
                    Some(Control::Resume) => {
                        paused = false;
                        emit_status(&app, if conn.is_some() { "live" } else { "reconnecting" }, "");
                    }
                    Some(Control::Stop) | None => break,
                }
            }

            // A background reconnect attempt succeeded.
            newconn = async { reconnect_rx.as_mut().unwrap().await }, if reconnect_rx.is_some() => {
                reconnect_rx = None;
                match newconn {
                    Ok(c) => {
                        conn = Some(c);
                        if let Some(start) = gap_start_ms.take() {
                            let now = last_chunk_ms.max(start);
                            if now.saturating_sub(start) >= GAP_MARK_THRESHOLD_MS {
                                let gap = assembler.gap(start, now);
                                append_and_emit(&app, &mut store, &config, &gap);
                            }
                        }
                        if !paused {
                            emit_status(&app, "live", "");
                        }
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
                    if let Some(d) = diarizer.as_mut() {
                        d.push_chunk(&chunk.system, chunk.t_ms);
                    }
                    assembler.push_mic_activity(chunk.mic_active, chunk.t_ms);
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
                    finalize_entry(&app, &mut assembler, diarizer.as_ref(), &mut store,
                                   &config, &mut speakers);
                    last_fragment_ms = 0;
                }
            }

            // Events from the Gemini Live connection.
            event = async { conn.as_mut().unwrap().events_rx.recv().await }, if conn.is_some() => {
                match event {
                    Some(LiveEvent::Ready) => {
                        if !paused {
                            emit_status(&app, "live", "");
                        }
                    }
                    Some(LiveEvent::Original(text)) => {
                        assembler.push_original(&text, last_chunk_ms);
                        last_fragment_ms = last_chunk_ms;
                        emit_partial(&app, &assembler);
                    }
                    Some(LiveEvent::Translated(text)) => {
                        assembler.push_translated(&text, last_chunk_ms);
                        last_fragment_ms = last_chunk_ms;
                        emit_partial(&app, &assembler);
                    }
                    Some(LiveEvent::TurnComplete) => {
                        finalize_entry(&app, &mut assembler, diarizer.as_ref(), &mut store,
                                       &config, &mut speakers);
                        last_fragment_ms = 0;
                    }
                    other => {
                        // Closed event, or `None` when the reader task ended
                        // without a Close frame: reconnect and mark the gap.
                        let reason = match other {
                            Some(LiveEvent::Closed(r)) => r,
                            _ => "connection lost".to_string(),
                        };
                        conn = None;
                        if gap_start_ms.is_none() {
                            gap_start_ms = Some(last_chunk_ms);
                        }
                        emit_status(&app, "reconnecting", &reason);
                        if reconnect_rx.is_none() {
                            reconnect_rx = Some(spawn_reconnect(
                                app.clone(),
                                config.clone(),
                                alive.clone(),
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

    // Meeting end: finalize open entry, close diarization, remove journal.
    alive.store(false, Ordering::SeqCst);
    capture_handle.stop();
    finalize_entry(&app, &mut assembler, diarizer.as_ref(), &mut store, &config, &mut speakers);
    if let Some(d) = diarizer.as_mut() {
        d.finish();
    }
    drop(conn);
    if let Err(e) = store.finalize() {
        log::error!("finalize failed: {e}");
    }
    emit_status(&app, "ended", "");
    let _ = done_tx.send(Ok(ReviewData {
        store,
        speakers: speakers.into_iter().collect(),
    }));
}

fn finalize_entry(
    app: &AppHandle,
    assembler: &mut Assembler,
    diarizer: Option<&Diarizer>,
    store: &mut MeetingStore,
    config: &AppConfig,
    speakers: &mut BTreeSet<String>,
) {
    if let Some(entry) = assembler.finalize_turn(diarizer) {
        speakers.insert(entry.speaker.clone());
        append_and_emit(app, store, config, &entry);
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
