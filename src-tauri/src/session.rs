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
use crate::split::{self, SplitDetector};
use crate::store::{MeetingStore, RecoveryJournal};
use crate::timeline::Assembler;
use serde::Serialize;
use serde_json::json;
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
/// A pending speaker-change split waits for a sentence boundary, but never
/// accumulates more than this much additional text before splitting anyway
/// (a speaker who never punctuates must not merge two voices forever).
const PENDING_SPLIT_MAX_EXTRA_CHARS: usize = 160;

pub enum Control {
    Pause,
    Resume,
    Stop,
    /// Toggle translated-voice readout mid-meeting.
    SetReadout(bool),
    /// Change readout volume (0.0–1.0) mid-meeting.
    SetReadoutVolume(f32),
}

/// Returned to the command layer when the meeting ends.
pub struct ReviewData {
    pub store: MeetingStore,
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
        &config.mac_capture_method,
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

/// All per-meeting state shared across the control/frame/live-event
/// handling below. Bundled into one struct (rather than ~25 loose locals in
/// `run_session`) so the frame- and live-event-handling logic can be moved
/// into named methods instead of living inline in two very large
/// `tokio::select!` arms.
struct Meeting {
    app: AppHandle,
    config: AppConfig,
    store: MeetingStore,
    pipeline: Pipeline,
    assembler: Assembler,
    /// Full meeting timeline kept in memory for the end-of-meeting speaker
    /// list and review data.
    sealed_entries: Vec<crate::timeline::TimelineEntry>,
    paused: bool,
    last_chunk_ms: u64,
    last_fragment_ms: u64,
    /// Readout: translated audio plays for every remote (Meeting) passage,
    /// regardless of source language — including source == target, so a
    /// Vietnamese-to-Vietnamese "translation" is dubbed same as any other.
    /// Mic (You) speech is never read back; it only ever reaches the raw
    /// transcript.
    readout_enabled: bool,
    readout_volume: f32,
    player: Option<Player>,
    recorder: Option<WavRecorder>,
    /// Language of the currently open entry's original text, for splitting
    /// when the spoken language changes mid-stream. Only script-detectable
    /// languages participate (Latin-only text stays None and never splits).
    open_lang: Option<&'static str>,
    /// A speaker-change boundary arrived but the rotation is deferred until
    /// the open entry's text reaches a sentence end, so the previous
    /// speaker's lagging transcription drains into their own entry.
    rotate_pending: bool,
    pending_since_len: usize,
    /// Speaker-change splitting: the detector arrives asynchronously once
    /// the model is ensured (first run downloads ~6 MB). The meeting never
    /// waits for it; until it lands, entries just split on turns alone.
    split_det: Option<SplitDetector>,
    conn: Option<live::LiveConnection>,
    api_version: String,
    reconnect_delay: Duration,
    connected_at: Option<Instant>,
    early_closes: u32,
    reconnect_rx: Option<oneshot::Receiver<live::LiveConnection>>,
    gap_start_ms: Option<u64>,
    alive: Arc<AtomicBool>,
}

impl Meeting {
    /// Process one raw audio frame from capture: mix/resample via the
    /// pipeline, feed the recorder/split-detector/live connection, and run
    /// the idle-turn-flush and long-turn-split checks. Only called when the
    /// meeting isn't paused (the caller checks `paused` before this).
    fn handle_frame(&mut self, frame: RawFrame) {
        self.pipeline.push(frame);
        while let Some(chunk) = self.pipeline.next_chunk() {
            self.last_chunk_ms = chunk.t_ms;
            // Selecting a specific app/tab as the capture source (per-app
            // loopback on Windows, the Core Audio tap on macOS) means
            // Sally's own readout output is structurally excluded from what
            // gets captured — it belongs to a different process, not the
            // selected app. Audio still flows to Gemini uninterrupted;
            // muting it made the whole pipeline stall until playback
            // finished.
            let readout_playing = self.player.as_mut().map(|p| p.is_active()).unwrap_or(false);
            let record_err = self
                .recorder
                .as_mut()
                .and_then(|rec| rec.write(chunk.t_ms, &chunk.mixed).err());
            if let Some(e) = record_err {
                log::error!("meeting recording stopped: {e}");
                let _ = self
                    .app
                    .emit("sally://warning", format!("Meeting recording stopped: {e}"));
                self.recorder = None;
            }
            self.assembler
                .push_activity(chunk.mic_active, chunk.system_active, chunk.t_ms);
            if let Some(d) = self.split_det.as_ref() {
                // Readout playback is our own translated voice in
                // the loopback: fed as silence so the detector
                // never calls it a new speaker.
                let _ = d.audio_tx.send(split::Feed {
                    samples: chunk.system,
                    t_ms: chunk.t_ms,
                    suppress: readout_playing,
                });
            }
            if let Some(c) = self.conn.as_ref() {
                // try_send: a stalled socket must not block audio.
                if c.audio_tx.try_send(chunk.mixed).is_err() {
                    log::warn!("live audio queue full; dropping chunk {}", chunk.seq);
                }
            }
        }
        if self.pipeline.take_drop_flag() {
            log::warn!("audio buffer overflow; oldest audio dropped");
        }
        // Idle turn flush keeps provisional entries bounded.
        if self.last_fragment_ms > 0
            && self.last_chunk_ms.saturating_sub(self.last_fragment_ms) > TURN_IDLE_FLUSH_MS
        {
            finalize_entry(
                &self.app,
                &mut self.assembler,
                &mut self.store,
                &self.config,
                &mut self.sealed_entries,
            );
            self.open_lang = None;
            self.rotate_pending = false;
            self.last_fragment_ms = 0;
        } else if !self.paused
            && self.assembler.open_original_len() >= MIN_SPLIT_CHARS
            && self
                .assembler
                .open_start_ms()
                .map(|start| self.last_chunk_ms.saturating_sub(start) >= MAX_TURN_DURATION_MS)
                .unwrap_or(false)
        {
            // Long uninterrupted turn: split on a time boundary the
            // same way a speaker-change boundary would, so trailing
            // translation can still land in the closing entry.
            if let Some(sealed) = self.assembler.rotate_turn() {
                emit_sealed(
                    &self.app,
                    sealed,
                    &mut self.store,
                    &self.config,
                    &mut self.sealed_entries,
                );
            }
            self.open_lang = None;
            self.rotate_pending = false;
            emit_partial(&self.app, &self.assembler);
        }
    }

    /// Process one event from the Gemini Live connection: transcription
    /// fragments, translated-audio playback, turn boundaries, and
    /// connection-lifecycle events (Ready / Closed / stream ended).
    fn handle_live_event(&mut self, event: Option<LiveEvent>) {
        match event {
            Some(LiveEvent::Ready) => {
                self.early_closes = 0;
                self.reconnect_delay = Duration::ZERO;
                if let Some(start) = self.gap_start_ms.take() {
                    let now = self.last_chunk_ms.max(start);
                    if now.saturating_sub(start) >= GAP_MARK_THRESHOLD_MS {
                        let gap = self.assembler.gap(start, now);
                        append_and_emit(&self.app, &mut self.store, &self.config, &gap);
                        self.sealed_entries.push(gap);
                    }
                }
                if !self.paused {
                    emit_status(&self.app, "live", "");
                }
            }
            Some(LiveEvent::Original(text)) => {
                let frag_lang = crate::lang::detect(&text);
                // Deferred speaker-change split: the previous
                // speaker's text already ends at a sentence, so this
                // fragment starts the new voice's entry.
                if self.rotate_pending && self.assembler.open_ends_sentence() {
                    if let Some(sealed) = self.assembler.rotate_turn() {
                        emit_sealed(
                            &self.app,
                            sealed,
                            &mut self.store,
                            &self.config,
                            &mut self.sealed_entries,
                        );
                    }
                    self.open_lang = None;
                    self.rotate_pending = false;
                }
                // Language changed mid-stream (e.g. Japanese hand-off
                // to Vietnamese): split so each entry stays in one
                // language. Latin-generic text detects as None and
                // never triggers this.
                if let (Some(f), Some(o)) = (frag_lang, self.open_lang) {
                    if f != o && self.assembler.open_original_len() >= MIN_SPLIT_CHARS {
                        if let Some(sealed) = self.assembler.rotate_turn() {
                            emit_sealed(
                                &self.app,
                                sealed,
                                &mut self.store,
                                &self.config,
                                &mut self.sealed_entries,
                            );
                        }
                        self.rotate_pending = false;
                    }
                }
                if frag_lang.is_some() {
                    self.open_lang = frag_lang;
                }
                self.assembler.push_original(&text, self.last_chunk_ms);
                // The fragment just appended may have completed the
                // previous speaker's sentence — split now so the
                // next fragment starts fresh. The size cap keeps an
                // unpunctuated stream from merging voices forever.
                if self.rotate_pending
                    && (self.assembler.open_ends_sentence()
                        || self.assembler.open_original_len()
                            > self.pending_since_len + PENDING_SPLIT_MAX_EXTRA_CHARS)
                {
                    if let Some(sealed) = self.assembler.rotate_turn() {
                        emit_sealed(
                            &self.app,
                            sealed,
                            &mut self.store,
                            &self.config,
                            &mut self.sealed_entries,
                        );
                    }
                    self.open_lang = None;
                    self.rotate_pending = false;
                }
                self.last_fragment_ms = self.last_chunk_ms;
                emit_partial(&self.app, &self.assembler);
            }
            Some(LiveEvent::Translated(text)) => {
                self.assembler.push_translated(&text, self.last_chunk_ms);
                self.last_fragment_ms = self.last_chunk_ms;
                emit_partial(&self.app, &self.assembler);
            }
            Some(LiveEvent::Audio(samples)) => {
                // Never read the user's own mic speech back to them
                // translated — only remote (Meeting) turns qualify. Beyond
                // that, no gating: Gemini translates every passage
                // uniformly (echoTargetLanguage: true), so this plays
                // regardless of source language, including source == target.
                if self.readout_enabled && !self.paused && !self.assembler.open_mic_dominated() {
                    play(
                        &mut self.player,
                        &mut self.readout_enabled,
                        &samples,
                        self.readout_volume,
                    );
                }
            }
            Some(LiveEvent::TurnComplete) => {
                // Rotate instead of sealing immediately: the entry
                // stays open one more turn so trailing translation
                // fragments can land in it.
                if let Some(sealed) = self.assembler.rotate_turn() {
                    emit_sealed(
                        &self.app,
                        sealed,
                        &mut self.store,
                        &self.config,
                        &mut self.sealed_entries,
                    );
                }
                self.open_lang = None;
                self.rotate_pending = false;
                self.last_fragment_ms = 0;
            }
            other => {
                // Closed event, or `None` when the reader task ended
                // without a Close frame: reconnect and mark the gap.
                let reason = match other {
                    Some(LiveEvent::Closed(r)) => r,
                    _ => "connection lost".to_string(),
                };
                log::warn!("live connection closed: {reason}");
                self.conn = None;
                if self.gap_start_ms.is_none() {
                    self.gap_start_ms = Some(self.last_chunk_ms);
                }
                // A close shortly after connecting means setup was
                // rejected, not a network drop. After three of those
                // in a row, try the other API version — preview
                // models move between v1alpha and v1beta.
                let early = self
                    .connected_at
                    .map(|t| t.elapsed() < Duration::from_secs(5))
                    .unwrap_or(false);
                self.connected_at = None;
                // 1007 schema errors ("Invalid JSON payload…Unknown
                // name") mean this API version rejects our setup
                // shape — flip immediately instead of after three
                // tries.
                let schema_reject =
                    reason.contains("Unknown name") || reason.contains("Invalid JSON payload");
                if early {
                    self.early_closes += 1;
                    if schema_reject || self.early_closes >= 3 {
                        self.early_closes = 0;
                        self.api_version = if self.api_version == "v1alpha" {
                            "v1beta".into()
                        } else {
                            "v1alpha".into()
                        };
                        log::warn!("live setup rejected; trying API version {}", self.api_version);
                    }
                }
                self.reconnect_delay = (self.reconnect_delay * 2 + Duration::from_secs(1)).min(MAX_BACKOFF);
                emit_status(&self.app, "reconnecting", &reason);
                if self.reconnect_rx.is_none() {
                    self.reconnect_rx = Some(spawn_reconnect(
                        self.app.clone(),
                        self.config.clone(),
                        self.alive.clone(),
                        self.api_version.clone(),
                        self.reconnect_delay,
                    ));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    app: AppHandle,
    config: AppConfig,
    store: MeetingStore,
    mut frame_rx: mpsc::Receiver<RawFrame>,
    mut control_rx: mpsc::Receiver<Control>,
    done_tx: oneshot::Sender<Result<ReviewData>>,
    capture_handle: capture::CaptureHandle,
) {
    let mut journal_tick = tokio::time::interval(JOURNAL_INTERVAL);
    let alive = Arc::new(AtomicBool::new(true));

    // Optional local recording: mixed 16 kHz chunks streamed to a WAV whose
    // positions line up with transcript timestamps. A recorder failure
    // degrades to "no recording" with a warning — it never ends the meeting.
    let recorder: Option<WavRecorder> = if config.save_audio {
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

    // Speaker-change splitting: the detector arrives asynchronously once
    // the model is ensured (first run downloads ~6 MB). The meeting never
    // waits for it; until it lands, entries just split on turns alone.
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
                Ok(path) => match tokio::task::spawn_blocking(move || SplitDetector::start(&path)).await {
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
    let api_version = config.live_api_version.clone();
    let reconnect_delay = Duration::ZERO;
    let reconnect_rx = Some(spawn_reconnect(
        app.clone(),
        config.clone(),
        alive.clone(),
        api_version.clone(),
        reconnect_delay,
    ));

    let mut state = Meeting {
        pipeline: Pipeline::new(),
        assembler: Assembler::new(),
        sealed_entries: Vec::new(),
        paused: false,
        last_chunk_ms: 0,
        last_fragment_ms: 0,
        readout_enabled: config.readout_enabled,
        readout_volume: config.readout_volume,
        player: None,
        recorder,
        open_lang: None,
        rotate_pending: false,
        pending_since_len: 0,
        split_det: None,
        conn: None,
        api_version,
        reconnect_delay,
        connected_at: None,
        early_closes: 0,
        reconnect_rx,
        gap_start_ms: None,
        alive: alive.clone(),
        app,
        config,
        store,
    };

    loop {
        tokio::select! {
            // Control from the command layer.
            ctrl = control_rx.recv() => {
                match ctrl {
                    Some(Control::Pause) => {
                        state.paused = true;
                        if let Some(p) = state.player.as_ref() {
                            p.clear();
                        }
                        emit_status(&state.app, "paused", "");
                    }
                    Some(Control::Resume) => {
                        state.paused = false;
                        emit_status(&state.app, if state.conn.is_some() { "live" } else { "reconnecting" }, "");
                    }
                    Some(Control::SetReadout(enabled)) => {
                        state.readout_enabled = enabled;
                        if !enabled {
                            if let Some(p) = state.player.as_ref() {
                                p.clear();
                            }
                        }
                    }
                    Some(Control::SetReadoutVolume(v)) => {
                        state.readout_volume = v.clamp(0.0, 1.0);
                        if let Some(p) = state.player.as_ref() {
                            p.set_volume(state.readout_volume);
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
                    state.split_det = Some(d);
                }
            }

            // A remote voice handed off to a different one. The rotation is
            // NOT applied immediately: transcription lags the audio by a
            // couple of seconds, so the previous speaker's last words are
            // still in flight and would land in the new speaker's entry.
            // Instead the split goes pending and fires once the open entry's
            // text reaches a sentence boundary (or a size cap), letting the
            // tail drain into its own entry first.
            boundary = async { state.split_det.as_mut().unwrap().boundary_rx.recv().await },
                if state.split_det.is_some() => {
                match boundary {
                    Some(_t_ms) => {
                        if !state.paused
                            && !state.assembler.open_mic_dominated()
                            && state.assembler.open_original_len() >= MIN_SPLIT_CHARS
                        {
                            state.rotate_pending = true;
                            state.pending_since_len = state.assembler.open_original_len();
                        }
                    }
                    None => {
                        // Worker thread ended (it never does unless the
                        // session is tearing down): stop selecting on it.
                        state.split_det = None;
                    }
                }
            }

            // A background reconnect attempt succeeded.
            newconn = async { state.reconnect_rx.as_mut().unwrap().await }, if state.reconnect_rx.is_some() => {
                state.reconnect_rx = None;
                match newconn {
                    Ok(c) => {
                        state.conn = Some(c);
                        state.connected_at = Some(Instant::now());
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
                if state.paused {
                    continue;
                }
                state.handle_frame(frame);
            }

            // Events from the Gemini Live connection.
            event = async { state.conn.as_mut().unwrap().events_rx.recv().await }, if state.conn.is_some() => {
                state.handle_live_event(event);
            }

            // Periodic recovery journal (design §8.2).
            _ = journal_tick.tick() => {
                let partial = state.assembler.partial();
                let journal = RecoveryJournal {
                    open_original: partial.as_ref().map(|p| p.original.clone()).unwrap_or_default(),
                    open_translated: partial.as_ref().map(|p| p.translated.clone()).unwrap_or_default(),
                    open_start_ms: partial.as_ref().map(|p| p.start_ms).unwrap_or(0),
                    ..Default::default()
                };
                if let Err(e) = state.store.write_journal(&journal) {
                    emit_status(&state.app, "storage-error", &e.to_string());
                }
            }
        }
    }

    // Meeting end: finalize open entries and close the store. The raw file
    // was appended as entries sealed, so no rewrite is needed — ending is
    // immediate.
    alive.store(false, Ordering::SeqCst);
    capture_handle.stop();
    finalize_entry(
        &state.app,
        &mut state.assembler,
        &mut state.store,
        &state.config,
        &mut state.sealed_entries,
    );
    if let Some(rec) = state.recorder.as_mut() {
        if let Err(e) = rec.finalize() {
            log::error!("recording finalize failed: {e}");
        }
    }
    drop(state.conn);
    if let Err(e) = state.store.finalize() {
        log::error!("finalize failed: {e}");
    }
    emit_status(&state.app, "ended", "");
    let _ = done_tx.send(Ok(ReviewData { store: state.store }));
}

/// Send gated samples to the output device, opening it lazily. If no output
/// device exists, readout turns itself off instead of failing the session.
fn play(
    player: &mut Option<Player>,
    readout_enabled: &mut bool,
    samples: &[i16],
    volume: f32,
) {
    if samples.is_empty() {
        return;
    }
    if player.is_none() {
        match Player::new(volume) {
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
    entry: crate::timeline::TimelineEntry,
    store: &mut MeetingStore,
    config: &AppConfig,
    sealed_entries: &mut Vec<crate::timeline::TimelineEntry>,
) {
    append_and_emit(app, store, config, &entry);
    sealed_entries.push(entry);
}

fn finalize_entry(
    app: &AppHandle,
    assembler: &mut Assembler,
    store: &mut MeetingStore,
    config: &AppConfig,
    sealed_entries: &mut Vec<crate::timeline::TimelineEntry>,
) {
    for entry in assembler.finalize_turn() {
        emit_sealed(app, entry, store, config, sealed_entries);
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
