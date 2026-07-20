//! Translated-voice playback. Receives 24 kHz mono i16 PCM from the Gemini
//! Live client (after the readout gate) and plays it on the default output
//! device, resampling to the device rate and fanning mono out to all
//! channels. The stream lives on its own thread because cpal streams are
//! not `Send`. Nothing is ever written to disk.
//!
//! Note: on speakers, the played translation is picked up again by loopback
//! capture and re-enters the pipeline. Setup docs recommend headphones when
//! readout is enabled.

use super::LinearResampler;
use crate::error::{Result, SallyError};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Gemini Live output audio rate.
const SOURCE_RATE: u32 = 24_000;
/// Cap queued playback at 15 s of device-rate audio: readout naturally lags
/// live speech, and a long backlog is worse than dropping stale audio.
const MAX_QUEUED_SECONDS: usize = 15;

/// Speaker-echo grace period: capture keeps hearing the tail of played audio
/// briefly after the queue drains.
const ACTIVE_TAIL: Duration = Duration::from_millis(500);

pub struct Player {
    queue: Arc<Mutex<VecDeque<f32>>>,
    resampler: LinearResampler,
    device_rate: u32,
    stop: Arc<AtomicBool>,
    /// Playback gain (f32 bits), applied in the output callback so it can
    /// change mid-stream without touching queued samples.
    volume: Arc<AtomicU32>,
    thread: Option<std::thread::JoinHandle<()>>,
    last_active: Instant,
}

impl Player {
    /// Open the default output device. Fails cleanly when none exists.
    pub fn new(volume: f32) -> Result<Self> {
        let queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let volume = Arc::new(AtomicU32::new(volume.clamp(0.0, 1.0).to_bits()));

        // Probe the device rate on the caller thread so `new` can fail early
        // and the resampler can be configured before the thread starts.
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| SallyError::Audio("no output device for readout".into()))?;
        let config = device
            .default_output_config()
            .map_err(|e| SallyError::Audio(format!("output config: {e}")))?;
        let device_rate = config.sample_rate().0;

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let q = queue.clone();
        let stop_flag = stop.clone();
        let vol = volume.clone();
        let thread = std::thread::spawn(move || {
            let stream = match build_output_stream(&device, &config, q, vol) {
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
                log::error!("readout stream failed to start: {e}");
                return;
            }
            while !stop_flag.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            drop(stream);
        });
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                return Err(SallyError::Audio("readout thread died during startup".into()))
            }
        }

        Ok(Self {
            queue,
            resampler: LinearResampler::new(SOURCE_RATE, device_rate),
            device_rate,
            stop,
            volume,
            thread: Some(thread),
            last_active: Instant::now() - ACTIVE_TAIL,
        })
    }

    /// Change readout volume (0.0–1.0) mid-stream.
    pub fn set_volume(&self, v: f32) {
        self.volume
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Queue translated 24 kHz mono samples for playback.
    pub fn push(&mut self, samples: &[i16]) {
        let as_f32: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
        let resampled = self.resampler.process(&as_f32);
        let mut q = self.queue.lock().unwrap();
        let cap = self.device_rate as usize * MAX_QUEUED_SECONDS;
        for s in resampled {
            if q.len() >= cap {
                q.pop_front();
            }
            q.push_back(s);
        }
        self.last_active = Instant::now();
    }

    /// True while queued audio is playing (plus a short tail). Used by the
    /// session to feed Gemini microphone-only audio during readout so the
    /// spoken translation is not captured via loopback and translated again.
    pub fn is_active(&mut self) -> bool {
        let queued = { !self.queue.lock().unwrap().is_empty() };
        if queued {
            self.last_active = Instant::now();
            return true;
        }
        self.last_active.elapsed() < ACTIVE_TAIL
    }

    /// Drop any queued audio (e.g. when readout is toggled off mid-turn).
    pub fn clear(&self) {
        self.queue.lock().unwrap().clear();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    queue: Arc<Mutex<VecDeque<f32>>>,
    volume: Arc<AtomicU32>,
) -> Result<cpal::Stream> {
    let channels = config.channels() as usize;
    let stream_config: cpal::StreamConfig = config.config();
    let err_fn = |e| log::error!("readout stream error: {e}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let vol = volume.clone();
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    let g = f32::from_bits(vol.load(Ordering::Relaxed));
                    let mut q = queue.lock().unwrap();
                    for frame in data.chunks_mut(channels) {
                        let s = q.pop_front().unwrap_or(0.0) * g;
                        for out in frame.iter_mut() {
                            *out = s;
                        }
                    }
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let vol = volume.clone();
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i16], _| {
                    let g = f32::from_bits(vol.load(Ordering::Relaxed));
                    let mut q = queue.lock().unwrap();
                    for frame in data.chunks_mut(channels) {
                        let s = q.pop_front().unwrap_or(0.0) * g;
                        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        for out in frame.iter_mut() {
                            *out = v;
                        }
                    }
                },
                err_fn,
                None,
            )
        }
        other => {
            return Err(SallyError::Audio(format!(
                "unsupported output sample format: {other:?}"
            )))
        }
    };
    stream.map_err(|e| SallyError::Audio(format!("failed to open readout stream: {e}")))
}
