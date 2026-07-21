//! cpal-based capture adapter.
//!
//! Windows: microphone via default/selected input device, system audio via
//! WASAPI loopback (an input stream opened on an output device).
//!
//! macOS: microphone works through cpal; system audio tries native paths in
//! order, falling back to a loopback *input* device (BlackHole or similar)
//! only when none of them can start:
//!   1. Core Audio process tap (`coreaudio_tap.rs`) on macOS 14.4+ — no
//!      Screen Recording permission needed.
//!   2. ScreenCaptureKit (`sck_capture.rs`) on macOS 13.0+ — needs Screen
//!      Recording permission, tried on 14.4+ too in case the tap API is
//!      unavailable for some other reason.
//!   3. A user-selected loopback device (BlackHole or similar).

use super::{AudioSource, RawFrame};
use crate::error::{Result, SallyError};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub struct CaptureHandle {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// True when system audio comes from a single app (process loopback)
    /// rather than the whole output device.
    pub app_capture_active: bool,
}

impl CaptureHandle {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// The microphone capture thread's lifecycle, managed independently of
/// `CaptureHandle`'s system-audio thread(s) so it can be torn down and
/// restarted with a different device mid-meeting (e.g. after the original
/// device is unplugged) without touching system-audio capture at all.
pub struct MicCapture {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub device: String,
}

impl MicCapture {
    /// Stop the current mic thread and join it. Safe to call more than
    /// once (a second call is a no-op) — both an explicit stop before
    /// restarting and the `Drop` impl can run without double-joining.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start (or restart) microphone capture alone. Used both for the initial
/// capture in `start_capture` and to switch devices mid-meeting after the
/// previous one disconnects.
pub fn spawn_mic(
    device_name: String,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
) -> Result<MicCapture> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread = spawn_mic_thread(device_name.clone(), session_start, tx, stop.clone())?;
    Ok(MicCapture {
        stop,
        thread: Some(thread),
        device: device_name,
    })
}

pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Candidates for the "system audio" picker. Windows: output devices
/// (WASAPI loopback opens an input stream on them). macOS: loopback *input*
/// devices such as BlackHole — capturing an output device directly is not
/// possible without ScreenCaptureKit.
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    #[cfg(target_os = "macos")]
    return host
        .input_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    #[cfg(not(target_os = "macos"))]
    host.output_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Start microphone and system-audio capture. Frames are pushed to `tx`
/// with timestamps from `session_start` (monotonic clock, design §5).
/// `capture_app` selects a single application for system audio — process
/// loopback on Windows, a ScreenCaptureKit app filter on macOS; empty
/// captures the whole device. `mac_capture_method` overrides macOS's
/// automatic tap/ScreenCaptureKit choice ("auto", "tap", or
/// "screencapturekit"); ignored elsewhere. The mic capture is returned
/// separately so the caller can restart just that lane (e.g. after the
/// device disconnects) without touching system-audio capture.
pub fn start_capture(
    mic_device: &str,
    system_device: &str,
    capture_app: &str,
    mac_capture_method: &str,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
) -> Result<(MicCapture, CaptureHandle)> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let mut app_capture_active = false;

    let mic_capture = spawn_mic(mic_device.to_string(), session_start, tx.clone())?;

    #[cfg(windows)]
    if !capture_app.is_empty() {
        match super::app_capture::resolve_app_pid(capture_app) {
            Some(pid) => {
                threads.push(super::app_capture::spawn_app_capture(
                    pid,
                    session_start,
                    tx.clone(),
                    stop.clone(),
                )?);
                app_capture_active = true;
            }
            None => {
                log::warn!(
                    "selected capture app '{capture_app}' has no audio session; \
                     falling back to whole-device loopback"
                );
            }
        }
    }
    if !app_capture_active {
        let (handle, app_isolated) = spawn_system_thread(
            system_device.to_string(),
            capture_app.to_string(),
            mac_capture_method.to_string(),
            session_start,
            tx,
            stop.clone(),
        )?;
        threads.push(handle);
        app_capture_active = app_isolated;
    }

    Ok((
        mic_capture,
        CaptureHandle {
            stop,
            threads,
            app_capture_active,
        },
    ))
}

fn find_by_name(mut devices: impl Iterator<Item = cpal::Device>, name: &str) -> Option<cpal::Device> {
    devices.find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

fn find_input_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if name.is_empty() {
        return host.default_input_device();
    }
    find_by_name(host.input_devices().ok()?, name).or_else(|| host.default_input_device())
}

#[cfg(not(target_os = "macos"))]
fn find_output_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if name.is_empty() {
        return host.default_output_device();
    }
    find_by_name(host.output_devices().ok()?, name).or_else(|| host.default_output_device())
}

/// The cpal `Stream` is not `Send`, so each capture runs on its own thread
/// that owns the stream and forwards frames until asked to stop.
fn spawn_mic_thread(
    device_name: String,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    spawn_capture_thread(move || {
        let host = cpal::default_host();
        let device = find_input_device(&host, &device_name)
            .ok_or_else(|| SallyError::Audio("no microphone device available".into()))?;
        let config = device
            .default_input_config()
            .map_err(|e| SallyError::Audio(format!("microphone config: {e}")))?;
        build_stream(
            &device,
            &config,
            AudioSource::Microphone,
            session_start,
            tx,
        )
    }, stop)
}

/// Returns the capture thread and whether a specific app's audio was
/// successfully isolated (vs. falling back to whole-system capture) — the
/// caller uses this to decide whether the "app has no audio session"
/// warning applies.
fn spawn_system_thread(
    device_name: String,
    capture_app: String,
    mac_capture_method: String,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
) -> Result<(std::thread::JoinHandle<()>, bool)> {
    #[cfg(target_os = "macos")]
    {
        spawn_macos_system_thread(device_name, capture_app, mac_capture_method, session_start, tx, stop)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (&capture_app, &mac_capture_method);
        spawn_capture_thread(move || {
            let host = cpal::default_host();
            // WASAPI loopback: open an *input* stream on an output device.
            let device = find_output_device(&host, &device_name)
                .ok_or_else(|| SallyError::Audio("no output device for loopback capture".into()))?;
            let config = device
                .default_output_config()
                .map_err(|e| SallyError::Audio(format!("loopback config: {e}")))?;
            build_stream(&device, &config, AudioSource::System, session_start, tx)
        }, stop)
        .map(|handle| (handle, false))
    }
}

#[cfg(target_os = "macos")]
fn spawn_macos_system_thread(
    device_name: String,
    capture_app: String,
    mac_capture_method: String,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
) -> Result<(std::thread::JoinHandle<()>, bool)> {
    let mut attempts: Vec<String> = Vec::new();
    let want_app = !capture_app.is_empty();

    let want_tap = match mac_capture_method.as_str() {
        "tap" => true,
        "screencapturekit" => false,
        _ => macos_version()
            .map(|(major, minor)| major > 14 || (major == 14 && minor >= 4))
            .unwrap_or(false),
    };

    if want_app && want_tap {
        // Per-app capture through the Core Audio tap: resolves entirely
        // via the HAL (kAudioHardwarePropertyProcessObjectList +
        // kAudioProcessProperty*), so unlike ScreenCaptureKit below, this
        // path never touches the Screen Recording permission. Falls
        // through to SCK if the target process can't be found this way
        // (stale selection, or it isn't visible to Core Audio for some
        // reason) or the tap itself fails to start.
        match super::coreaudio_tap::resolve_process_by_name(&capture_app) {
            Some(process_id) => match super::coreaudio_tap::spawn_tap_capture(
                session_start,
                tx.clone(),
                stop.clone(),
                Some(process_id),
            ) {
                Ok(handle) => return Ok((handle, true)),
                Err(e) => {
                    log::warn!(
                        "Core Audio per-app tap unavailable, trying ScreenCaptureKit: {e}"
                    );
                    attempts.push(format!("Core Audio tap (per-app): {e}"));
                }
            },
            None => {
                log::warn!(
                    "'{capture_app}' not found via Core Audio process list, \
                     trying ScreenCaptureKit"
                );
            }
        }
    } else if !want_app && want_tap {
        match super::coreaudio_tap::spawn_tap_capture(session_start, tx.clone(), stop.clone(), None)
        {
            Ok(handle) => return Ok((handle, false)),
            Err(e) => {
                log::warn!("Core Audio process tap unavailable, trying ScreenCaptureKit: {e}");
                attempts.push(format!("Core Audio tap: {e}"));
            }
        }
    }

    match super::sck_capture::spawn_sck_capture(
        session_start,
        tx.clone(),
        stop.clone(),
        capture_app,
    ) {
        Ok(handle) => return Ok((handle, want_app)),
        Err(e) => {
            log::warn!("ScreenCaptureKit unavailable, falling back to loopback device: {e}");
            attempts.push(format!("ScreenCaptureKit: {e}"));
        }
    }

    return spawn_capture_thread(
        move || {
            let host = cpal::default_host();
            // System audio arrives through a loopback input device
            // (BlackHole or similar) that the user routes meeting audio
            // into. Selected by name; with no selection, look for a
            // BlackHole-style device.
            let device = find_macos_loopback_device(&host, &device_name).ok_or_else(|| {
                SallyError::Audio(format!(
                    "no native system-audio capture worked, and no loopback \
                     input device is selected either:\n{}\nInstall a loopback \
                     driver such as BlackHole, route meeting audio to it (Multi-\
                     Output Device), and select it as the system audio device in \
                     Settings — or grant the permission the errors above are \
                     asking for and restart the meeting.",
                    attempts.join("\n")
                ))
            })?;
            let config = device
                .default_input_config()
                .map_err(|e| SallyError::Audio(format!("loopback config: {e}")))?;
            build_stream(&device, &config, AudioSource::System, session_start, tx)
        },
        stop,
    )
    .map(|handle| (handle, false));
}

/// `(major, minor)` from `sw_vers -productVersion`, best-effort. Used only
/// to decide whether the Core Audio process-tap API (14.4+) is worth
/// attempting before falling back to ScreenCaptureKit.
#[cfg(target_os = "macos")]
fn macos_version() -> Option<(u32, u32)> {
    static VERSION: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *VERSION.get_or_init(|| {
        let out = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.trim().split('.');
        let major: u32 = parts.next()?.parse().ok()?;
        let minor: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
        Some((major, minor))
    })
}

/// A named loopback input device, or any device whose name suggests a
/// loopback driver when nothing is selected. Never falls back to the
/// default input: that is the microphone, and capturing it twice would
/// duplicate the user's voice into the "Meeting" lane.
#[cfg(target_os = "macos")]
fn find_macos_loopback_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    let devices: Vec<cpal::Device> = host.input_devices().ok()?.collect();
    if !name.is_empty() {
        return find_by_name(devices.into_iter(), name);
    }
    devices.into_iter().find(|d| {
        d.name()
            .map(|n| {
                let n = n.to_lowercase();
                n.contains("blackhole") || n.contains("loopback") || n.contains("soundflower")
            })
            .unwrap_or(false)
    })
}

fn spawn_capture_thread(
    make_stream: impl FnOnce() -> Result<cpal::Stream> + Send + 'static,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    super::spawn_with_ready_signal("capture", move |ready_tx| {
        let stream = match make_stream() {
            Ok(s) => {
                let _ = ready_tx.send(Ok(()));
                s
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        if let Err(e) = stream.play() {
            log::error!("failed to start audio stream: {e}");
            return;
        }
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        drop(stream);
    })
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    source: AudioSource,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
) -> Result<cpal::Stream> {
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let stream_config: cpal::StreamConfig = config.config();
    let err_fn = move |e| log::error!("audio stream error ({source:?}): {e}");

    let make_frame = move |samples: Vec<f32>| RawFrame {
        source,
        t_ms: session_start.elapsed().as_millis() as u64,
        sample_rate,
        channels,
        samples,
    };

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| {
                // try_send keeps the audio callback non-blocking; the pipeline
                // reports drops through sequence gaps.
                let _ = tx.try_send(make_frame(data.to_vec()));
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                let samples = data.iter().map(|&s| s as f32 / 32768.0).collect();
                let _ = tx.try_send(make_frame(samples));
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _| {
                let samples = data
                    .iter()
                    .map(|&s| (s as f32 - 32768.0) / 32768.0)
                    .collect();
                let _ = tx.try_send(make_frame(samples));
            },
            err_fn,
            None,
        ),
        other => {
            return Err(SallyError::Audio(format!(
                "unsupported sample format: {other:?}"
            )))
        }
    };
    stream.map_err(|e| SallyError::Audio(format!("failed to open {source:?} stream: {e}")))
}
