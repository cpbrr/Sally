//! Timeline assembler (design §4.2 item 5, §5 step 6).
//!
//! Aligns original-transcript fragments, translated fragments, the session
//! clock, chunk sequence numbers, and mic activity into stable timeline
//! entries. Entries are provisional while a turn is open and final once the
//! turn completes; finalized entries never change.

use serde::Serialize;

const SENTENCE_END: [char; 8] = ['.', '!', '?', '。', '！', '？', '…', '．'];

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
    /// Session-clock ms when `closing` was created (set once by
    /// `rotate_turn`, never updated afterward). Backs `drain_stale_closing`
    /// — a fixed grace window from creation, not an idle timer: a rotation
    /// that fires before `closing`'s translation has caught up must not
    /// just seal it with the stream cut off mid-passage, but the window
    /// must also expire on a schedule even if translation keeps trickling
    /// in, or every later rotation trigger stays blocked indefinitely.
    closing_since_ms: Option<u64>,
}

struct OpenEntry {
    start_ms: u64,
    last_ms: u64,
    original: String,
    translated: String,
    mic_active_chunks: u32,
    speech_chunks: u32,
}

/// Splits translated text at its own last sentence-ending punctuation,
/// returning `(sealed, carry)`. Translation lags the original, so a
/// rotation timed off the *original* text's sentence boundary lands at an
/// arbitrary point in the translated stream — often a few words into (or
/// short of) its own sentence end. Snapping to translated's own last
/// terminator and carrying the remainder into the next entry keeps each
/// entry's translation aligned to its own sentence instead of a real-time
/// cutoff. If no terminator is present yet, nothing is carried (same as
/// before) — this can only improve on a period that already arrived.
fn split_translated_carry(text: &str) -> (String, String) {
    let mut cut = None;
    for (i, c) in text.char_indices() {
        if SENTENCE_END.contains(&c) {
            cut = Some(i + c.len_utf8());
        }
    }
    match cut {
        Some(end) if end < text.len() => {
            (text[..end].trim().to_string(), text[end..].trim_start().to_string())
        }
        _ => (text.trim().to_string(), String::new()),
    }
}

impl Assembler {
    pub fn new() -> Self {
        Self {
            next_index: 0,
            closing: None,
            open: None,
            mic_attribution_threshold: 0.5,
            closing_since_ms: None,
        }
    }

    /// Prepends translation-split overflow (see `split_translated_carry`)
    /// into whichever entry `push_translated` would route to right now —
    /// same closing-takes-priority rule, so overshoot words land at the
    /// front of the next entry's translation instead of being lost or
    /// stuck behind the sentence they don't belong to.
    fn prepend_translated_carry(&mut self, carry: String, t_ms: u64) {
        if carry.is_empty() {
            return;
        }
        let target = if let Some(e) = self.closing.as_mut() {
            &mut e.translated
        } else {
            &mut self.open_mut(t_ms).translated
        };
        *target = if target.is_empty() {
            carry
        } else {
            format!("{carry} {target}")
        };
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
            // Deliberately does NOT touch `closing_since_ms` — the drain
            // deadline is fixed at the moment this entry became closing,
            // not reset on activity. Continuous translation trickle (very
            // common; a passage can stream in for many seconds) would
            // otherwise keep pushing the deadline back indefinitely and
            // never let `has_closing()` clear, freezing every other
            // rotation trigger behind it.
            return;
        }
        let e = self.open_mut(t_ms);
        e.translated.push_str(text);
        e.last_ms = e.last_ms.max(t_ms);
    }

    /// Whether a closing entry is still pending (created by `rotate_turn`,
    /// not yet sealed). Client-side rotation triggers (split-line-count,
    /// language-change, long-turn-duration — none of which have any
    /// guarantee translation has caught up) should defer instead of
    /// rotating again while this is true: a second `rotate_turn` call
    /// seals whatever `closing` has right now, even mid-stream.
    pub fn has_closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Original text accumulated in the currently open entry only — unlike
    /// `partial()`, this does not include the closing entry's frozen text.
    /// For decisions that must apply to the newest speech alone (readout
    /// gating), blending in an older, possibly different-language closing
    /// entry would corrupt the language signal.
    pub fn open_original_text(&self) -> &str {
        self.open.as_ref().map(|e| e.original.as_str()).unwrap_or("")
    }

    /// Original text accumulated in the currently open entry.
    pub fn open_original_len(&self) -> usize {
        self.open.as_ref().map(|e| e.original.chars().count()).unwrap_or(0)
    }

    /// Translated text accumulated for whichever entry is currently
    /// receiving translation fragments — mirrors `push_translated`'s own
    /// routing (closing takes priority whenever it exists, else open).
    pub fn active_translated_text(&self) -> &str {
        self.closing
            .as_ref()
            .map(|e| e.translated.as_str())
            .or_else(|| self.open.as_ref().map(|e| e.translated.as_str()))
            .unwrap_or("")
    }

    /// Start timestamp of the currently open entry, for duration-based
    /// splitting of long uninterrupted turns.
    pub fn open_start_ms(&self) -> Option<u64> {
        self.open.as_ref().map(|e| e.start_ms)
    }

    /// Whether the open entry's original text currently ends at a sentence
    /// boundary. Speaker-change rotations wait for this so a previous
    /// speaker's lagging tail words drain into their own entry instead of
    /// leaking into the next speaker's line.
    pub fn open_ends_sentence(&self) -> bool {
        self.open
            .as_ref()
            .and_then(|e| e.original.trim_end().chars().last())
            .map(|c| SENTENCE_END.contains(&c))
            .unwrap_or(false)
    }

    /// Count of sentence-ending punctuation marks in the open entry's
    /// original text so far. Backs `SALLY_SPLIT_LINE_COUNT`: a simpler,
    /// speaker-agnostic alternative to speaker-change splitting that just
    /// forces a new line every N sentences.
    pub fn open_sentence_count(&self) -> u32 {
        self.open
            .as_ref()
            .map(|e| e.original.chars().filter(|c| SENTENCE_END.contains(c)).count() as u32)
            .unwrap_or(0)
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
        let old_closing = self.closing.take();
        self.closing = self.open.take();
        self.closing_since_ms = self.closing.as_ref().map(|e| e.last_ms);
        old_closing.and_then(|e| {
            let carry_t_ms = e.last_ms;
            let (sealed_translated, carry) = split_translated_carry(&e.translated);
            self.prepend_translated_carry(carry, carry_t_ms);
            self.seal_entry(OpenEntry {
                translated: sealed_translated,
                ..e
            })
        })
    }

    /// Seal and clear `closing` on its own, without touching `open`, once
    /// `grace_ms` has passed since it was created — a fixed deadline, not
    /// an idle timer, so ongoing translation trickle can't push it back
    /// indefinitely. Callers poll this periodically (e.g. once per audio
    /// chunk) so a `closing` slot a rotation trigger deferred on
    /// (`has_closing()`) still drains on its own instead of only ever
    /// clearing via the next rotation.
    pub fn drain_stale_closing(&mut self, now_ms: u64, grace_ms: u64) -> Option<TimelineEntry> {
        let stale = self
            .closing_since_ms
            .map(|since| now_ms.saturating_sub(since) >= grace_ms)
            .unwrap_or(false);
        if !stale {
            return None;
        }
        self.closing_since_ms = None;
        self.closing.take().and_then(|e| {
            let carry_t_ms = e.last_ms;
            let (sealed_translated, carry) = split_translated_carry(&e.translated);
            self.prepend_translated_carry(carry, carry_t_ms);
            self.seal_entry(OpenEntry {
                translated: sealed_translated,
                ..e
            })
        })
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
        self.closing_since_ms = None;
        if let Some(e) = self.closing.take() {
            let carry_t_ms = e.last_ms;
            let (sealed_translated, carry) = split_translated_carry(&e.translated);
            self.prepend_translated_carry(carry, carry_t_ms);
            if let Some(entry) = self.seal_entry(OpenEntry {
                translated: sealed_translated,
                ..e
            }) {
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
    fn open_original_text_excludes_closing() {
        let mut a = Assembler::new();
        a.push_original("frozen closing text", 1000);
        assert!(a.rotate_turn().is_none());
        assert_eq!(a.open_original_text(), "");
        a.push_original("new open text", 3000);
        assert_eq!(a.open_original_text(), "new open text");
    }

    #[test]
    fn active_translated_text_follows_push_translated_routing() {
        let mut a = Assembler::new();
        // No closing yet: translation lands in (and reads from) open.
        a.push_original("first speaker words", 1000);
        a.push_translated("partial one", 1100);
        assert_eq!(a.active_translated_text(), "partial one");
        // Speaker changes: closing now exists, so routing (and this
        // accessor) both prefer it, even before it has any text yet.
        assert!(a.rotate_turn().is_none());
        assert_eq!(a.active_translated_text(), "partial one");
        a.push_translated(" continues", 1200);
        assert_eq!(a.active_translated_text(), "partial one continues");
    }

    #[test]
    fn stale_closing_drains_without_touching_open() {
        let mut a = Assembler::new();
        a.push_original("first speaker words", 1000);
        assert!(a.rotate_turn().is_none());
        a.push_original("second speaker words", 1500);
        assert!(a.has_closing());
        // Not stale yet: translation could still be streaming in.
        assert!(a.drain_stale_closing(2_000, 2_500).is_none());
        assert!(a.has_closing());
        a.push_translated("bản dịch", 2_200);
        // Now enough time has passed since the entry became closing.
        let sealed = a.drain_stale_closing(5_000, 2_500).expect("stale seal");
        assert_eq!(sealed.original, "first speaker words");
        assert_eq!(sealed.translated, "bản dịch");
        assert!(!a.has_closing());
        // The open entry (second speaker) must be untouched throughout.
        assert_eq!(a.open_original_text(), "second speaker words");
    }

    #[test]
    fn stale_closing_deadline_is_not_reset_by_translation_activity() {
        // Regression test: v1.1.1 reset the drain deadline on every
        // `push_translated` into `closing`, so a passage that kept
        // streaming translation for many seconds (common — translation
        // genuinely takes a while) could keep the deadline pushed back
        // forever, permanently blocking every later rotation trigger
        // gated on `!has_closing()`. The deadline must be fixed at
        // creation time, not extended by ongoing activity.
        let mut a = Assembler::new();
        a.push_original("first speaker words", 1000);
        assert!(a.rotate_turn().is_none());
        a.push_original("second speaker words", 1500);
        // Translation keeps trickling in well past where the old
        // (buggy) activity-reset behavior would have kept pushing the
        // deadline back — this must not stop it from going stale.
        a.push_translated("một ", 2_000);
        a.push_translated("hai ", 3_000);
        a.push_translated("ba", 3_900);
        let sealed = a.drain_stale_closing(4_000, 2_500).expect("stale seal");
        assert_eq!(sealed.original, "first speaker words");
        assert_eq!(sealed.translated, "một hai ba");
        assert!(!a.has_closing());
    }

    #[test]
    fn rotate_after_stale_drain_does_not_reseal_empty_closing() {
        let mut a = Assembler::new();
        a.push_original("one", 0);
        assert!(a.rotate_turn().is_none());
        assert!(a.drain_stale_closing(3_000, 2_500).is_some());
        assert!(!a.has_closing());
        a.push_original("two", 3_500);
        // No stale closing left to (re-)seal — only "two" moves to closing.
        assert!(a.rotate_turn().is_none());
        assert!(a.has_closing());
    }

    #[test]
    fn translated_overshoot_carries_to_next_entry() {
        // Regression test: translation lags the original, so a rotation
        // timed off the original's sentence boundary can capture a few
        // words of the *next* sentence's translation too. Those words
        // must move to the following entry instead of sticking to this
        // one's tail.
        let mut a = Assembler::new();
        a.push_original("first speaker words", 1000);
        assert!(a.rotate_turn().is_none());
        a.push_original("second speaker words", 3000);
        // Translation overshoots: includes the start of the next
        // sentence before the rotation catches up.
        a.push_translated("First sentence done. And, first", 3200);
        let sealed = a.rotate_turn().expect("first entry sealed");
        assert_eq!(sealed.translated, "First sentence done.");
        // Overshoot words now sit at the front of the new closing entry.
        a.push_translated(" I wanted to ask you something.", 3400);
        let entries = a.finalize_turn();
        assert_eq!(entries[0].original, "second speaker words");
        assert_eq!(entries[0].translated, "And, first I wanted to ask you something.");
    }

    #[test]
    fn translated_without_punctuation_is_not_split() {
        // No sentence terminator yet: keep current (pre-fix) behavior —
        // take everything, carry nothing, since there's no better cut
        // point available.
        let mut a = Assembler::new();
        a.push_original("one", 0);
        assert!(a.rotate_turn().is_none());
        a.push_original("two", 1000);
        a.push_translated("một hai ba", 1100);
        let sealed = a.rotate_turn().expect("sealed");
        assert_eq!(sealed.translated, "một hai ba");
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
