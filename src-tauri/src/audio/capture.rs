//! cpal-based capture adapter.
//!
//! Windows: microphone via default/selected input device, system audio via
//! WASAPI loopback (an input stream opened on an output device).
//!
//! macOS: microphone works through cpal; system audio is captured from a
//! loopback *input* device (BlackHole or similar) that the user routes
//! meeting audio into. Without one, a clear error explains the setup.

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
/// `capture_app` selects a single application (executable name) for system
/// audio via process loopback on Windows; empty captures the whole device.
pub fn start_capture(
    mic_device: &str,
    system_device: &str,
    capture_app: &str,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
) -> Result<CaptureHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let mut app_capture_active = false;

    threads.push(spawn_mic_thread(
        mic_device.to_string(),
        session_start,
        tx.clone(),
        stop.clone(),
    )?);

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
    #[cfg(not(windows))]
    let _ = capture_app;

    if !app_capture_active {
        threads.push(spawn_system_thread(
            system_device.to_string(),
            session_start,
            tx,
            stop.clone(),
        )?);
    }

    Ok(CaptureHandle {
        stop,
        threads,
        app_capture_active,
    })
}

fn find_input_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if name.is_empty() {
        return host.default_input_device();
    }
    host.input_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .or_else(|| host.default_input_device())
}

#[cfg(not(target_os = "macos"))]
fn find_output_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    if name.is_empty() {
        return host.default_output_device();
    }
    host.output_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .or_else(|| host.default_output_device())
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

fn spawn_system_thread(
    device_name: String,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    #[cfg(target_os = "macos")]
    return spawn_capture_thread(move || {
        let host = cpal::default_host();
        // System audio arrives through a loopback input device (BlackHole
        // or similar) that the user routes meeting audio into. Selected by
        // name; with no selection, look for a BlackHole-style device.
        let device = find_macos_loopback_device(&host, &device_name).ok_or_else(|| {
            SallyError::Audio(
                "system audio on macOS needs a loopback input device such as \
                 BlackHole: install one, route meeting audio to it (Multi-Output \
                 Device), and select it as the system audio device in Settings"
                    .into(),
            )
        })?;
        let config = device
            .default_input_config()
            .map_err(|e| SallyError::Audio(format!("loopback config: {e}")))?;
        build_stream(&device, &config, AudioSource::System, session_start, tx)
    }, stop);
    #[cfg(not(target_os = "macos"))]
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
}

/// A named loopback input device, or any device whose name suggests a
/// loopback driver when nothing is selected. Never falls back to the
/// default input: that is the microphone, and capturing it twice would
/// duplicate the user's voice into the "Meeting" lane.
#[cfg(target_os = "macos")]
fn find_macos_loopback_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    let devices: Vec<cpal::Device> = host.input_devices().ok()?.collect();
    if !name.is_empty() {
        return devices
            .into_iter()
            .find(|d| d.name().map(|n| n == name).unwrap_or(false));
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
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let handle = std::thread::spawn(move || {
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
    });
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => Err(SallyError::Audio("capture thread died during startup".into())),
    }
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
