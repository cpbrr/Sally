//! Timeline assembler (design §4.2 item 5, §5 step 6).
//!
//! Aligns original-transcript fragments, translated fragments, the session
//! clock, chunk sequence numbers, and mic activity into stable timeline
//! entries. Entries are provisional while a turn is open and final once the
//! turn completes; finalized entries never change.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Speech,
    Gap,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    pub index: u64,
    pub kind: EntryKind,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: String,
    pub original: String,
    pub translated: String,
}

/// Provisional view of the currently open entry, for live panels.
#[derive(Debug, Clone, Serialize)]
pub struct PartialEntry {
    pub start_ms: u64,
    pub speaker: String,
    pub original: String,
    pub translated: String,
}

/// Two-stage assembly: `open` receives new original fragments; `closing` is
/// an entry whose original text is frozen (speaker changed) but whose
/// translation is still streaming in. Translated text lags the original by
/// seconds, so routing it to the newest entry smeared each passage's
/// translation into the following paragraph.
pub struct Assembler {
    next_index: u64,
    closing: Option<OpenEntry>,
    open: Option<OpenEntry>,
    /// Fraction of *speech-active* chunks with mic energy needed to
    /// attribute a turn to `You`. Measured against chunks where anyone was
    /// speaking, so long silences no longer dilute the ratio (which used to
    /// merge the user's own speech into remote labels).
    mic_attribution_threshold: f32,
}

struct OpenEntry {
    start_ms: u64,
    last_ms: u64,
    original: String,
    translated: String,
    mic_active_chunks: u32,
    speech_chunks: u32,
}

impl Assembler {
    pub fn new() -> Self {
        Self {
            next_index: 0,
            closing: None,
            open: None,
            mic_attribution_threshold: 0.5,
        }
    }

    fn open_mut(&mut self, t_ms: u64) -> &mut OpenEntry {
        self.open.get_or_insert(OpenEntry {
            start_ms: t_ms,
            last_ms: t_ms,
            original: String::new(),
            translated: String::new(),
            mic_active_chunks: 0,
            speech_chunks: 0,
        })
    }

    pub fn push_original(&mut self, text: &str, t_ms: u64) {
        let e = self.open_mut(t_ms);
        e.original.push_str(text);
        e.last_ms = e.last_ms.max(t_ms);
    }

    /// Translation fragments belong to the oldest unfinished entry: the
    /// model translates a passage seconds after transcribing it.
    pub fn push_translated(&mut self, text: &str, t_ms: u64) {
        if let Some(e) = self.closing.as_mut() {
            e.translated.push_str(text);
            return;
        }
        let e = self.open_mut(t_ms);
        e.translated.push_str(text);
        e.last_ms = e.last_ms.max(t_ms);
    }

    /// Original text accumulated in the currently open entry.
    pub fn open_original_len(&self) -> usize {
        self.open.as_ref().map(|e| e.original.chars().count()).unwrap_or(0)
    }

    /// Start timestamp of the currently open entry, for duration-based
    /// splitting of long uninterrupted turns.
    pub fn open_start_ms(&self) -> Option<u64> {
        self.open.as_ref().map(|e| e.start_ms)
    }

    /// Whether the open entry is (so far) attributed to the microphone.
    /// Speaker-change boundaries from the system lane must not split the
    /// user's own turns.
    pub fn open_mic_dominated(&self) -> bool {
        self.open
            .as_ref()
            .map(|e| {
                e.speech_chunks > 0
                    && e.mic_active_chunks as f32 / e.speech_chunks as f32
                        >= self.mic_attribution_threshold
            })
            .unwrap_or(false)
    }

    /// Rotate the turn: freeze the open entry's original and let its
    /// translation finish streaming. Any previously closing entry is
    /// finalized and returned.
    pub fn rotate_turn(&mut self) -> Option<TimelineEntry> {
        let finished = self.closing.take().and_then(|e| self.seal_entry(e));
        self.closing = self.open.take();
        finished
    }

    /// Speech-activity signal from the pipeline, once per mixed chunk.
    /// Only chunks where someone (mic or system) is speaking count toward
    /// speaker attribution.
    pub fn push_activity(&mut self, mic_active: bool, system_active: bool, _t_ms: u64) {
        if let Some(e) = self.open.as_mut() {
            if mic_active || system_active {
                e.speech_chunks += 1;
            }
            if mic_active {
                e.mic_active_chunks += 1;
            }
        }
    }

    pub fn partial(&self) -> Option<PartialEntry> {
        // Live view: everything not yet sealed. The closing entry's frozen
        // original must stay visible here — it leaves the panel only when
        // its sealed entry arrives, never before (text used to vanish for
        // the whole closing window after a turn rotation).
        let e = self.open.as_ref().or(self.closing.as_ref())?;
        let translated = self
            .closing
            .as_ref()
            .map(|c| c.translated.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| e.translated.clone());
        let mut original = self
            .closing
            .as_ref()
            .map(|c| c.original.clone())
            .unwrap_or_default();
        if let Some(o) = self.open.as_ref() {
            if !original.is_empty() && !o.original.is_empty() {
                original.push(' ');
            }
            original.push_str(&o.original);
        }
        let start_ms = self
            .closing
            .as_ref()
            .map(|c| c.start_ms)
            .unwrap_or(e.start_ms);
        Some(PartialEntry {
            start_ms,
            speaker: String::new(), // provisional: label assigned at finalize
            original,
            translated,
        })
    }

    fn seal_entry(&mut self, e: OpenEntry) -> Option<TimelineEntry> {
        if e.original.trim().is_empty() && e.translated.trim().is_empty() {
            return None;
        }
        let mic_fraction = if e.speech_chunks > 0 {
            e.mic_active_chunks as f32 / e.speech_chunks as f32
        } else {
            0.0
        };
        let speaker = if mic_fraction >= self.mic_attribution_threshold {
            "You".to_string()
        } else {
            "Meeting".to_string()
        };
        let entry = TimelineEntry {
            index: self.next_index,
            kind: EntryKind::Speech,
            start_ms: e.start_ms,
            end_ms: e.last_ms,
            speaker,
            original: e.original.trim().to_string(),
            translated: e.translated.trim().to_string(),
        };
        self.next_index += 1;
        Some(entry)
    }

    /// Finalize everything (turn complete, forced flush, or meeting end):
    /// the closing entry first, then the open one, in timeline order.
    pub fn finalize_turn(&mut self) -> Vec<TimelineEntry> {
        let mut out = Vec::new();
        if let Some(e) = self.closing.take() {
            if let Some(entry) = self.seal_entry(e) {
                out.push(entry);
            }
        }
        if let Some(e) = self.open.take() {
            if let Some(entry) = self.seal_entry(e) {
                out.push(entry);
            }
        }
        out
    }

    /// Explicit gap marker for an unrecoverable transcription interval
    /// (design §11).
    pub fn gap(&mut self, start_ms: u64, end_ms: u64) -> TimelineEntry {
        let entry = TimelineEntry {
            index: self.next_index,
            kind: EntryKind::Gap,
            start_ms,
            end_ms,
            speaker: String::new(),
            original: String::new(),
            translated: String::new(),
        };
        self.next_index += 1;
        entry
    }
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

/// `[mm:ss]` under an hour, `[h:mm:ss]` beyond (4-hour meetings, design §2).
pub fn format_timestamp(ms: u64) -> String {
    let total_s = ms / 1000;
    let (h, m, s) = (total_s / 3600, (total_s % 3600) / 60, total_s % 60);
    if h > 0 {
        format!("[{h}:{m:02}:{s:02}]")
    } else {
        format!("[{m:02}:{s:02}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_assemble_into_entry() {
        let mut a = Assembler::new();
        a.push_original("Hello ", 1000);
        a.push_original("world", 1400);
        a.push_translated("Xin chào ", 1200);
        a.push_translated("thế giới", 1600);
        let e = a.finalize_turn().pop().expect("entry");
        assert_eq!(e.original, "Hello world");
        assert_eq!(e.translated, "Xin chào thế giới");
        assert_eq!(e.start_ms, 1000);
        assert_eq!(e.end_ms, 1600);
        assert_eq!(e.speaker, "Meeting");
    }

    #[test]
    fn mic_dominated_turn_is_you() {
        let mut a = Assembler::new();
        a.push_original("my words", 0);
        for _ in 0..10 {
            a.push_activity(true, false, 0);
        }
        let e = a.finalize_turn().pop().expect("entry");
        assert_eq!(e.speaker, "You");
    }

    #[test]
    fn translation_routes_to_closing_entry_after_split() {
        let mut a = Assembler::new();
        a.push_original("first speaker words", 1000);
        // Speaker changes before the translation arrives.
        let done = a.rotate_turn();
        assert!(done.is_none(), "nothing was closing yet");
        a.push_original("second speaker words", 3000);
        // Late translation belongs to the first (closing) entry.
        a.push_translated("bản dịch của người一", 3200);
        let entries = a.finalize_turn();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].original, "first speaker words");
        assert_eq!(entries[0].translated, "bản dịch của người一");
        assert_eq!(entries[1].original, "second speaker words");
        assert_eq!(entries[1].translated, "");
    }

    #[test]
    fn second_split_finalizes_previous_closing() {
        let mut a = Assembler::new();
        a.push_original("one", 0);
        assert!(a.rotate_turn().is_none());
        a.push_original("two", 1000);
        a.push_translated("một", 1100);
        let sealed = a.rotate_turn().expect("first entry sealed");
        assert_eq!(sealed.original, "one");
        assert_eq!(sealed.translated, "một");
        a.push_original("three", 2000);
        let rest = a.finalize_turn();
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].original, "two");
        assert_eq!(rest[1].original, "three");
        // Indices strictly increasing across all seals.
        assert!(sealed.index < rest[0].index && rest[0].index < rest[1].index);
    }

    #[test]
    fn silence_does_not_dilute_mic_attribution() {
        let mut a = Assembler::new();
        a.push_original("my words", 0);
        // 6 chunks of me speaking, then 30 chunks of dead air: still You.
        for _ in 0..6 {
            a.push_activity(true, false, 0);
        }
        for _ in 0..30 {
            a.push_activity(false, false, 0);
        }
        let e = a.finalize_turn().pop().expect("entry");
        assert_eq!(e.speaker, "You");
    }

    #[test]
    fn remote_dominated_turn_is_not_you() {
        let mut a = Assembler::new();
        a.push_original("their words", 0);
        for _ in 0..2 {
            a.push_activity(true, false, 0);
        }
        for _ in 0..10 {
            a.push_activity(false, true, 0);
        }
        let e = a.finalize_turn().pop().expect("entry");
        assert_eq!(e.speaker, "Meeting");
    }

    #[test]
    fn closing_text_stays_visible_in_partial_after_rotate() {
        let mut a = Assembler::new();
        a.push_original("long frozen passage", 1000);
        assert!(a.rotate_turn().is_none());
        // Nothing sealed yet: the frozen text must still be in the partial.
        let p = a.partial().expect("partial");
        assert_eq!(p.original, "long frozen passage");
        assert_eq!(p.start_ms, 1000);
        // New open text joins it until the closing entry seals.
        a.push_original("new words", 3000);
        let p = a.partial().expect("partial");
        assert_eq!(p.original, "long frozen passage new words");
        assert_eq!(p.start_ms, 1000);
    }

    #[test]
    fn empty_turn_produces_nothing() {
        let mut a = Assembler::new();
        a.push_original("   ", 0);
        assert!(a.finalize_turn().is_empty());
    }

    #[test]
    fn indices_are_stable_and_increasing() {
        let mut a = Assembler::new();
        a.push_original("one", 0);
        let e1 = a.finalize_turn().pop().unwrap();
        let g = a.gap(5000, 9000);
        a.push_original("two", 10_000);
        let e2 = a.finalize_turn().pop().unwrap();
        assert_eq!((e1.index, g.index, e2.index), (0, 1, 2));
        assert_eq!(g.kind, EntryKind::Gap);
    }

    #[test]
    fn timestamp_formats() {
        assert_eq!(format_timestamp(18_000), "[00:18]");
        assert_eq!(format_timestamp(65_000), "[01:05]");
        assert_eq!(format_timestamp(3_723_000), "[1:02:03]");
    }
}
