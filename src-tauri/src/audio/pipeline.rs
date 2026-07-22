//! Audio pipeline: per-source resampling to 16 kHz mono, bounded buffering,
//! and mixing into 50 ms chunks for the Gemini Live client. Keeps a
//! mic-activity flag for `You` labeling. Never writes audio to disk itself
//! (the optional recorder in `recorder.rs` does that downstream).

use super::{
    downmix, f32_to_i16, AudioSource, CubicResampler, MixedChunk, RawFrame, CHUNK_SAMPLES,
    TARGET_SAMPLE_RATE,
};
use std::collections::VecDeque;

/// Cap per-source buffered audio at 10 s of 16 kHz mono. If a consumer
/// stalls, the oldest audio is dropped (bounded memory, design §4.2).
const MAX_BUFFERED_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 10;

/// RMS threshold above which a chunk counts as active speech energy.
/// Kept low: quiet laptop microphones still need to register as "you
/// speaking" for speaker attribution.
const MIC_ACTIVITY_RMS: f32 = 0.008;
const SYSTEM_ACTIVITY_RMS: f32 = 0.008;

struct SourceLane {
    resampler: CubicResampler,
    rate: u32,
    buffer: VecDeque<f32>,
    dropped: bool,
}

impl SourceLane {
    fn new() -> Self {
        Self {
            resampler: CubicResampler::new(TARGET_SAMPLE_RATE, TARGET_SAMPLE_RATE),
            rate: TARGET_SAMPLE_RATE,
            buffer: VecDeque::new(),
            dropped: false,
        }
    }

    fn push(&mut self, frame: &RawFrame) {
        if frame.sample_rate != self.rate {
            self.resampler = CubicResampler::new(frame.sample_rate, TARGET_SAMPLE_RATE);
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

    /// True when both sources can fill a whole chunk, or one source is a
    /// full chunk ahead (the other lane is stalled or absent and gets
    /// silence). Requiring both when both are flowing matters: emitting on
    /// the first full lane used to consume the other lane short by a few
    /// jittered samples and zero-pad the remainder, splicing silence into
    /// the middle of continuous audio many times per second — audible as
    /// choppy, glitchy recordings.
    pub fn chunk_ready(&self) -> bool {
        let mic = self.mic.available();
        let system = self.system.available();
        (mic >= CHUNK_SAMPLES && system >= CHUNK_SAMPLES)
            || mic >= 2 * CHUNK_SAMPLES
            || system >= 2 * CHUNK_SAMPLES
    }

    pub fn next_chunk(&mut self) -> Option<MixedChunk> {
        if !self.chunk_ready() {
            return None;
        }
        let mic = self.mic.take_chunk();
        let system = self.system.take_chunk();
        let mic_rms =
            (mic.iter().map(|s| s * s).sum::<f32>() / mic.len().max(1) as f32).sqrt();
        let system_rms =
            (system.iter().map(|s| s * s).sum::<f32>() / system.len().max(1) as f32).sqrt();

        let mixed: Vec<i16> = mic
            .iter()
            .zip(system.iter())
            .map(|(a, b)| f32_to_i16((a + b).clamp(-1.0, 1.0)))
            .collect();
        let mic_i16: Vec<i16> = mic.iter().map(|&s| f32_to_i16(s)).collect();

        let chunk_ms = (CHUNK_SAMPLES as u64 * 1000) / TARGET_SAMPLE_RATE as u64;
        let t_ms = self.start_t_ms.unwrap_or(0) + self.seq * chunk_ms;
        let chunk = MixedChunk {
            seq: self.seq,
            t_ms,
            mixed,
            mic: mic_i16,
            mic_active: mic_rms > MIC_ACTIVITY_RMS,
            system_active: system_rms > SYSTEM_ACTIVITY_RMS,
            system,
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
        // A lane with no counterpart must be a full chunk ahead before the
        // pipeline gives up on the other lane and pads it.
        p.push(frame(
            AudioSource::System,
            vec![0.5; CHUNK_SAMPLES * 2],
            TARGET_SAMPLE_RATE,
        ));
        let chunk = p.next_chunk().expect("chunk");
        assert_eq!(chunk.mixed.len(), CHUNK_SAMPLES);
        assert_eq!(chunk.mixed[0], f32_to_i16(0.5));
        assert!(!chunk.mic_active);
    }

    #[test]
    fn waits_for_lagging_lane_instead_of_padding() {
        let mut p = Pipeline::new();
        p.push(frame(
            AudioSource::Microphone,
            vec![0.3; CHUNK_SAMPLES],
            TARGET_SAMPLE_RATE,
        ));
        // System lane is 10 samples short (delivery jitter): must wait, not
        // splice silence into the middle of continuous audio.
        p.push(frame(
            AudioSource::System,
            vec![0.5; CHUNK_SAMPLES - 10],
            TARGET_SAMPLE_RATE,
        ));
        assert!(!p.chunk_ready(), "must wait for the lagging lane");
        p.push(frame(AudioSource::System, vec![0.5; 10], TARGET_SAMPLE_RATE));
        let chunk = p.next_chunk().expect("chunk");
        assert!(
            chunk.system.iter().all(|&s| s != 0.0),
            "no silence spliced into the system lane"
        );
    }

    #[test]
    fn mic_activity_detected() {
        let mut p = Pipeline::new();
        p.push(frame(
            AudioSource::Microphone,
            vec![0.3; CHUNK_SAMPLES * 2],
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
            vec![0.1; CHUNK_SAMPLES * 4],
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
