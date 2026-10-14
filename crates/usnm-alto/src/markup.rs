//! Markup in engine text (13 §13.4.4): HTML from Surya's blocks, Markdown
//! from most vision-language models. It is stripped once, before the ALTO
//! writer and `normalize_ocr`, so no markup reaches the index.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Markup {
    #[default]
    None,
    Html,
    Markdown,
}

impl Markup {
    pub fn as_str(self) -> &'static str {
        match self {
            Markup::None => "none",
            Markup::Html => "html",
            Markup::Markdown => "markdown",
        }
    }
}

/// `text` without its markup. Line breaks stay, so a block's text still
/// splits into lines.
pub fn strip(text: &str, markup: Markup) -> String {
    match markup {
        Markup::None => text.to_owned(),
        Markup::Html => strip_html(text),
        Markup::Markdown => text
            .split('\n')
            .filter_map(strip_markdown_line)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Tags removed (`<br>` and block ends become line breaks), the common
/// entities decoded. A `<` that opens no tag stays as text.
fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let opens_tag = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        match after.find('>') {
            Some(j) if opens_tag => {
                let name = after[..j]
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                ) {
                    out.push('\n');
                } else if matches!(name.as_str(), "td" | "th") {
                    out.push(' ');
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    decode_entities(&out)
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let end = after.find(';').filter(|&j| j <= 10);
        let decoded = end.and_then(|j| {
            let name = &after[1..j];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((c, j))
        });
        match decoded {
            Some((c, j)) => {
                out.push(c);
                rest = &after[j + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One line of Markdown as plain text; `None` for a line that is only
/// markup (a rule, a table's delimiter row, a code fence).
fn strip_markdown_line(line: &str) -> Option<String> {
    let t = line.trim();
    let only = |c: char| t.len() >= 3 && t.chars().all(|x| x == c || x == ' ');
    if only('-') || only('*') || only('_') || t.starts_with("```") {
        return None;
    }
    if t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
        return None;
    }
    let mut s = t;
    // Block prefixes: headings, quotes, list markers.
    loop {
        let before = s;
        s = s.trim_start_matches('#').trim_start();
        if let Some(r) = s.strip_prefix('>') {
            s = r.trim_start();
        }
        for m in ["- ", "* ", "+ "] {
            if let Some(r) = s.strip_prefix(m) {
                s = r;
            }
        }
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && s[digits..].starts_with(". ") {
            s = &s[digits + 2..];
        }
        if s == before {
            break;
        }
    }
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            // An image: dropped. A link: its text.
            '!' if chars.get(i + 1) == Some(&'[') => {
                if let Some(end) = link_end(&chars, i + 1) {
                    i = end;
                    continue;
                }
                out.push(c);
            }
            '[' => {
                if let Some(end) = link_end(&chars, i) {
                    let close = chars[i..].iter().position(|&x| x == ']').unwrap_or(0) + i;
                    out.extend(&chars[i + 1..close]);
                    i = end;
                    continue;
                }
                out.push(c);
            }
            '*' | '`' | '~' => {}
            // `__bold__`, but not an underscore inside a word.
            '_' => {
                let prev = i.checked_sub(1).map(|p| chars[p]);
                let next = chars.get(i + 1).copied();
                let inside = prev.is_some_and(char::is_alphanumeric)
                    && next.is_some_and(char::is_alphanumeric);
                if inside {
                    out.push(c);
                }
            }
            '|' => out.push(' '),
            '\\' if chars.get(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(out)
}

/// For `[text](url)` starting at `open`, the index after its `)`.
fn link_end(chars: &[char], open: usize) -> Option<usize> {
    let close = chars[open..].iter().position(|&c| c == ']')? + open;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let paren = chars[close + 1..].iter().position(|&c| c == ')')? + close + 1;
    Some(paren + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_html() {
        assert_eq!(
            strip(
                "<p>Die <b>Zeitung</b> &amp; Co.<br/>Seite&#32;2</p>",
                Markup::Html
            ),
            "\nDie Zeitung & Co.\nSeite 2\n"
        );
        assert_eq!(strip("a < b &c", Markup::Html), "a < b &c");
        assert_eq!(strip("<td>1</td><td>2</td>", Markup::Html), " 1  2 ");
    }

    #[test]
    fn strips_markdown() {
        let md = "# The *Evening* Gazette\n\n- **Wheat** rose\n1. [Corn](http://x) fell\n---\n| a | b |\n|---|---|\n> snake_case and __bold__ ![pic](p.png)";
        assert_eq!(
            strip(md, Markup::Markdown),
            "The Evening Gazette\n\nWheat rose\nCorn fell\na b\nsnake_case and bold"
        );
        assert_eq!(strip("*x*", Markup::None), "*x*");
    }
}
