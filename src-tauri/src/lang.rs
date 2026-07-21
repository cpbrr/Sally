//! Lightweight script/language heuristics.
//!
//! Sally splits the transcript into a new entry whenever the spoken
//! language changes mid-stream, so a switch from Japanese to Vietnamese
//! mid-meeting doesn't blend two languages into one entry. Gemini Live
//! Translate detects languages automatically but does not label per-turn
//! source language, so classification runs locally on the original
//! transcript text. This is a heuristic: exact for distinct scripts
//! (Japanese, Korean, Chinese, Thai, Vietnamese diacritics), coarse for
//! plain-Latin languages, which all map to `latin`.

/// Map a display language name (Settings dropdown) to a BCP-47 code for
/// `translationConfig.targetLanguageCode`.
pub fn bcp47(display_name: &str) -> &'static str {
    match display_name.to_ascii_lowercase().as_str() {
        "vietnamese" => "vi",
        "english" => "en",
        "japanese" => "ja",
        "korean" => "ko",
        "chinese" => "zh",
        "french" => "fr",
        "german" => "de",
        "spanish" => "es",
        "portuguese" => "pt",
        "thai" => "th",
        "indonesian" => "id",
        "hindi" => "hi",
        _ => "en",
    }
}

/// Vietnamese-specific letters: đ/ơ/ư/ă/â-family plus every tone-marked
/// vowel. Must include the plain grave/acute/tilde tone marks (à á ã, è é,
/// ì í, ò ó õ, ù ú ũ, ý) alongside the circumflex/breve/horn/hook/dot
/// combinations — those plain-tone vowels are common in everyday words
/// (là, có, và, má, nhà, vào) and their earlier omission meant short
/// streaming fragments built only from such words detected as `latin`
/// instead of `vi`, flip-flopping against fragments that happened to use a
/// circumflex/breve/horn vowel and spuriously triggering the language-
/// change-split logic mid-sentence.
const VIETNAMESE_MARKERS: &str = "\
đĐơƠưƯăĂ\
àáảãạằắẳẵặầấẩẫậ\
èéẻẽẹềếểễệ\
ìíỉĩị\
òóỏõọồốổỗộờớởỡợ\
ùúủũụừứửữự\
ỳýỷỹỵ\
ÀÁẢÃẠẰẮẲẴẶẦẤẨẪẬ\
ÈÉẺẼẸỀẾỂỄỆ\
ÌÍỈĨỊ\
ÒÓỎÕỌỒỐỔỖỘỜỚỞỠỢ\
ÙÚỦŨỤỪỨỬỮỰ\
ỲÝỶỸỴ";

/// Detect the dominant script/language of a text fragment.
/// Returns a BCP-47-ish tag, or `latin` when it is Latin script without
/// Vietnamese markers (English, French, Spanish, … are indistinguishable
/// cheaply), or `None` when there are no letters yet.
pub fn detect(text: &str) -> Option<&'static str> {
    let mut kana = 0usize;
    let mut cjk = 0usize;
    let mut hangul = 0usize;
    let mut thai = 0usize;
    let mut devanagari = 0usize;
    let mut latin = 0usize;
    let mut viet = 0usize;
    let mut letters = 0usize;

    for c in text.chars() {
        let u = c as u32;
        match u {
            0x3040..=0x30FF => {
                kana += 1;
                letters += 1;
            }
            0x4E00..=0x9FFF | 0x3400..=0x4DBF => {
                cjk += 1;
                letters += 1;
            }
            0xAC00..=0xD7AF | 0x1100..=0x11FF => {
                hangul += 1;
                letters += 1;
            }
            0x0E00..=0x0E7F => {
                thai += 1;
                letters += 1;
            }
            0x0900..=0x097F => {
                devanagari += 1;
                letters += 1;
            }
            _ if c.is_alphabetic() => {
                latin += 1;
                letters += 1;
                if VIETNAMESE_MARKERS.contains(c) {
                    viet += 1;
                }
            }
            _ => {}
        }
    }

    if letters == 0 {
        return None;
    }
    // Any kana means Japanese even among kanji (distinguishes ja from zh).
    if kana > 0 {
        return Some("ja");
    }
    if hangul * 2 > letters {
        return Some("ko");
    }
    if cjk * 2 > letters {
        return Some("zh");
    }
    if thai * 2 > letters {
        return Some("th");
    }
    if devanagari * 2 > letters {
        return Some("hi");
    }
    if latin > 0 {
        if viet > 0 {
            return Some("vi");
        }
        return Some("latin");
    }
    None
}

/// Minimum repeated-window length (chars) before flagging a loop; shorter
/// windows risk false positives from incidental short repeated words ("no
/// no no").
const MIN_REPEAT_WINDOW: usize = 10;
/// Cap how large a window we search for, so this stays cheap on long turns.
const MAX_REPEAT_WINDOW: usize = 60;

/// Detects a degenerate repetition loop: Gemini's live-translate model
/// occasionally gets stuck re-emitting the same phrase verbatim instead of
/// continuing, most often observed when the target language is Vietnamese
/// (on both the original transcript and the translation). Checks whether
/// the tail of the text is the same substring repeated at least twice in a
/// row, for a range of window sizes.
pub fn has_repeat_loop(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let max_w = (len / 2).min(MAX_REPEAT_WINDOW);
    if max_w < MIN_REPEAT_WINDOW {
        return false;
    }
    for w in MIN_REPEAT_WINDOW..=max_w {
        if chars[len - 2 * w..len - w] == chars[len - w..len] {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_vietnamese() {
        assert_eq!(detect("Chúng ta sẽ họp vào thứ Sáu tới nhé"), Some("vi"));
        assert_eq!(detect("Được rồi, cảm ơn mọi người"), Some("vi"));
    }

    #[test]
    fn detects_vietnamese_common_words_with_plain_tone_marks() {
        // Regression: à/á/ã/è/é/ì/í/ò/ó/õ/ù/ú/ũ/ý were missing from
        // VIETNAMESE_MARKERS, which only covered circumflex/breve/horn/
        // hook/dot combinations. Short streaming fragments built only from
        // everyday words using these plain tone marks fell through to
        // "latin", flip-flopping against fragments using a circumflex/
        // breve/horn vowel and spuriously splitting mid-sentence.
        assert_eq!(detect("là"), Some("vi"));
        assert_eq!(detect("có"), Some("vi"));
        assert_eq!(detect("và"), Some("vi"));
        assert_eq!(detect("má"), Some("vi"));
        assert_eq!(detect("nhà"), Some("vi"));
        assert_eq!(detect("vào"), Some("vi"));
        assert_eq!(detect("đi đâu đó"), Some("vi"));
    }

    #[test]
    fn detects_japanese_and_chinese() {
        assert_eq!(detect("来週の金曜日までにお願いします"), Some("ja"));
        assert_eq!(detect("我们下周五开会"), Some("zh"));
    }

    #[test]
    fn detects_korean_thai_hindi() {
        assert_eq!(detect("다음 주 금요일까지 부탁드립니다"), Some("ko"));
        assert_eq!(detect("ประชุมวันศุกร์หน้า"), Some("th"));
        assert_eq!(detect("अगले शुक्रवार तक"), Some("hi"));
    }

    #[test]
    fn plain_latin_is_generic() {
        assert_eq!(detect("Let's meet next Friday about the deadline"), Some("latin"));
        assert_eq!(detect("123 …!?"), None);
    }

    #[test]
    fn bcp47_mapping() {
        assert_eq!(bcp47("Vietnamese"), "vi");
        assert_eq!(bcp47("Japanese"), "ja");
        assert_eq!(bcp47("Unknown Language"), "en");
    }

    #[test]
    fn detects_repeat_loop_in_japanese() {
        // Observed verbatim in a v0.17.0 field report: original transcript
        // stuck re-emitting the same clause when the target was Vietnamese.
        let block = "これない警察庁に遺体が送られる予定です。です。";
        let text = format!("{block}{block}");
        assert!(has_repeat_loop(&text));
    }

    #[test]
    fn detects_repeat_loop_in_vietnamese() {
        // Same field report: the Vietnamese translation looped too.
        let block = "Cái này trước cảnh sát sẽ đưa thi thể đến nơi dự kiến. ";
        let text = format!("{block}{block}");
        assert!(has_repeat_loop(&text));
    }

    #[test]
    fn short_natural_repetition_is_not_a_loop() {
        assert!(!has_repeat_loop("no no no I meant Tuesday"));
        assert!(!has_repeat_loop("thank you thank you so much"));
    }

    #[test]
    fn ordinary_text_is_not_a_loop() {
        assert!(!has_repeat_loop(
            "Let's meet next Friday about the deadline and finalize the budget"
        ));
        assert!(!has_repeat_loop(""));
        assert!(!has_repeat_loop("hello"));
    }
}
