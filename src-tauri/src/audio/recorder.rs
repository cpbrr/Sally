//! Optional meeting audio recorder (Settings → "Save meeting audio").
//!
//! Streams the mixed 16 kHz mono chunks to a WAV file as they leave the
//! pipeline. The file is padded with silence up to each chunk's session-clock
//! timestamp, so a `[mm:ss]` transcript timestamp maps directly to the same
//! position in the recording. RIFF sizes are re-patched every few seconds so
//! a crash still leaves a playable file. Recording stays on the user's disk
//! and is never uploaded.

use crate::error::Result;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

const SAMPLE_RATE: u32 = 16_000;
const SAMPLES_PER_MS: u64 = (SAMPLE_RATE / 1000) as u64;
/// Patch RIFF sizes at most this often (in samples): ~5 s of audio.
const PATCH_EVERY_SAMPLES: u64 = SAMPLE_RATE as u64 * 5;
/// Silence padding is written in bounded blocks (1 s) to keep allocations
/// small even after a long capture gap.
const PAD_BLOCK_SAMPLES: u64 = SAMPLE_RATE as u64;

pub struct WavRecorder {
    file: File,
    samples_written: u64,
    last_patched: u64,
}

impl WavRecorder {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.write_all(&header(0))?;
        Ok(Self {
            file,
            samples_written: 0,
            last_patched: 0,
        })
    }

    /// Append one chunk of 16 kHz mono samples. `t_ms` is the chunk's start
    /// on the session clock; any gap since the last write is filled with
    /// silence so timestamps stay aligned.
    pub fn write(&mut self, t_ms: u64, samples: &[i16]) -> Result<()> {
        let expected = t_ms * SAMPLES_PER_MS;
        while expected > self.samples_written {
            let pad = (expected - self.samples_written).min(PAD_BLOCK_SAMPLES);
            self.file.write_all(&vec![0u8; pad as usize * 2])?;
            self.samples_written += pad;
        }
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.file.write_all(&bytes)?;
        self.samples_written += samples.len() as u64;
        if self.samples_written - self.last_patched >= PATCH_EVERY_SAMPLES {
            self.patch_sizes()?;
        }
        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        self.patch_sizes()?;
        self.file.flush()?;
        Ok(())
    }

    fn patch_sizes(&mut self) -> Result<()> {
        let data = self.samples_written * 2;
        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&((36 + data) as u32).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(40))?;
        self.file.write_all(&(data as u32).to_le_bytes())?;
        self.file.seek(SeekFrom::End(0))?;
        self.last_patched = self.samples_written;
        Ok(())
    }
}

/// 44-byte canonical PCM WAV header: mono, 16-bit, 16 kHz.
fn header(data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_wav(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("sally-rec-{tag}-{}.wav", std::process::id()))
    }

    #[test]
    fn pads_silence_to_timestamp_and_patches_header() {
        let path = tmp_wav("pad");
        let mut rec = WavRecorder::create(&path).unwrap();
        // First chunk starts at 500 ms: 8000 samples of silence expected first.
        rec.write(500, &[1000i16; 800]).unwrap();
        rec.finalize().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        assert_eq!(data_len as usize, (8000 + 800) * 2);
        assert_eq!(bytes.len(), 44 + data_len as usize);
        // The pad region is silence; the payload follows it.
        assert_eq!(bytes[44], 0);
        assert_eq!(
            i16::from_le_bytes(bytes[44 + 8000 * 2..44 + 8000 * 2 + 2].try_into().unwrap()),
            1000
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn contiguous_chunks_do_not_pad() {
        let path = tmp_wav("contig");
        let mut rec = WavRecorder::create(&path).unwrap();
        rec.write(0, &[1i16; 800]).unwrap(); // 0–50 ms
        rec.write(50, &[2i16; 800]).unwrap(); // 50–100 ms
        rec.finalize().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        assert_eq!(data_len as usize, 1600 * 2);
        std::fs::remove_file(&path).ok();
    }
}
