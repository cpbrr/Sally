//! Meeting store (design §4.2 item 6, §8).
//!
//! Appends finalized timeline entries to the raw Markdown file, keeps a
//! recovery journal for incomplete state, and performs safe finalization,
//! timestamp-free export, renaming, and crash recovery. Never stores audio.

use crate::error::{Result, SallyError};
use crate::timeline::{format_timestamp, EntryKind, TimelineEntry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingMeta {
    pub title: String,
    /// Wall-clock start, used only for metadata and filenames (design §5).
    pub started_at: String,
    pub target_language: String,
}

/// Journal snapshot: everything not yet safely inside the raw Markdown.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecoveryJournal {
    pub meta: Option<MeetingMeta>,
    pub raw_path: Option<PathBuf>,
    /// Provisional entry text not yet finalized.
    pub open_original: String,
    pub open_translated: String,
    pub open_start_ms: u64,
    /// Speaker renames chosen in review but not yet applied.
    pub speaker_renames: BTreeMap<String, String>,
}

pub struct MeetingStore {
    raw_dir: PathBuf,
    polished_dir: PathBuf,
    recovery_dir: PathBuf,
    raw_path: PathBuf,
    /// Date-time filename prefix, `DD-MM-YYYY_HH.MM` (24-hour; `.` instead
    /// of `:` because Windows forbids colons in filenames).
    prefix: String,
    stem: String,
    meta: MeetingMeta,
}

impl MeetingStore {
    /// Create the raw file with its metadata header. Raw and polished files
    /// live in separate folders; filenames are
    /// `DD-MM-YYYY_HH.MM-name.md`.
    pub fn create(
        meetings_dir: PathBuf,
        recovery_dir: PathBuf,
        started: chrono::DateTime<chrono::Local>,
        target_language: &str,
    ) -> Result<Self> {
        let raw_dir = meetings_dir.join("raw");
        let polished_dir = meetings_dir.join("polished");
        std::fs::create_dir_all(&raw_dir)?;
        std::fs::create_dir_all(&polished_dir)?;
        std::fs::create_dir_all(&recovery_dir)?;
        let prefix = started.format("%d-%m-%Y_%H.%M").to_string();
        let mut stem = format!("{prefix}-untitled");
        // Avoid clobbering an existing meeting started the same minute.
        let mut n = 1;
        while raw_dir.join(format!("{stem}.md")).exists() {
            n += 1;
            stem = format!("{prefix}-untitled-{n}");
        }
        let raw_path = raw_dir.join(format!("{stem}.md"));
        let meta = MeetingMeta {
            title: "Untitled meeting".into(),
            started_at: started.format("%d-%m-%Y %H:%M").to_string(),
            target_language: target_language.to_string(),
        };
        let header = render_header(&meta);
        std::fs::write(&raw_path, header)?;
        Ok(Self {
            raw_dir,
            polished_dir,
            recovery_dir,
            raw_path,
            prefix,
            stem,
            meta,
        })
    }

    pub fn raw_path(&self) -> &Path {
        &self.raw_path
    }

    pub fn raw_dir(&self) -> &Path {
        &self.raw_dir
    }

    pub fn polished_dir(&self) -> &Path {
        &self.polished_dir
    }

    pub fn polished_path(&self) -> PathBuf {
        self.polished_dir.join(format!("{}.md", self.stem))
    }

    pub fn export_path(&self) -> PathBuf {
        self.raw_dir.join(format!("{}-no-timestamps.md", self.stem))
    }

    fn journal_path(&self) -> PathBuf {
        self.recovery_dir.join(format!("{}.journal.json", self.stem))
    }

    /// Append one finalized entry. Flushes so a crash loses at most the
    /// journaled provisional state.
    pub fn append_entry(&mut self, entry: &TimelineEntry, target_language: &str) -> Result<()> {
        let block = render_entry(entry, target_language);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.raw_path)?;
        f.write_all(block.as_bytes())?;
        f.flush()?;
        Ok(())
    }

    /// Persist provisional (not yet finalized) state. Called periodically
    /// during the meeting; contains text only, never audio (design §8.2).
    pub fn write_journal(&self, journal: &RecoveryJournal) -> Result<()> {
        let mut j = journal.clone();
        j.meta = Some(self.meta.clone());
        j.raw_path = Some(self.raw_path.clone());
        let tmp = self.journal_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&j).unwrap_or_default())?;
        std::fs::rename(&tmp, self.journal_path())?;
        Ok(())
    }

    /// Successful finalization removes the journal (design §8.2).
    pub fn finalize(&self) -> Result<()> {
        let jp = self.journal_path();
        if jp.exists() {
            std::fs::remove_file(jp)?;
        }
        Ok(())
    }

    /// Apply global speaker renames/merges to the raw file after review.
    /// Rewrites atomically via a temp file.
    pub fn apply_speaker_renames(&self, renames: &BTreeMap<String, String>) -> Result<()> {
        if renames.is_empty() {
            return Ok(());
        }
        let text = std::fs::read_to_string(&self.raw_path)?;
        let updated = rename_speakers_in_markdown(&text, renames);
        let tmp = self.raw_path.with_extension("md.tmp");
        std::fs::write(&tmp, updated)?;
        std::fs::rename(&tmp, &self.raw_path)?;
        Ok(())
    }

    /// Timestamp-free export: separate copy, source untouched (design §2, §8.1).
    pub fn export_without_timestamps(&self) -> Result<PathBuf> {
        let text = std::fs::read_to_string(&self.raw_path)?;
        let out = strip_timestamps(&text);
        let path = self.export_path();
        std::fs::write(&path, out)?;
        Ok(path)
    }

    /// Rename the meeting; renames every associated file together (design §8).
    pub fn rename_meeting(&mut self, new_title: &str) -> Result<()> {
        let safe = sanitize_title(new_title);
        if safe.is_empty() {
            return Err(SallyError::Storage("empty meeting name".into()));
        }
        // Keep the date-time prefix, replace the rest of the stem.
        let new_stem = format!("{}-{safe}", self.prefix);
        if new_stem == self.stem {
            return Ok(());
        }
        let moves = [
            (self.raw_dir.clone(), format!("{}.md", self.stem), format!("{new_stem}.md")),
            (
                self.raw_dir.clone(),
                format!("{}-no-timestamps.md", self.stem),
                format!("{new_stem}-no-timestamps.md"),
            ),
            (
                self.polished_dir.clone(),
                format!("{}.md", self.stem),
                format!("{new_stem}.md"),
            ),
        ];
        for (dir, old_name, new_name) in moves {
            let old = dir.join(old_name);
            if old.exists() {
                std::fs::rename(old, dir.join(new_name))?;
            }
        }
        let old_journal = self.journal_path();
        self.stem = new_stem;
        self.raw_path = self.raw_dir.join(format!("{}.md", self.stem));
        if old_journal.exists() {
            std::fs::rename(old_journal, self.journal_path())?;
        }
        self.meta.title = new_title.to_string();
        Ok(())
    }

    /// Reopen an interrupted meeting from its journal: append any journaled
    /// provisional text to the raw file with a recovery note, then finalize.
    pub fn recover(recovery_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut recovered = Vec::new();
        if !recovery_dir.exists() {
            return Ok(recovered);
        }
        for entry in std::fs::read_dir(recovery_dir)? {
            let path = entry?.path();
            if path.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(journal) = serde_json::from_str::<RecoveryJournal>(&text) else {
                continue;
            };
            let Some(raw_path) = journal.raw_path.clone() else {
                std::fs::remove_file(&path)?;
                continue;
            };
            if raw_path.exists()
                && (!journal.open_original.trim().is_empty()
                    || !journal.open_translated.trim().is_empty())
            {
                let mut f = std::fs::OpenOptions::new().append(true).open(&raw_path)?;
                let ts = format_timestamp(journal.open_start_ms);
                write!(
                    f,
                    "{ts} **Meeting** *(recovered after interruption)*\n\nOriginal: {}\n\nTranslation: {}\n\n",
                    journal.open_original.trim(),
                    journal.open_translated.trim()
                )?;
            }
            std::fs::remove_file(&path)?;
            if raw_path.exists() {
                recovered.push(raw_path);
            }
        }
        Ok(recovered)
    }

    /// Whether any interrupted meeting journals exist (checked at launch).
    pub fn pending_recoveries(recovery_dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(recovery_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .collect()
    }
}

fn render_header(meta: &MeetingMeta) -> String {
    format!(
        "# {}\n\n- Started: {}\n- Target language: {}\n\n---\n\n",
        meta.title, meta.started_at, meta.target_language
    )
}

/// Raw entry format from design §8.1.
pub fn render_entry(entry: &TimelineEntry, target_language: &str) -> String {
    match entry.kind {
        EntryKind::Gap => format!(
            "{} — {} *(transcription unavailable for this interval)*\n\n",
            format_timestamp(entry.start_ms),
            format_timestamp(entry.end_ms)
        ),
        EntryKind::Speech => {
            let mut block = format!(
                "{} **{}**\n\nOriginal: {}\n\n",
                format_timestamp(entry.start_ms),
                entry.speaker,
                entry.original
            );
            if !entry.translated.is_empty() {
                block.push_str(&format!("{target_language}: {}\n\n", entry.translated));
            } else {
                block.push_str("*(translation unavailable)*\n\n");
            }
            block
        }
    }
}

/// Remove leading `[mm:ss]` / `[h:mm:ss]` tokens without touching content.
pub fn strip_timestamps(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix('[') {
                if let Some(close) = rest.find(']') {
                    let ts = &rest[..close];
                    if !ts.is_empty() && ts.chars().all(|c| c.is_ascii_digit() || c == ':') {
                        return rest[close + 1..].trim_start().to_string();
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rename bold speaker labels: `**Old**` becomes `**New**`, applied globally.
pub fn rename_speakers_in_markdown(text: &str, renames: &BTreeMap<String, String>) -> String {
    let mut out = text.to_string();
    for (old, new) in renames {
        if old.trim().is_empty() || new.trim().is_empty() {
            continue;
        }
        out = out.replace(&format!("**{old}**"), &format!("**{}**", new.trim()));
    }
    out
}

fn sanitize_title(title: &str) -> String {
    title
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::TimelineEntry;

    fn tmp_dirs(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("sally-store-{tag}-{}", std::process::id()));
        let m = base.join("meetings");
        let r = base.join(".recovery");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::create_dir_all(&r).unwrap();
        (m, r)
    }

    fn speech(index: u64, start_ms: u64, speaker: &str, orig: &str, tr: &str) -> TimelineEntry {
        TimelineEntry {
            index,
            kind: EntryKind::Speech,
            start_ms,
            end_ms: start_ms + 1000,
            speaker: speaker.into(),
            original: orig.into(),
            translated: tr.into(),
        }
    }

    #[test]
    fn appends_entries_in_design_format() {
        let (m, r) = tmp_dirs("append");
        let mut store =
            MeetingStore::create(m, r, chrono::Local::now(), "Vietnamese").unwrap();
        store
            .append_entry(&speech(0, 18_000, "Speaker 1", "hello", "xin chào"), "Vietnamese")
            .unwrap();
        let text = std::fs::read_to_string(store.raw_path()).unwrap();
        assert!(text.contains("[00:18] **Speaker 1**"));
        assert!(text.contains("Original: hello"));
        assert!(text.contains("Vietnamese: xin chào"));
    }

    #[test]
    fn export_strips_timestamps_but_preserves_raw() {
        let (m, r) = tmp_dirs("export");
        let mut store =
            MeetingStore::create(m, r, chrono::Local::now(), "Vietnamese").unwrap();
        store
            .append_entry(&speech(0, 18_000, "You", "hi", "chào"), "Vietnamese")
            .unwrap();
        let export = store.export_without_timestamps().unwrap();
        let exported = std::fs::read_to_string(&export).unwrap();
        assert!(!exported.contains("[00:18]"));
        assert!(exported.contains("**You**"));
        let raw = std::fs::read_to_string(store.raw_path()).unwrap();
        assert!(raw.contains("[00:18]"), "raw keeps timestamps");
    }

    #[test]
    fn speaker_rename_applies_globally() {
        let mut renames = BTreeMap::new();
        renames.insert("Speaker 1".to_string(), "Tanaka".to_string());
        // Merge: Speaker 2 also becomes Tanaka.
        renames.insert("Speaker 2".to_string(), "Tanaka".to_string());
        let text = "[00:01] **Speaker 1**\n\nx\n\n[00:05] **Speaker 2**\n\ny\n";
        let out = rename_speakers_in_markdown(text, &renames);
        assert!(!out.contains("Speaker 1"));
        assert!(!out.contains("Speaker 2"));
        assert_eq!(out.matches("**Tanaka**").count(), 2);
    }

    #[test]
    fn gap_entries_render_visibly() {
        let e = TimelineEntry {
            index: 0,
            kind: EntryKind::Gap,
            start_ms: 60_000,
            end_ms: 75_000,
            speaker: String::new(),
            original: String::new(),
            translated: String::new(),
        };
        let block = render_entry(&e, "Vietnamese");
        assert!(block.contains("[01:00] — [01:15]"));
        assert!(block.contains("unavailable"));
    }

    #[test]
    fn recovery_appends_journaled_text_and_removes_journal() {
        let (m, r) = tmp_dirs("recover");
        let mut store =
            MeetingStore::create(m, r.clone(), chrono::Local::now(), "Vietnamese").unwrap();
        store
            .append_entry(&speech(0, 1000, "You", "done part", "phần xong"), "Vietnamese")
            .unwrap();
        store
            .write_journal(&RecoveryJournal {
                open_original: "unfinished words".into(),
                open_translated: "lời dang dở".into(),
                open_start_ms: 5000,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(MeetingStore::pending_recoveries(&r).len(), 1);
        let recovered = MeetingStore::recover(&r).unwrap();
        assert_eq!(recovered.len(), 1);
        let text = std::fs::read_to_string(store.raw_path()).unwrap();
        assert!(text.contains("unfinished words"));
        assert!(text.contains("recovered after interruption"));
        assert!(MeetingStore::pending_recoveries(&r).is_empty());
    }

    #[test]
    fn finalize_removes_journal() {
        let (m, r) = tmp_dirs("finalize");
        let store =
            MeetingStore::create(m, r.clone(), chrono::Local::now(), "Vietnamese").unwrap();
        store.write_journal(&RecoveryJournal::default()).unwrap();
        assert_eq!(MeetingStore::pending_recoveries(&r).len(), 1);
        store.finalize().unwrap();
        assert!(MeetingStore::pending_recoveries(&r).is_empty());
    }

    #[test]
    fn filenames_use_day_month_year_and_split_folders() {
        use chrono::TimeZone;
        let (m, r) = tmp_dirs("naming");
        let started = chrono::Local.with_ymd_and_hms(2026, 7, 18, 14, 5, 0).unwrap();
        let store = MeetingStore::create(m, r, started, "Vietnamese").unwrap();
        let name = store
            .raw_path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(name.starts_with("18-07-2026_14.05-untitled"), "{name}");
        assert!(store.raw_path().parent().unwrap().ends_with("raw"));
        assert!(store.polished_path().parent().unwrap().ends_with("polished"));
        assert!(store.export_path().parent().unwrap().ends_with("raw"));
    }

    #[test]
    fn rename_meeting_moves_all_files() {
        let (m, r) = tmp_dirs("rename");
        let mut store =
            MeetingStore::create(m.clone(), r, chrono::Local::now(), "Vietnamese").unwrap();
        std::fs::write(store.polished_path(), "polished").unwrap();
        store.rename_meeting("Weekly Sync: Q3 planning!").unwrap();
        assert!(store.raw_path().exists());
        assert!(store.polished_path().exists());
        let name = store.raw_path().file_name().unwrap().to_string_lossy().to_string();
        assert!(name.contains("Weekly-Sync"), "{name}");
        assert!(!name.contains(':'));
    }
}
