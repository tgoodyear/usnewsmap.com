//! The study's text handling (`qe_lib.py`: `norm_text`, `fold`, `words`,
//! `lm_text`), with Python's `re` character classes: a word character is a
//! letter or number (`str.isalnum`) or `_`, a digit is a decimal digit (Nd),
//! and white space is `str.isspace`.
use unicode_general_category::{get_general_category, GeneralCategory as G};
use unicode_normalization::UnicodeNormalization as _;

/// `[^\W\d_]`: a letter, a letter number or another number.
pub(crate) fn is_letter(c: char) -> bool {
    matches!(
        get_general_category(c),
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
            | G::LetterNumber
            | G::OtherNumber
    )
}

/// `\d`.
fn is_digit(c: char) -> bool {
    get_general_category(c) == G::DecimalNumber
}

/// `\w`.
fn is_word(c: char) -> bool {
    c == '_' || is_letter(c) || is_digit(c)
}

/// `\s` and `str.isspace`: Unicode white space and the four separators
/// U+001C..U+001F, which Python counts and Rust doesn't.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// NFC, carriage returns dropped and line-end hyphenation joined: `(\w)[-¬⸗]\s*\n\s*(\w)`
/// replaced by the two word characters, left to right without overlap.
pub fn normalize(text: &str) -> String {
    let cs: Vec<char> = text.chars().filter(|&c| c != '\r').nfc().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < cs.len() {
        if is_word(cs[i]) && matches!(cs.get(i + 1), Some('-' | '¬' | '\u{2e17}')) {
            let mut j = i + 2;
            let mut newline = false;
            while j < cs.len() && is_space(cs[j]) {
                newline |= cs[j] == '\n';
                j += 1;
            }
            if newline && j < cs.len() && is_word(cs[j]) {
                out.push(cs[i]);
                out.push(cs[j]);
                i = j + 1;
                continue;
            }
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

/// The words of a normalized text: letter runs joined by an inner
/// apostrophe (`[^\W\d_]+(?:['’][^\W\d_]+)*`), as written.
pub fn words(normalized: &str) -> Vec<&str> {
    let cs: Vec<(usize, char)> = normalized.char_indices().collect();
    let end_of = |k: usize| cs.get(k).map_or(normalized.len(), |&(b, _)| b);
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !is_letter(cs[i].1) {
            i += 1;
            continue;
        }
        let start = cs[i].0;
        let mut j = i;
        while j < cs.len() && is_letter(cs[j].1) {
            j += 1;
        }
        while j + 1 < cs.len() && matches!(cs[j].1, '\'' | '’') && is_letter(cs[j + 1].1) {
            j += 1;
            while j < cs.len() && is_letter(cs[j].1) {
                j += 1;
            }
        }
        out.push(&normalized[start..end_of(j)]);
        i = j;
    }
    out
}

/// Serbian Cyrillic to Latin, with the pre-Vuk and Russian-style letters
/// seen in Serbian print.
fn cyrillic_to_latin(c: char) -> Option<&'static str> {
    Some(match c {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'ђ' => "đ",
        'е' => "e",
        'ж' => "ž",
        'з' => "z",
        'и' => "i",
        'ј' => "j",
        'к' => "k",
        'л' => "l",
        'љ' => "lj",
        'м' => "m",
        'н' => "n",
        'њ' => "nj",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'ћ' => "ć",
        'у' => "u",
        'ф' => "f",
        'х' => "h",
        'ц' => "c",
        'ч' => "č",
        'џ' => "dž",
        'ш' => "š",
        'ѣ' => "e",
        'і' => "i",
        'ї' => "i",
        'ы' => "i",
        'й' => "j",
        'ъ' | 'ь' => "",
        'щ' => "šć",
        'ю' => "ju",
        'я' => "ja",
        'э' => "e",
        'ё' => "jo",
        _ => return None,
    })
}

/// Pre-1918 Russian letters to the modern ones.
fn russian_modern(c: char) -> char {
    match c {
        'ѣ' => 'е',
        'і' => 'и',
        'ѳ' => 'ф',
        'ѵ' => 'и',
        'Ѣ' => 'Е',
        'І' => 'И',
        'Ѳ' => 'Ф',
        'Ѵ' => 'И',
        c => c,
    }
}

/// The lookup form of a word, or of a whole text, in `lang` (ISO 639-3):
/// long s and ezh folded, lower-cased; Russian pre-1918 letters modernised
/// and a word-final hard sign dropped; Serbian transliterated to Latin;
/// Yiddish points dropped and ligatures split.
pub fn fold(w: &str, lang: &str) -> String {
    let w = w.replace('ſ', "s").replace('ʒ', "z").to_lowercase();
    match lang {
        "rus" => {
            let cs: Vec<char> = w.chars().map(russian_modern).collect();
            cs.iter()
                .enumerate()
                .filter(|&(i, &c)| !(c == 'ъ' && cs.get(i + 1).is_none_or(|&n| !is_word(n))))
                .map(|(_, &c)| c)
                .collect()
        }
        "srp" => {
            let mut out = String::with_capacity(w.len());
            for c in w.chars() {
                match cyrillic_to_latin(c) {
                    Some(s) => out.push_str(s),
                    None => out.push(c),
                }
            }
            out
        }
        "yid" => {
            let mut out = String::with_capacity(w.len());
            for c in w.chars() {
                match c {
                    '\u{591}'..='\u{5c7}' => {}
                    'װ' => out.push_str("וו"),
                    'ױ' => out.push_str("וי"),
                    'ײ' => out.push_str("יי"),
                    c => out.push(c),
                }
            }
            out
        }
        _ => w,
    }
}

/// The text the character model scores: normalized, folded, decimal digits
/// to `0`, white-space runs to one space, trimmed.
pub fn lm_text(text: &str, lang: &str) -> String {
    let folded = fold(&normalize(text), lang);
    let mut out = String::with_capacity(folded.len());
    let mut space = false;
    for c in folded.chars() {
        if is_space(c) {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(if is_digit(c) { '0' } else { c });
    }
    out
}
