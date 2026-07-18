//! Audio pipeline: per-source resampling to 16 kHz mono, bounded buffering,
//! and mixing into 100 ms chunks for the Gemini Live client. Keeps a
//! system-only copy for diarization and a mic-activity flag for `You`
//! labeling. Never writes audio to disk.

use super::{
    downmix, f32_to_i16, AudioSource, LinearResampler, MixedChunk, RawFrame, CHUNK_SAMPLES,
    TARGET_SAMPLE_RATE,
};
use std::collections::VecDeque;

/// Cap per-source buffered audio at 10 s of 16 kHz mono. If a consumer
/// stalls, the oldest audio is dropped (bounded memory, design §4.2).
const MAX_BUFFERED_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 10;

/// RMS threshold above which a mic chunk counts as active speech energy.
const MIC_ACTIVITY_RMS: f32 = 0.012;

struct SourceLane {
    resampler: LinearResampler,
    rate: u32,
    buffer: VecDeque<f32>,
    dropped: bool,
}

impl SourceLane {
    fn new() -> Self {
        Self {
            resampler: LinearResampler::new(TARGET_SAMPLE_RATE, TARGET_SAMPLE_RATE),
            rate: TARGET_SAMPLE_RATE,
            buffer: VecDeque::new(),
            dropped: false,
        }
    }

    fn push(&mut self, frame: &RawFrame) {
        if frame.sample_rate != self.rate {
            self.resampler = LinearResampler::new(frame.sample_rate, TARGET_SAMPLE_RATE);
            self.rate = frame.sample_rate;
        }
        let mono = downmix(&frame.samples, frame.channels);
        for s in self.resampler.process(&mono) {
            if self.buffer.len() >= MAX_BUFFERED_SAMPLES {
                self.buffer.pop_front();
                self.dropped = true;
            }
            self.buffer.push_back(s);
        }
    }

    /// Take exactly CHUNK_SAMPLES, padding with silence when short.
    fn take_chunk(&mut self) -> Vec<f32> {
        let mut out = Vec::with_capacity(CHUNK_SAMPLES);
        for _ in 0..CHUNK_SAMPLES {
            out.push(self.buffer.pop_front().unwrap_or(0.0));
        }
        out
    }

    fn available(&self) -> usize {
        self.buffer.len()
    }
}

pub struct Pipeline {
    mic: SourceLane,
    system: SourceLane,
    seq: u64,
    start_t_ms: Option<u64>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            mic: SourceLane::new(),
            system: SourceLane::new(),
            seq: 0,
            start_t_ms: None,
        }
    }

    pub fn push(&mut self, frame: RawFrame) {
        if self.start_t_ms.is_none() {
            self.start_t_ms = Some(frame.t_ms);
        }
        match frame.source {
            AudioSource::Microphone => self.mic.push(&frame),
            AudioSource::System => self.system.push(&frame),
        }
    }

    /// True when at least one source can fill a whole chunk. The other lane
    /// is padded with silence so a silent mic never stalls system audio.
    pub fn chunk_ready(&self) -> bool {
        self.mic.available() >= CHUNK_SAMPLES || self.system.available() >= CHUNK_SAMPLES
    }

    pub fn next_chunk(&mut self) -> Option<MixedChunk> {
        if !self.chunk_ready() {
            return None;
        }
        let mic = self.mic.take_chunk();
        let system = self.system.take_chunk();
        let mic_rms =
            (mic.iter().map(|s| s * s).sum::<f32>() / mic.len().max(1) as f32).sqrt();

        let mixed: Vec<i16> = mic
            .iter()
            .zip(system.iter())
            .map(|(a, b)| f32_to_i16((a + b).clamp(-1.0, 1.0)))
            .collect();
        let system_i16: Vec<i16> = system.iter().map(|&s| f32_to_i16(s)).collect();
        let mic_i16: Vec<i16> = mic.iter().map(|&s| f32_to_i16(s)).collect();

        let chunk_ms = (CHUNK_SAMPLES as u64 * 1000) / TARGET_SAMPLE_RATE as u64;
        let t_ms = self.start_t_ms.unwrap_or(0) + self.seq * chunk_ms;
        let chunk = MixedChunk {
            seq: self.seq,
            t_ms,
            mixed,
            system: system_i16,
            mic: mic_i16,
            mic_active: mic_rms > MIC_ACTIVITY_RMS,
        };
        self.seq += 1;
        Some(chunk)
    }

    /// Whether any source dropped audio since the last check (buffer overflow).
    pub fn take_drop_flag(&mut self) -> bool {
        let dropped = self.mic.dropped || self.system.dropped;
        self.mic.dropped = false;
        self.system.dropped = false;
        dropped
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(source: AudioSource, samples: Vec<f32>, rate: u32) -> RawFrame {
        RawFrame {
            source,
            t_ms: 0,
            sample_rate: rate,
            channels: 1,
            samples,
        }
    }

    #[test]
    fn mixes_both_sources_and_pads_missing() {
        let mut p = Pipeline::new();
        p.push(frame(
            AudioSource::System,
            vec![0.5; CHUNK_SAMPLES],
            TARGET_SAMPLE_RATE,
        ));
        // No mic audio at all: still produces a chunk padded with silence.
        let chunk = p.next_chunk().expect("chunk");
        assert_eq!(chunk.mixed.len(), CHUNK_SAMPLES);
        assert_eq!(chunk.mixed[0], f32_to_i16(0.5));
        assert!(!chunk.mic_active);
    }

    #[test]
    fn mic_activity_detected() {
        let mut p = Pipeline::new();
        p.push(frame(
            AudioSource::Microphone,
            vec![0.3; CHUNK_SAMPLES],
            TARGET_SAMPLE_RATE,
        ));
        let chunk = p.next_chunk().expect("chunk");
        assert!(chunk.mic_active);
    }

    #[test]
    fn sequence_numbers_increase() {
        let mut p = Pipeline::new();
        p.push(frame(
            AudioSource::System,
            vec![0.1; CHUNK_SAMPLES * 3],
            TARGET_SAMPLE_RATE,
        ));
        assert_eq!(p.next_chunk().unwrap().seq, 0);
        assert_eq!(p.next_chunk().unwrap().seq, 1);
        assert_eq!(p.next_chunk().unwrap().seq, 2);
    }

    #[test]
    fn buffer_is_bounded() {
        let mut p = Pipeline::new();
        for _ in 0..40 {
            p.push(frame(
                AudioSource::System,
                vec![0.1; TARGET_SAMPLE_RATE as usize], // 1 s each
                TARGET_SAMPLE_RATE,
            ));
        }
        assert!(p.system.available() <= MAX_BUFFERED_SAMPLES);
        assert!(p.take_drop_flag());
        assert!(!p.take_drop_flag());
    }

    #[test]
    fn resamples_non_native_rates() {
        let mut p = Pipeline::new();
        // 48 kHz input: 4800 samples = 100 ms = one chunk at 16 kHz.
        p.push(frame(AudioSource::System, vec![0.2; 9600], 48_000));
        let chunk = p.next_chunk().expect("chunk");
        assert_eq!(chunk.mixed.len(), CHUNK_SAMPLES);
    }
}
