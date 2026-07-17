//! Readout gate: decides, per turn, whether translated audio from Gemini is
//! played aloud. Playback happens only for passages whose detected source
//! language differs from the target language (user requirement: never read
//! out Vietnamese when translating into Vietnamese).
//!
//! Audio for a turn arrives before enough original text exists to classify
//! the source language, so early chunks are buffered until the gate can
//! decide, then either flushed to the player or dropped. Pure logic, no
//! audio devices — unit-testable.

use crate::lang::should_read_out;

/// Original-text length (chars) considered enough to classify a turn.
const DECIDE_AFTER_CHARS: usize = 6;
/// Bound the undecided buffer: 30 s of 24 kHz mono.
const MAX_BUFFERED_SAMPLES: usize = 24_000 * 30;

pub struct ReadoutGate {
    target_code: String,
    /// None = undecided for the current turn.
    decision: Option<bool>,
    buffer: Vec<i16>,
}

impl ReadoutGate {
    pub fn new(target_code: &str) -> Self {
        Self {
            target_code: target_code.to_string(),
            decision: None,
            buffer: Vec::new(),
        }
    }

    /// Feed translated audio for the current turn together with the original
    /// text accumulated so far. Returns samples that should be played now.
    pub fn push_audio(&mut self, samples: Vec<i16>, original_so_far: &str) -> Vec<i16> {
        match self.decision {
            Some(true) => samples,
            Some(false) => Vec::new(),
            None => {
                self.buffer.extend_from_slice(&samples);
                if self.buffer.len() > MAX_BUFFERED_SAMPLES {
                    let excess = self.buffer.len() - MAX_BUFFERED_SAMPLES;
                    self.buffer.drain(..excess);
                }
                if original_so_far.chars().count() >= DECIDE_AFTER_CHARS {
                    self.decide(original_so_far)
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Turn ended. Decide with whatever text exists, return any remaining
    /// playable audio, and reset for the next turn.
    pub fn end_turn(&mut self, original_text: &str) -> Vec<i16> {
        let out = if self.decision.is_none() && !self.buffer.is_empty() {
            self.decide(original_text)
        } else {
            Vec::new()
        };
        self.decision = None;
        self.buffer.clear();
        out
    }

    fn decide(&mut self, original_text: &str) -> Vec<i16> {
        let play = should_read_out(original_text, &self.target_code);
        self.decision = Some(play);
        if play {
            std::mem::take(&mut self.buffer)
        } else {
            self.buffer.clear();
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_until_text_then_flushes_for_foreign_speech() {
        let mut g = ReadoutGate::new("vi");
        // Audio arrives before any text: buffered.
        assert!(g.push_audio(vec![1; 100], "").is_empty());
        assert!(g.push_audio(vec![2; 100], "nex").is_empty());
        // Enough English text: everything buffered comes out at once.
        let out = g.push_audio(vec![3; 100], "next Friday deadline");
        assert_eq!(out.len(), 300);
        // Later chunks in the same turn stream straight through.
        assert_eq!(g.push_audio(vec![4; 50], "next Friday deadline q").len(), 50);
    }

    #[test]
    fn drops_target_language_speech() {
        let mut g = ReadoutGate::new("vi");
        assert!(g.push_audio(vec![1; 100], "").is_empty());
        let out = g.push_audio(vec![2; 100], "hạn chót thứ Sáu");
        assert!(out.is_empty());
        // Subsequent chunks also dropped.
        assert!(g.push_audio(vec![3; 100], "hạn chót thứ Sáu tuần sau").is_empty());
        // Next turn re-decides.
        g.end_turn("hạn chót thứ Sáu tuần sau");
        let out = g.push_audio(vec![4; 100], "ok let's do that then");
        assert_eq!(out.len(), 100);
    }

    #[test]
    fn end_turn_flushes_short_undecided_turns() {
        let mut g = ReadoutGate::new("vi");
        assert!(g.push_audio(vec![1; 80], "yes").is_empty());
        // Short English turn ends before the char threshold: decided at end.
        let out = g.end_turn("yes");
        assert_eq!(out.len(), 80);
    }

    #[test]
    fn end_turn_drops_short_vietnamese_turns() {
        let mut g = ReadoutGate::new("vi");
        assert!(g.push_audio(vec![1; 80], "dạ").is_empty());
        assert!(g.end_turn("dạ").is_empty());
    }

    #[test]
    fn buffer_is_bounded() {
        let mut g = ReadoutGate::new("vi");
        for _ in 0..40 {
            g.push_audio(vec![0; 24_000], ""); // 1 s each, never decidable
        }
        assert!(g.buffer.len() <= MAX_BUFFERED_SAMPLES);
    }

    #[test]
    fn japanese_read_out_when_target_vietnamese() {
        let mut g = ReadoutGate::new("vi");
        let out = g.push_audio(vec![7; 10], "来週の金曜日ですか");
        assert_eq!(out.len(), 10);
    }
}
