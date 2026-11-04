//! Markup in engine text (13 §13.4.4): HTML from Surya's blocks, Markdown
//! from most vision-language models. It is stripped once, before the ALTO
//! writer and `normalize_ocr`, so no markup reaches the index.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
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
        Markup::Markdown => strip_markdown(text),
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

/// Markdown as plain text, line by line.
///
/// A list marker at the start of a line is kept as text unless the line
/// is clearly an item of a Markdown list (#1488). In a newspaper, a line
/// often starts with a printed number and period or a dash: a date
/// (`31. Dezember früh …`), a price (`- 5 Cents`), a numbered item, a
/// dash that opens a paragraph. Stripping those deletes content, while
/// keeping a real list's marker costs at most a character or two. So:
///
/// - An ordered marker (`1. `) is always kept. A Markdown renderer shows
///   its number, and on a printed page the number is part of the text.
/// - A bullet (`- `, `* `, `+ `) is removed only when its line is one of
///   at least two adjacent lines that start with the same bullet. A line
///   whose item's text, without its markup, starts with a number, or
///   with a currency sign and a number, is never such a line: `- 5 Cents`, `- $1.50` and
///   `- 31. Dezember` are as likely a printed dash before a price or a
///   date. A blank line ends a run, and text stripped one line or one
///   word at a time (a line or word box) keeps its bullets.
fn strip_markdown(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let bullets: Vec<Option<char>> = lines
        .iter()
        .map(|l| block_body(l).and_then(bullet))
        .collect();
    let in_list = |i: usize| {
        bullets[i].is_some_and(|b| {
            (i > 0 && bullets[i - 1] == Some(b)) || bullets.get(i + 1) == Some(&Some(b))
        })
    };
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| strip_markdown_line(l, in_list(i)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line, trimmed, after its heading and quote prefixes; `None` for a
/// line that is only markup (a rule, a table's delimiter row, a code
/// fence). A `#` with no space after it (`#1`) stays.
fn block_body(line: &str) -> Option<&str> {
    let t = line.trim();
    let only = |c: char| t.len() >= 3 && t.chars().all(|x| x == c || x == ' ');
    if only('-') || only('*') || only('_') || t.starts_with("```") {
        return None;
    }
    if t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
        return None;
    }
    let mut s = t;
    loop {
        let before = s;
        let hashes = s.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && s[hashes..].starts_with(' ') {
            s = s[hashes..].trim_start();
        }
        if let Some(r) = s.strip_prefix("> ").or_else(|| (s == ">").then_some("")) {
            s = r.trim_start();
        }
        if s == before {
            return Some(s);
        }
    }
}

/// The bullet `body` starts with, if it may be a list marker: one of
/// `-`, `*`, `+` and a space, before an item whose text, without its
/// markup, starts with neither a number nor a currency sign and a number.
fn bullet(body: &str) -> Option<char> {
    let (b, item) = bullet_item(body)?;
    let text = inline(item);
    let mut t = text.chars();
    let first = t.next()?;
    let currency = matches!(first, '$' | '¢' | '£' | '€' | '¥' | '₤' | 'ℳ');
    let numeric = first.is_numeric() || (currency && t.next().is_some_and(char::is_numeric));
    (!numeric).then_some(b)
}

/// The bullet at the start of `body` and its item, after the item's own
/// heading and quote prefixes.
fn bullet_item(body: &str) -> Option<(char, &str)> {
    let mut c = body.chars();
    let b = c.next().filter(|b| matches!(b, '-' | '*' | '+'))?;
    let item = c.as_str().strip_prefix(' ')?.trim_start();
    Some((b, block_body(item).unwrap_or(item)))
}

/// One line of Markdown as plain text; `None` for a line that is only
/// markup. `list_item` removes the line's bullet ([`strip_markdown`]).
fn strip_markdown_line(line: &str, list_item: bool) -> Option<String> {
    let mut s = block_body(line)?;
    if list_item {
        if let Some((_, item)) = bullet_item(s) {
            s = item;
        }
    }
    Some(inline(s))
}

/// Text without its inline Markdown, spaces collapsed. Only markup is
/// removed: a `*` or `~` that opens or closes nothing (`2 * 3`) and an
/// underscore inside a word stay.
fn inline(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    // Emphasis, strike-through and code spans: a delimiter run that opens
    // (next to text on its right, not on its left) and a later one that
    // closes are removed together.
    for d in ["**", "__", "~~", "*", "_", "`"] {
        chars = strip_pairs(&chars, &d.chars().collect::<Vec<_>>());
    }
    let mut out = String::with_capacity(s.len());
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
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `chars` with each pair of delimiter runs `d` removed, where the first
/// can open (text right after it, and a space, punctuation or the start
/// before it) and the second can close (text right before it, and a
/// space, punctuation or the end after it).
fn strip_pairs(chars: &[char], d: &[char]) -> Vec<char> {
    let n = d.len();
    let at = |i: usize| i + n <= chars.len() && chars[i..i + n] == *d;
    let edge = |c: Option<&char>| {
        c.is_none_or(|c| c.is_whitespace() || (c.is_ascii_punctuation() && !d.contains(c)))
    };
    let text = |c: Option<&char>| c.is_some_and(|c| !c.is_whitespace() && !d.contains(c));
    let opens = |i: usize| {
        at(i) && edge(i.checked_sub(1).and_then(|p| chars.get(p))) && text(chars.get(i + n))
    };
    let closes = |i: usize| at(i) && i > 0 && text(chars.get(i - 1)) && edge(chars.get(i + n));
    let mut drop = vec![false; chars.len()];
    let mut i = 0;
    while i < chars.len() {
        if opens(i) {
            if let Some(j) = (i + n..chars.len()).find(|&j| closes(j)) {
                drop[i..i + n].fill(true);
                drop[j..j + n].fill(true);
                i = j + n;
                continue;
            }
        }
        i += 1;
    }
    chars
        .iter()
        .zip(drop)
        .filter(|(_, d)| !d)
        .map(|(c, _)| *c)
        .collect()
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
        let md = "# The *Evening* Gazette\n\n- **Wheat** rose\n- [Corn](http://x) fell\n---\n| a | b |\n|---|---|\n> snake_case and __bold__ ![pic](p.png)";
        assert_eq!(
            strip(md, Markup::Markdown),
            "The Evening Gazette\n\nWheat rose\nCorn fell\na b\nsnake_case and bold"
        );
        assert_eq!(strip("*x*", Markup::None), "*x*");
        // Literal text stays.
        assert_eq!(
            strip(
                "2 * 3 = 6, ~5 miles, #1 in town, a `tick, x*y",
                Markup::Markdown
            ),
            "2 * 3 = 6, ~5 miles, #1 in town, a `tick, x*y"
        );
        assert_eq!(
            strip("_italic_ and `code` and ~~gone~~", Markup::Markdown),
            "italic and code and gone"
        );
    }

    #[test]
    fn markdown_keeps_line_start_numbers_and_dashes() {
        let md = |s: &str| strip(s, Markup::Markdown);
        // #1488: a date, a price, an ordered item and a lone bullet stay.
        let date = "Cöln, 1. Januar. Die englische Post aus London vom\n31. Dezember früh ist ausgeblieben.";
        assert_eq!(md(date), date);
        assert_eq!(md("- 5 Cents"), "- 5 Cents");
        assert_eq!(md("1. Wheat rose"), "1. Wheat rose");
        assert_eq!(md("- Wheat rose"), "- Wheat rose");
        // Ordered markers stay even in a list: a renderer shows them.
        assert_eq!(md("1. Wheat\n2. Corn"), "1. Wheat\n2. Corn");
        // Prices and dates never count towards a list.
        assert_eq!(md("- 5 Cents\n- $1.50"), "- 5 Cents\n- $1.50");
        assert_eq!(
            md("- 31. Dezember\n- Wheat rose"),
            "- 31. Dezember\n- Wheat rose"
        );
        // Different bullets, or a blank line between, make no list.
        assert_eq!(md("- Wheat\n* Corn"), "- Wheat\n* Corn");
        assert_eq!(md("- Wheat\n\n- Corn"), "- Wheat\n\n- Corn");
        // A dash inside a line is text.
        assert_eq!(
            md("Wien - Ungarn, 5 - 10 Cents"),
            "Wien - Ungarn, 5 - 10 Cents"
        );
    }

    #[test]
    fn markdown_strips_bullets_of_a_list() {
        let md = |s: &str| strip(s, Markup::Markdown);
        assert_eq!(md("- Wheat rose\n- Corn fell"), "Wheat rose\nCorn fell");
        assert_eq!(
            md("Prices:\n* Wheat\n  * Corn\n- 5 Cents"),
            "Prices:\nWheat\nCorn\n- 5 Cents"
        );
        assert_eq!(md("> - Wheat\n> - Corn"), "Wheat\nCorn");
        // The item's own prefixes go too; formatted prices still stay.
        assert_eq!(md("- # Wheat\n- > Corn"), "Wheat\nCorn");
        assert_eq!(md("- **5 Cents**\n- [$1.50](x)"), "- 5 Cents\n- $1.50");
    }
}
