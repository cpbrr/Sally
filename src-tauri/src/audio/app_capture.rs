//! Windows per-application audio capture (WASAPI process loopback).
//!
//! Capturing a single meeting app instead of the whole output device means
//! Sally's own translated-voice readout is never part of the captured
//! stream — the echo problem disappears at the source. Requires Windows 10
//! 2004+.

#![cfg(windows)]

use super::{AudioSource, RawFrame};
use crate::error::{Result, SallyError};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

const CAPTURE_RATE: u32 = 48_000;
const CAPTURE_CHANNELS: u16 = 2;

#[derive(Debug, Clone, Serialize)]
pub struct AudioApp {
    pub pid: u32,
    pub name: String,
}

fn process_name(pid: u32) -> Option<String> {
    use windows::Win32::System::ProcessStatus::K32GetProcessImageFileNameW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let len = K32GetProcessImageFileNameW(handle, &mut buf);
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if len == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let name = path.rsplit(['\\', '/']).next()?.to_string();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }
}

/// Applications that currently have an audio session on the default output
/// device (wasapi-rs exposes no session enumeration, so this goes through
/// COM directly). Call from a thread where blocking is acceptable.
pub fn list_audio_apps() -> Vec<AudioApp> {
    let _ = wasapi::initialize_mta();
    let mut apps: Vec<AudioApp> = Vec::new();
    let pids = unsafe { session_pids() }.unwrap_or_default();
    for pid in pids {
        if pid == 0 {
            continue; // system sounds
        }
        let Some(name) = process_name(pid) else {
            continue;
        };
        if apps.iter().any(|a| a.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        apps.push(AudioApp { pid, name });
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

unsafe fn session_pids() -> windows::core::Result<Vec<u32>> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, AudioSessionStateExpired, IAudioSessionControl2,
        IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
    let sessions = manager.GetSessionEnumerator()?;
    let count = sessions.GetCount()?;
    let mut pids = Vec::new();
    for i in 0..count {
        let Ok(control) = sessions.GetSession(i) else {
            continue;
        };
        if control
            .GetState()
            .map(|s| s == AudioSessionStateExpired)
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        if let Ok(pid) = control2.GetProcessId() {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Resolve an executable name (chosen in Settings) to a live audio-session
/// PID. Names are stable across runs; PIDs are not.
pub fn resolve_app_pid(exe_name: &str) -> Option<u32> {
    list_audio_apps()
        .into_iter()
        .find(|a| a.name.eq_ignore_ascii_case(exe_name))
        .map(|a| a.pid)
}

/// Capture one process tree's audio into `tx` as `AudioSource::System`
/// frames. Runs until `stop` is set.
pub fn spawn_app_capture(
    pid: u32,
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    super::spawn_with_ready_signal("per-app capture", move |ready_tx| {
        let _ = wasapi::initialize_mta();
        let result = (|| -> std::result::Result<
            (wasapi::AudioClient, wasapi::Handle, wasapi::AudioCaptureClient),
            wasapi::WasapiError,
        > {
            let mut client = wasapi::AudioClient::new_application_loopback_client(pid, true)?;
            let format = wasapi::WaveFormat::new(
                32,
                32,
                &wasapi::SampleType::Float,
                CAPTURE_RATE as usize,
                CAPTURE_CHANNELS as usize,
                None,
            );
            let mode = wasapi::StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 200_000, // 20 ms
            };
            client.initialize_client(&format, &wasapi::Direction::Capture, &mode)?;
            let event = client.set_get_eventhandle()?;
            let capture = client.get_audiocaptureclient()?;
            client.start_stream()?;
            Ok((client, event, capture))
        })();

        let (client, event, capture) = match result {
            Ok(v) => {
                let _ = ready_tx.send(Ok(()));
                v
            }
            Err(e) => {
                let _ = ready_tx.send(Err(SallyError::Audio(format!(
                    "per-app capture failed to start: {e}"
                ))));
                return;
            }
        };

        let block_align = (CAPTURE_CHANNELS as usize) * 4; // f32 frames
        let mut byte_queue: VecDeque<u8> = VecDeque::new();
        while !stop.load(Ordering::SeqCst) {
            if event.wait_for_event(500).is_err() {
                continue; // timeout: check stop flag and keep waiting
            }
            if capture.read_from_device_to_deque(&mut byte_queue).is_err() {
                log::error!("per-app capture read failed");
                break;
            }
            let whole_frames = byte_queue.len() / block_align;
            if whole_frames == 0 {
                continue;
            }
            let byte_count = whole_frames * block_align;
            let bytes: Vec<u8> = byte_queue.drain(..byte_count).collect();
            let samples: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            let _ = tx.try_send(RawFrame {
                source: AudioSource::System,
                t_ms: session_start.elapsed().as_millis() as u64,
                sample_rate: CAPTURE_RATE,
                channels: CAPTURE_CHANNELS,
                samples,
            });
        }
        let _ = client.stop_stream();
    })
}
