//! macOS system-audio capture via ScreenCaptureKit (macOS 13+).
//!
//! Captures the system mix natively — no BlackHole/virtual-device routing.
//! `excludes_current_process_audio` keeps Sally's own translated-voice
//! readout out of the captured stream, the same echo-at-the-source
//! elimination the Windows per-app path provides. Requires the Screen
//! Recording permission (macOS prompts on first use); when SCK cannot
//! start, the caller falls back to the legacy loopback-device path.
//!
//! Also the only macOS path that supports capturing a single app's audio
//! (`spawn_sck_capture`'s `capture_app` param) — the Core Audio tap path
//! only knows how to tap everything, so `capture.rs` routes a per-app
//! selection here even on 14.4+ where the tap would otherwise be tried
//! first.

#![cfg(target_os = "macos")]

use super::{AudioSource, RawFrame};
use crate::error::{Result, SallyError};
use screencapturekit::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

const SAMPLE_RATE: u32 = 48_000;

struct AudioHandler {
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if !matches!(of_type, SCStreamOutputType::Audio) || self.stop.load(Ordering::SeqCst) {
            return;
        }
        let Some(list) = sample.audio_buffer_list() else {
            return;
        };
        // SCK delivers f32 non-interleaved: one AudioBuffer per channel.
        // Average the channels to mono here; the pipeline resamples 48 kHz
        // to its 16 kHz target like any other frame.
        let n = list.num_buffers();
        if n == 0 {
            return;
        }
        let channels: Vec<&[u8]> = (0..n)
            .filter_map(|i| list.get(i).map(|b| b.data()))
            .collect();
        let frames = channels.iter().map(|c| c.len() / 4).min().unwrap_or(0);
        if frames == 0 {
            return;
        }
        let mut samples = Vec::with_capacity(frames);
        for f in 0..frames {
            let mut acc = 0f32;
            for ch in &channels {
                let o = f * 4;
                acc += f32::from_le_bytes([ch[o], ch[o + 1], ch[o + 2], ch[o + 3]]);
            }
            samples.push(acc / channels.len() as f32);
        }
        // try_send keeps the callback non-blocking, like every other
        // capture path.
        let _ = self.tx.try_send(RawFrame {
            source: AudioSource::System,
            t_ms: self.session_start.elapsed().as_millis() as u64,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            samples,
        });
    }
}

/// Applications ScreenCaptureKit can see right now, for the per-app capture
/// picker. Requires Screen Recording permission to populate; empty (not an
/// error) when it isn't granted yet, matching the Windows picker's shape.
///
/// `SCShareableContent::applications()` lists every running process with
/// any UI presence at all — background agents like CoreServicesUIAgent,
/// loginwindow, universalcontrol, coreauthd have no audio of their own and
/// just clutter the picker. Only apps that own at least one on-screen
/// window are kept, which is the closest signal this crate exposes to
/// "an app the user would actually recognize."
pub fn list_audio_apps() -> Vec<String> {
    let Ok(content) = SCShareableContent::get() else {
        return Vec::new();
    };
    let visible_owners: std::collections::HashSet<String> = content
        .windows()
        .into_iter()
        .filter(SCWindow::is_on_screen)
        .filter_map(|w| w.owning_application())
        .map(|a| a.application_name())
        .filter(|n| !n.is_empty())
        .collect();
    let names: std::collections::BTreeSet<String> = content
        .applications()
        .into_iter()
        .map(|a| a.application_name())
        .filter(|n| !n.is_empty() && visible_owners.contains(n))
        .collect();
    names.into_iter().collect()
}

/// Capture system audio into `tx` until `stop` is set. `capture_app`, when
/// non-empty, scopes the filter to that one application's audio (matched by
/// display name against `list_audio_apps`) instead of the whole display.
/// Fails cleanly (for the loopback fallback) when Screen Recording
/// permission is missing or SCK is unavailable.
pub fn spawn_sck_capture(
    session_start: Instant,
    tx: mpsc::Sender<RawFrame>,
    stop: Arc<AtomicBool>,
    capture_app: String,
) -> Result<std::thread::JoinHandle<()>> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let handle = std::thread::spawn(move || {
        let started = (|| -> std::result::Result<SCStream, String> {
            let content = SCShareableContent::get().map_err(|e| {
                format!(
                    "shareable content unavailable (Screen Recording \
                     permission?): {e}"
                )
            })?;
            let display = content
                .displays()
                .into_iter()
                .next()
                .ok_or_else(|| "no display found".to_string())?;
            let filter = if capture_app.is_empty() {
                SCContentFilter::create()
                    .with_display(&display)
                    .with_excluding_windows(&[])
                    .build()
            } else {
                let app = content
                    .applications()
                    .into_iter()
                    .find(|a| a.application_name() == capture_app)
                    .ok_or_else(|| {
                        format!("'{capture_app}' has no active audio session")
                    })?;
                SCContentFilter::create()
                    .with_display(&display)
                    .with_excluding_windows(&[])
                    .with_including_applications(&[&app], &[])
                    .build()
            };
            // Video output is mandatory for an SCStream; keep it as small
            // and slow as the API allows and simply never register a
            // Screen handler.
            let config = SCStreamConfiguration::new()
                .with_width(2)
                .with_height(2)
                .with_captures_audio(true)
                .with_excludes_current_process_audio(true)
                .with_sample_rate(SAMPLE_RATE as i32)
                .with_channel_count(2);
            let mut stream = SCStream::new(&filter, &config);
            stream.add_output_handler(
                AudioHandler {
                    session_start,
                    tx,
                    stop: stop.clone(),
                },
                SCStreamOutputType::Audio,
            );
            stream
                .start_capture()
                .map_err(|e| format!("SCK capture failed to start: {e}"))?;
            Ok(stream)
        })();

        let stream = match started {
            Ok(s) => {
                let _ = ready_tx.send(Ok(()));
                s
            }
            Err(e) => {
                let _ = ready_tx.send(Err(SallyError::Audio(e)));
                return;
            }
        };
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = stream.stop_capture();
    });
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => Err(SallyError::Audio(
            "ScreenCaptureKit thread died during startup".into(),
        )),
    }
}
