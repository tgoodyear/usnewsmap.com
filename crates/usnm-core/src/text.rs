//! Text normalization shared by ingest (04 §4.5) and the query parser, so the
//! index and queries agree. `tokenize` mirrors the `usnm_text` analyzer
//! (05 §5.5): Unicode word split, lowercase, ASCII folding, drop tokens > 40 chars.

use serde::{Deserialize, Serialize};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Tokens longer than this are OCR garbage and are not indexed.
pub const MAX_TOKEN_CHARS: usize = 40;

/// Pages whose normalized text is shorter than this are `short`.
pub const MIN_PAGE_CHARS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextStatus {
    Ok,
    Short,
    Empty,
}

/// Normalize raw OCR text for storage (04 §4.5). Case is preserved.
pub fn normalize_ocr(raw: &str) -> String {
    let nfc: String = raw
        .nfc()
        .filter_map(|c| match c {
            'ſ' => Some('s'),
            '\n' | '\t' | ' ' => Some(c),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let expanded = expand_ligatures(&nfc);
    let joined = rejoin_hyphenation(&expanded);
    collapse_whitespace(&joined)
}

pub fn text_status(normalized: &str) -> TextStatus {
    let n = normalized.chars().filter(|c| !c.is_whitespace()).count();
    if n == 0 {
        TextStatus::Empty
    } else if normalized.trim().chars().count() < MIN_PAGE_CHARS {
        TextStatus::Short
    } else {
        TextStatus::Ok
    }
}

fn expand_ligatures(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            c => out.push(c),
        }
    }
    out
}

/// `indus-\ntry` → `industry`, only when both sides are alphabetic.
fn rejoin_hyphenation(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '-' && i > 0 && chars[i - 1].is_alphabetic() {
            let mut j = i + 1;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            if j < chars.len() && chars[j] == '\n' {
                let mut k = j + 1;
                while k < chars.len() && chars[k].is_whitespace() {
                    k += 1;
                }
                if k < chars.len() && chars[k].is_alphabetic() {
                    i = k;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Collapse runs of spaces/tabs; keep single line breaks as paragraph markers.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    let mut pending_newline = false;
    for c in s.chars() {
        if c == '\n' {
            pending_newline = true;
            pending_space = false;
        } else if c.is_whitespace() {
            if !pending_newline {
                pending_space = true;
            }
        } else {
            if !out.is_empty() {
                if pending_newline {
                    out.push('\n');
                } else if pending_space {
                    out.push(' ');
                }
            }
            pending_space = false;
            pending_newline = false;
            out.push(c);
        }
    }
    out
}

/// Lowercase + ASCII-fold one token (`Ñoño` → `nono`, `ſ` → `s`, `Æsop` → `aesop`).
///
/// Decomposable letters lose their diacritics via NFKD; letters with no
/// decomposition (`æ œ ø ł ß đ ð þ ı`) are transliterated the way Lucene's
/// ASCII folding filter does.
pub fn fold(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for c in token
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        match c {
            'ſ' => out.push('s'),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ø' => out.push('o'),
            'ł' => out.push('l'),
            'ß' => out.push_str("ss"),
            'đ' | 'ð' => out.push('d'),
            'þ' => out.push_str("th"),
            'ı' => out.push('i'),
            'ŋ' => out.push_str("ng"),
            'ħ' => out.push('h'),
            c => out.push(c),
        }
    }
    out
}

/// Split into folded index tokens, exactly as the `usnm_text` analyzer does.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(fold)
        .filter(|t| t.chars().count() <= MAX_TOKEN_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ocr_text() {
        let raw = "The indus-\ntry of the ſtate\u{0007} is  great.\n\n\nNew  para ﬁne";
        assert_eq!(
            normalize_ocr(raw),
            "The industry of the state is great.\nNew para fine"
        );
    }

    #[test]
    fn keeps_hyphens_that_are_not_line_breaks() {
        assert_eq!(
            normalize_ocr("well-known 1896-\n97"),
            "well-known 1896-\n97"
        );
    }

    #[test]
    fn classifies_text_status() {
        assert_eq!(text_status(""), TextStatus::Empty);
        assert_eq!(text_status("  \n "), TextStatus::Empty);
        assert_eq!(text_status("tiny page"), TextStatus::Short);
        assert_eq!(
            text_status("a reasonably long page of newsprint"),
            TextStatus::Ok
        );
    }

    #[test]
    fn tokenizes_like_the_analyzer() {
        assert_eq!(
            tokenize("Crucify mankind upon a CROSS of Gold! Café—ſo"),
            vec!["crucify", "mankind", "upon", "a", "cross", "of", "gold", "cafe", "so"]
        );
        assert_eq!(
            tokenize("Æsop Œuvre Søren Łódź Straße Þór Đakovo"),
            vec!["aesop", "oeuvre", "soren", "lodz", "strasse", "thor", "dakovo"]
        );
        assert!(tokenize("Æsop Œuvre Søren Łódź Straße")
            .iter()
            .all(|t| t.is_ascii()));
        let long = "x".repeat(41);
        assert!(tokenize(&long).is_empty());
    }
}
