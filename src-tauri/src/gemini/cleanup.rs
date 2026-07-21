//! Cleanup and summarization service (design §9).
//!
//! Manual and optional. Reads the finalized raw transcript, cleans it in
//! bounded sections, then runs one consolidation request for the structured
//! meeting summary. The polished file is written by the caller only after
//! every stage succeeds; failures never touch the raw transcript.

use crate::config::redact_key;
use crate::error::{Result, SallyError};
use serde::Deserialize;
use serde_json::{json, Value};

/// Character budget per cleanup section, split at entry boundaries.
pub const SECTION_BUDGET: usize = 12_000;

#[derive(Debug, Deserialize, Default)]
pub struct MeetingSummary {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<ActionItem>,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ActionItem {
    #[serde(default)]
    pub item: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub deadline: String,
}

/// Split raw markdown into sections at timestamped-entry boundaries so no
/// request exceeds the budget (design §9: bounded sections). Entries contain
/// internal blank lines, so blocks are delimited by lines starting with `[`.
pub fn split_sections(raw: &str, budget: usize) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in raw.lines() {
        if line.starts_with('[') && !current.trim().is_empty() {
            blocks.push(std::mem::take(&mut current).trim_end().to_string());
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        blocks.push(current.trim_end().to_string());
    }

    let mut sections = Vec::new();
    let mut section = String::new();
    for block in blocks {
        if !section.is_empty() && section.len() + block.len() + 2 > budget {
            sections.push(std::mem::take(&mut section));
        }
        if !section.is_empty() {
            section.push_str("\n\n");
        }
        section.push_str(&block);
    }
    if !section.trim().is_empty() {
        sections.push(section);
    }
    sections
}

fn cleanup_prompt(include_timestamps: bool, include_original: bool, context: &str) -> String {
    let mut p = format!(
        "You clean up a raw meeting transcript section. Rules:\n\
         - Preserve the meaning of every passage exactly; never invent facts.\n\
         - Remove filler words, false starts, and repeated fragments.\n\
         - Lines labeled **You** are the user and keep that label. Lines \
         labeled **Meeting** are remote participants: work out from the \
         conversation itself who is speaking and replace **Meeting** with a \
         distinct label per person — a real name when the dialogue reveals \
         one, otherwise Speaker 1, Speaker 2, … used consistently for the \
         same voice throughout. When one entry clearly contains two \
         people, split it into separate entries at the handover. The \
         **You**/**Meeting** split itself comes from a very rough local \
         heuristic (which microphone picked up more energy) — it can be \
         wrong, especially near handovers, so weigh the conversation's own \
         content and phrasing at least as heavily as the existing label \
         when deciding who said what; don't treat it as ground truth.\n\
         - Keep the exact Markdown structure of entries: a `[mm:ss]` \
         timestamp, the bold speaker label, the `Original:` line, then a \
         blank line, then the translation line.\n\
         - Mark genuinely unclear passages with [unclear].\n\
         - Keep both the original text and its translation lines.\n\
         - {} timestamps.\n\
         Return only the cleaned Markdown, no commentary.",
        if include_timestamps { "Keep" } else { "Remove" }
    );
    if !context.is_empty() {
        p.push_str(&format!(
            "\n\nThe transcript is processed in sections. The previous \
             section ended like this — reuse the same speaker labels for \
             the same voices:\n{context}"
        ));
    }
    p
}

fn summary_prompt(ui_language: &str) -> String {
    let language_name = match ui_language {
        "vi" => "Vietnamese",
        _ => "English",
    };
    format!(
        "You summarize a cleaned meeting transcript. Respond with JSON only. \
         Do not invent facts; include owners and deadlines only when explicitly \
         stated; mark uncertainty with [unclear]. Write every field's text \
         (summary, decisions, action items, open questions) in {language_name}, \
         regardless of what language the transcript itself is in."
    )
}

pub struct CleanupClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl CleanupClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            model,
        }
    }

    fn redact(&self, m: String) -> String {
        redact_key(&m, &self.api_key)
    }

    async fn generate(&self, body: Value) -> Result<Value> {
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            super::REST_BASE,
            self.model,
            self.api_key
        );
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SallyError::Gemini(self.redact(format!("cleanup request failed: {e}"))))?;
        let status = resp.status();
        let value: Value = resp
            .json()
            .await
            .map_err(|e| SallyError::Gemini(self.redact(format!("cleanup response invalid: {e}"))))?;
        if !status.is_success() {
            let msg = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            // Rate/quota/unavailable errors surface verbatim (redacted) so the
            // UI can show an actionable message (design §11).
            return Err(SallyError::Gemini(
                self.redact(format!("cleanup failed ({status}): {msg}")),
            ));
        }
        Ok(value)
    }

    fn extract_text(value: &Value) -> Result<String> {
        value
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| SallyError::Gemini("cleanup response had no text".into()))
    }

    /// Clean one section. `context` carries the tail of the previous
    /// cleaned section so speaker labels stay consistent across sections.
    pub async fn clean_section(
        &self,
        section: &str,
        include_timestamps: bool,
        include_original: bool,
        context: &str,
    ) -> Result<String> {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": cleanup_prompt(include_timestamps, include_original, context) }] },
            "contents": [{ "role": "user", "parts": [{ "text": section }] }]
        });
        let value = self.generate(body).await?;
        Self::extract_text(&value)
    }

    pub async fn summarize(&self, cleaned: &str, ui_language: &str) -> Result<MeetingSummary> {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": summary_prompt(ui_language) }] },
            "contents": [{ "role": "user", "parts": [{ "text": cleaned }] }],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": {
                        "summary": { "type": "STRING" },
                        "decisions": { "type": "ARRAY", "items": { "type": "STRING" } },
                        "action_items": {
                            "type": "ARRAY",
                            "items": {
                                "type": "OBJECT",
                                "properties": {
                                    "item": { "type": "STRING" },
                                    "owner": { "type": "STRING" },
                                    "deadline": { "type": "STRING" }
                                }
                            }
                        },
                        "open_questions": { "type": "ARRAY", "items": { "type": "STRING" } }
                    },
                    "required": ["summary"]
                }
            }
        });
        let value = self.generate(body).await?;
        let text = Self::extract_text(&value)?;
        serde_json::from_str(&text)
            .map_err(|e| SallyError::Gemini(format!("summary JSON invalid: {e}")))
    }
}

struct Headers {
    meeting_notes: &'static str,
    summary: &'static str,
    key_decisions: &'static str,
    action_items: &'static str,
    open_questions: &'static str,
    cleaned_transcript: &'static str,
    none_recorded: &'static str,
    due: &'static str,
}

const HEADERS_EN: Headers = Headers {
    meeting_notes: "Meeting Notes",
    summary: "Summary",
    key_decisions: "Key Decisions",
    action_items: "Action Items",
    open_questions: "Open Questions",
    cleaned_transcript: "Cleaned Transcript",
    none_recorded: "_None recorded._",
    due: "due",
};

const HEADERS_VI: Headers = Headers {
    meeting_notes: "Ghi chú cuộc họp",
    summary: "Tóm tắt",
    key_decisions: "Quyết định chính",
    action_items: "Việc cần làm",
    open_questions: "Câu hỏi còn bỏ ngỏ",
    cleaned_transcript: "Bản ghi đã dọn",
    none_recorded: "_Không có._",
    due: "hạn",
};

fn headers_for(ui_language: &str) -> &'static Headers {
    match ui_language {
        "vi" => &HEADERS_VI,
        _ => &HEADERS_EN,
    }
}

/// Render the polished Markdown (design §9: summary, decisions, action
/// items, open questions, cleaned transcript). Section headers follow
/// `ui_language`; the model is separately instructed (`summary_prompt`) to
/// write the summary content itself in that language.
pub fn render_polished(
    title: &str,
    summary: &MeetingSummary,
    cleaned: &str,
    ui_language: &str,
) -> String {
    let h = headers_for(ui_language);
    let mut out = format!(
        "# {title} — {}\n\n## {}\n\n{}\n\n",
        h.meeting_notes, h.summary, summary.summary
    );
    render_section(&mut out, h.key_decisions, h.none_recorded, &summary.decisions, |d| {
        format!("- {d}")
    });
    render_section(&mut out, h.action_items, h.none_recorded, &summary.action_items, |a| {
        let mut line = format!("- {}", a.item);
        if !a.owner.is_empty() {
            line.push_str(&format!(" — {}", a.owner));
        }
        if !a.deadline.is_empty() {
            line.push_str(&format!(" ({} {})", h.due, a.deadline));
        }
        line
    });
    render_section(&mut out, h.open_questions, h.none_recorded, &summary.open_questions, |q| {
        format!("- {q}")
    });
    out.push_str(&format!("## {}\n\n", h.cleaned_transcript));
    out.push_str(cleaned);
    out.push('\n');
    out
}

/// One polished-file section: a `## Header`, then either `none_recorded` or
/// one rendered line per item.
fn render_section<T>(
    out: &mut String,
    header: &str,
    none_recorded: &str,
    items: &[T],
    render_item: impl Fn(&T) -> String,
) {
    out.push_str(&format!("## {header}\n\n"));
    if items.is_empty() {
        out.push_str(none_recorded);
        out.push_str("\n\n");
    } else {
        for item in items {
            out.push_str(&render_item(item));
            out.push('\n');
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_respect_budget_and_boundaries() {
        let entry = "[00:01] **You**\n\nOriginal: hello hello hello\n\nVietnamese: xin chào";
        let raw = vec![entry; 50].join("\n\n");
        let sections = split_sections(&raw, 500);
        assert!(sections.len() > 1);
        for s in &sections {
            assert!(s.len() <= 500 + entry.len(), "section too large");
            // Sections start at an entry boundary, not mid-entry.
            assert!(s.starts_with('['), "section starts mid-entry: {}", &s[..20.min(s.len())]);
        }
        let rejoined = sections.join("\n\n");
        assert_eq!(rejoined, raw, "no content lost");
    }

    #[test]
    fn single_small_transcript_is_one_section() {
        let raw = "[00:01] **You**\n\nOriginal: hi\n\nVietnamese: chào";
        assert_eq!(split_sections(raw, SECTION_BUDGET).len(), 1);
    }

    #[test]
    fn polished_renders_all_sections() {
        let summary = MeetingSummary {
            summary: "Short sync.".into(),
            decisions: vec!["Ship Friday".into()],
            action_items: vec![ActionItem {
                item: "Prepare demo".into(),
                owner: "Rey".into(),
                deadline: "next Friday".into(),
            }],
            open_questions: vec![],
        };
        let out = render_polished("Weekly", &summary, "cleaned text", "en");
        assert!(out.contains("## Summary"));
        assert!(out.contains("Ship Friday"));
        assert!(out.contains("Prepare demo — Rey (due next Friday)"));
        assert!(out.contains("_None recorded._"));
        assert!(out.contains("cleaned text"));
    }

    #[test]
    fn summary_json_parses_with_missing_fields() {
        let s: MeetingSummary = serde_json::from_str(r#"{"summary":"x"}"#).unwrap();
        assert_eq!(s.summary, "x");
        assert!(s.decisions.is_empty());
    }
}
