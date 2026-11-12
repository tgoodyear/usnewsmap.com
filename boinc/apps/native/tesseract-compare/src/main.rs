//! Compares what two Tesseract builds wrote for one page, and summarizes timing runs (#1835).
//!
//! `--mode compare` reads the `txt` and `alto` outputs of both builds (and optionally a
//! reference text) and prints one JSON object: whether the text is identical, the character
//! error rate of each build against the reference, and the ALTO differences in classes:
//!
//! - `version`: the `<Description>` section, where the version of Tesseract is written.
//! - `block`: coordinates of `ComposedBlock` and `TextBlock` (a paragraph) elements, by index.
//! - `word`: coordinates of `String` elements, matched by their text.
//! - `confidence`: the `WC` of matched `String` elements, as written, and how many of them
//!   differ in value (5.5.0 writes a confidence of 1 to 9 percent as 0.1 to 0.9).
//! - `other`: everything else: line and space coordinates, element ids, element counts, words
//!   that have no match, and any attribute not named above.
//!
//! `--mode timing` reads a TSV of `profile page version rep user sys paragraphs` rows and prints
//! the median, range and the ratio of the versions per profile and page.
//!
//! Every option is a named flag (clap).

use clap::Parser;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::ExitCode;

#[derive(Debug, Default, Clone)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
}

impl Element {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Default)]
struct Alto {
    description: String,
    blocks: Vec<Element>,
    paragraphs: Vec<Element>,
    lines: Vec<Element>,
    words: Vec<Element>,
    spaces: Vec<Element>,
    others: Vec<Element>,
}

/// Splits `<Name a="b" c="d"/>` into its name and attributes. Tesseract escapes `<`, `>`, `&`
/// and `"` in attribute values, so the first `>` ends a tag.
fn parse_tag(tag: &str) -> Element {
    let tag = tag.trim_end_matches('/').trim();
    let (name, mut rest) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
    let mut attrs = Vec::new();
    loop {
        rest = rest.trim_start();
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let after = after.trim_start();
        let Some(body) = after.strip_prefix('"') else {
            break;
        };
        let Some((value, tail)) = body.split_once('"') else {
            break;
        };
        attrs.push((key.trim().to_owned(), value.to_owned()));
        rest = tail;
    }
    Element {
        name: name.to_owned(),
        attrs,
    }
}

fn parse_alto(xml: &str) -> Alto {
    let mut alto = Alto::default();
    if let (Some(start), Some(end)) = (xml.find("<Description>"), xml.find("</Description>")) {
        alto.description = xml[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    }
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let tag = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let el = parse_tag(tag);
        match el.name.as_str() {
            "ComposedBlock" => alto.blocks.push(el),
            "TextBlock" => alto.paragraphs.push(el),
            "TextLine" => alto.lines.push(el),
            "String" => alto.words.push(el),
            "SP" => alto.spaces.push(el),
            "Illustration" | "GraphicalElement" => alto.others.push(el),
            _ => {}
        }
    }
    alto
}

/// How far ahead `align` looks for the next word both pages have, when it can't use a table.
const RESYNC: usize = 40;

const GEOMETRY: [&str; 4] = ["HPOS", "VPOS", "WIDTH", "HEIGHT"];

#[derive(Default, Debug)]
struct Diff {
    version: u64,
    block_a: u64,
    block_b: u64,
    block_coords: u64,
    block_matched: u64,
    paragraph_coords: u64,
    paragraph_matched: u64,
    word_matched: u64,
    word_coords: u64,
    word_confidence: u64,
    word_confidence_value: u64,
    word_confidence_over_cent: u64,
    word_unmatched_a: u64,
    word_unmatched_b: u64,
    other_lines: u64,
    other_spaces: u64,
    other_ids: u64,
    other_attrs: u64,
    other_counts: u64,
    max_confidence_delta: f64,
}

/// Compares two same-kind elements by index: how many differ in geometry, and in anything else.
fn compare_by_index(a: &[Element], b: &[Element], diff: &mut Diff, kind: &str) -> (u64, u64) {
    let (mut matched, mut geometry) = (0, 0);
    if a.len() != b.len() {
        diff.other_counts += 1;
    }
    for (x, y) in a.iter().zip(b) {
        matched += 1;
        let mut geom = false;
        let mut keys: Vec<&str> = x
            .attrs
            .iter()
            .chain(&y.attrs)
            .map(|(k, _)| k.as_str())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            if x.attr(key) == y.attr(key) {
                continue;
            }
            if GEOMETRY.contains(&key) {
                geom = true;
            } else if key == "ID" {
                diff.other_ids += 1;
            } else {
                diff.other_attrs += 1;
            }
        }
        if geom {
            geometry += 1;
        }
    }
    let _ = kind;
    (matched, geometry)
}

/// Pairs up the words of two pages by their text: the common prefix and suffix, then the longest
/// common subsequence of the middle when it is small enough. Returns index pairs.
fn align(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    pairs.extend((0..prefix).map(|i| (i, i)));
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    if !ma.is_empty()
        && !mb.is_empty()
        && ma.len().saturating_mul(mb.len()) <= 150_000_000
        && ma.len().min(mb.len()) < usize::from(u16::MAX)
    {
        let w = mb.len() + 1;
        let mut t = vec![0u16; (ma.len() + 1) * w];
        for i in (0..ma.len()).rev() {
            for j in (0..mb.len()).rev() {
                t[i * w + j] = if ma[i] == mb[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < ma.len() && j < mb.len() {
            if ma[i] == mb[j] {
                pairs.push((prefix + i, prefix + j));
                i += 1;
                j += 1;
            } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    } else if !ma.is_empty() && !mb.is_empty() {
        // Too big for the table: walk both, and on a mismatch look a short way ahead in either
        // for the next word the other has.
        let (mut i, mut j) = (0, 0);
        while i < ma.len() && j < mb.len() {
            if ma[i] == mb[j] {
                pairs.push((prefix + i, prefix + j));
                i += 1;
                j += 1;
                continue;
            }
            let skip_a = (1..=RESYNC).find(|k| ma.get(i + k) == Some(&mb[j]));
            let skip_b = (1..=RESYNC).find(|k| mb.get(j + k) == Some(&ma[i]));
            match (skip_a, skip_b) {
                (Some(x), Some(y)) if x <= y => i += x,
                (Some(_), Some(y)) | (None, Some(y)) => j += y,
                (Some(x), None) => i += x,
                (None, None) => {
                    i += 1;
                    j += 1;
                }
            }
        }
    }
    pairs.extend((0..suffix).map(|k| (a.len() - suffix + k, b.len() - suffix + k)));
    pairs
}

fn compare_alto(a: &Alto, b: &Alto) -> Diff {
    let mut d = Diff {
        version: u64::from(a.description != b.description),
        ..Diff::default()
    };
    d.block_a = a.blocks.len() as u64;
    d.block_b = b.blocks.len() as u64;
    let (m, g) = compare_by_index(&a.blocks, &b.blocks, &mut d, "block");
    d.block_matched = m;
    d.block_coords = g;
    let (m, g) = compare_by_index(&a.paragraphs, &b.paragraphs, &mut d, "paragraph");
    d.paragraph_matched = m;
    d.paragraph_coords = g;
    let (_, g) = compare_by_index(&a.lines, &b.lines, &mut d, "line");
    d.other_lines = g;
    let (_, g) = compare_by_index(&a.spaces, &b.spaces, &mut d, "space");
    d.other_spaces = g;
    if a.others.len() != b.others.len() {
        d.other_counts += 1;
    }
    let ta: Vec<&str> = a
        .words
        .iter()
        .map(|w| w.attr("CONTENT").unwrap_or(""))
        .collect();
    let tb: Vec<&str> = b
        .words
        .iter()
        .map(|w| w.attr("CONTENT").unwrap_or(""))
        .collect();
    let pairs = align(&ta, &tb);
    d.word_unmatched_a = (ta.len() - pairs.len()) as u64;
    d.word_unmatched_b = (tb.len() - pairs.len()) as u64;
    for (i, j) in pairs {
        let (x, y) = (&a.words[i], &b.words[j]);
        d.word_matched += 1;
        if GEOMETRY.iter().any(|k| x.attr(k) != y.attr(k)) {
            d.word_coords += 1;
        }
        if x.attr("WC") != y.attr("WC") {
            d.word_confidence += 1;
            let wc = |e: &Element| {
                e.attr("WC")
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0)
            };
            let delta = (wc(x) - wc(y)).abs();
            if delta > 1e-9 {
                d.word_confidence_value += 1;
            }
            if delta > 0.0101 {
                d.word_confidence_over_cent += 1;
            }
            d.max_confidence_delta = d.max_confidence_delta.max(delta);
        }
        for (k, v) in &x.attrs {
            if !GEOMETRY.contains(&k.as_str())
                && !["WC", "CONTENT", "ID"].contains(&k.as_str())
                && y.attr(k) != Some(v)
            {
                d.other_attrs += 1;
            }
        }
        if x.attr("ID") != y.attr("ID") {
            d.other_ids += 1;
        }
    }
    d
}

/// Joins a word broken at a line end (`-`, optional spaces, a newline, optional spaces), then
/// collapses whitespace: both texts are compared as one line of words. This is the gold
/// scorer's normalization without its Unicode normalization (NFC), which std doesn't have.
fn flatten(s: &str) -> Vec<char> {
    let chars: Vec<char> = s.chars().collect();
    let mut joined = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '-' {
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j], ' ' | '\t') {
                j += 1;
            }
            if j < chars.len() && chars[j] == '\r' {
                j += 1;
            }
            if j < chars.len() && chars[j] == '\n' {
                j += 1;
                while j < chars.len() && matches!(chars[j], ' ' | '\t') {
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        joined.push(chars[i]);
        i += 1;
    }
    joined
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect()
}

/// Levenshtein distance in two rows.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, x) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(x != y))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn compare(args: &BTreeMap<String, String>) -> Result<(), String> {
    let get = |k: &str| {
        args.get(k)
            .map(String::as_str)
            .ok_or(format!("--{k} is required"))
    };
    let (txt_a, txt_b) = (read(get("txt-a")?)?, read(get("txt-b")?)?);
    let (alto_a, alto_b) = (read(get("alto-a")?)?, read(get("alto-b")?)?);
    let d = compare_alto(&parse_alto(&alto_a), &parse_alto(&alto_b));
    let mut o = format!(
        "{{\"page\":{},\"text_identical\":{},\"alto_identical\":{}",
        json_str(get("page")?),
        txt_a == txt_b,
        alto_a == alto_b
    );
    if let Some(r) = args.get("ref") {
        let reference = flatten(&read(r)?);
        for (key, hyp) in [("a", &txt_a), ("b", &txt_b)] {
            let edits = distance(&reference, &flatten(hyp));
            let _ = write!(
                o,
                ",\"edits_{key}\":{edits},\"ref_chars\":{},\"cer_{key}\":{:.6}",
                reference.len(),
                edits as f64 / reference.len().max(1) as f64
            );
        }
    }
    let _ = write!(
        o,
        ",\"version\":{},\"blocks_a\":{},\"blocks_b\":{},\"block_coords\":{},\"block_matched\":{},\
         \"paragraphs_a\":{},\"paragraphs_b\":{},\"paragraph_coords\":{},\
         \"words_a\":{},\"words_b\":{},\"word_matched\":{},\"word_unmatched_a\":{},\"word_unmatched_b\":{},\
         \"word_coords\":{},\"word_confidence\":{},\"word_confidence_value\":{},\"word_confidence_over_cent\":{},\"max_confidence_delta\":{:.2},\
         \"other_lines\":{},\"other_spaces\":{},\"other_ids\":{},\"other_attrs\":{},\"other_counts\":{}}}",
        d.version,
        d.block_a,
        d.block_b,
        d.block_coords,
        d.block_matched,
        parse_alto(&alto_a).paragraphs.len(),
        parse_alto(&alto_b).paragraphs.len(),
        d.paragraph_coords,
        parse_alto(&alto_a).words.len(),
        parse_alto(&alto_b).words.len(),
        d.word_matched,
        d.word_unmatched_a,
        d.word_unmatched_b,
        d.word_coords,
        d.word_confidence,
        d.word_confidence_value,
        d.word_confidence_over_cent,
        d.max_confidence_delta,
        d.other_lines,
        d.other_spaces,
        d.other_ids,
        d.other_attrs,
        d.other_counts,
    );
    println!("{o}");
    Ok(())
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn timing(args: &BTreeMap<String, String>) -> Result<(), String> {
    let tsv = read(args.get("tsv").ok_or("--tsv is required")?)?;
    // (profile, page) -> version -> user seconds, and the page's paragraph count.
    let mut runs: BTreeMap<(String, String), BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    let mut paragraphs: BTreeMap<(String, String), String> = BTreeMap::new();
    for line in tsv
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            return Err(format!("short row: {line}"));
        }
        let user: f64 = f[4].parse().map_err(|_| format!("bad user time: {line}"))?;
        let key = (f[0].to_owned(), f[1].to_owned());
        runs.entry(key.clone())
            .or_default()
            .entry(f[2].to_owned())
            .or_default()
            .push(user);
        paragraphs.insert(key, f[6].to_owned());
    }
    println!(
        "profile\tpage\tparagraphs\tversion\truns\tmedian_user_s\tmin\tmax\tratio_vs_first_version"
    );
    for (key, versions) in &mut runs {
        let mut first = None;
        for (ver, xs) in versions.iter_mut() {
            let m = median(xs);
            let ratio = first.map_or(1.0, |f: f64| m / f);
            first.get_or_insert(m);
            println!(
                "{}\t{}\t{}\t{}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.3}",
                key.0,
                key.1,
                paragraphs[key],
                ver,
                xs.len(),
                m,
                xs.iter().cloned().fold(f64::INFINITY, f64::min),
                xs.iter().cloned().fold(0.0, f64::max),
                ratio
            );
        }
    }
    Ok(())
}

/// Every option is a named flag.
#[derive(clap::Parser)]
struct Cli {
    /// `compare` or `timing`.
    #[arg(long)]
    mode: String,
    #[arg(long)]
    page: Option<String>,
    #[arg(long)]
    txt_a: Option<String>,
    #[arg(long)]
    txt_b: Option<String>,
    #[arg(long)]
    alto_a: Option<String>,
    #[arg(long)]
    alto_b: Option<String>,
    #[arg(long = "ref")]
    reference: Option<String>,
    #[arg(long)]
    tsv: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut args = BTreeMap::new();
    args.insert("mode".to_owned(), cli.mode);
    for (key, value) in [
        ("page", cli.page),
        ("txt-a", cli.txt_a),
        ("txt-b", cli.txt_b),
        ("alto-a", cli.alto_a),
        ("alto-b", cli.alto_b),
        ("ref", cli.reference),
        ("tsv", cli.tsv),
    ] {
        if let Some(value) = value {
            args.insert(key.to_owned(), value);
        }
    }
    let result = match args.get("mode").map(String::as_str) {
        Some("compare") => compare(&args),
        Some("timing") => timing(&args),
        _ => Err("--mode compare|timing is required".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = r#"<alto><Description><OCRProcessing><softwareName>tesseract 5.5.0</softwareName></OCRProcessing></Description><Layout><Page><ComposedBlock ID="c0" HPOS="1" VPOS="2" WIDTH="3" HEIGHT="4"><TextBlock ID="b0" HPOS="1" VPOS="2" WIDTH="3" HEIGHT="4"><TextLine ID="l0" HPOS="1" VPOS="2" WIDTH="3" HEIGHT="4"><String ID="s0" HPOS="1" VPOS="2" WIDTH="3" HEIGHT="4" WC="0.50" CONTENT="a"/><SP WIDTH="1" VPOS="2" HPOS="4"/><String ID="s1" HPOS="5" VPOS="2" WIDTH="3" HEIGHT="4" WC="0.60" CONTENT="b"/></TextLine></TextBlock></ComposedBlock></Page></Layout></alto>"#;

    #[test]
    fn identical_pages_have_no_differences() {
        let d = compare_alto(&parse_alto(A), &parse_alto(A));
        assert_eq!(
            (d.version, d.block_coords, d.paragraph_coords, d.word_coords),
            (0, 0, 0, 0)
        );
        assert_eq!(
            (d.word_confidence, d.word_matched, d.word_unmatched_a),
            (0, 2, 0)
        );
    }

    #[test]
    fn each_class_is_counted_on_its_own() {
        let b = A
            .replace("5.5.0", "5.5.4")
            .replace(
                r#"<TextBlock ID="b0" HPOS="1""#,
                r#"<TextBlock ID="b0" HPOS="9""#,
            )
            .replace(
                r#"HPOS="5" VPOS="2" WIDTH="3""#,
                r#"HPOS="6" VPOS="2" WIDTH="3""#,
            )
            .replace(r#"WC="0.50""#, r#"WC="0.51""#);
        let d = compare_alto(&parse_alto(A), &parse_alto(&b));
        assert_eq!((d.version, d.block_coords, d.paragraph_coords), (1, 0, 1));
        assert_eq!((d.word_coords, d.word_confidence), (1, 1));
        assert_eq!(
            d.other_lines + d.other_spaces + d.other_ids + d.other_attrs,
            0
        );
    }

    #[test]
    fn changed_words_are_unmatched() {
        let b = A.replace(r#"CONTENT="b""#, r#"CONTENT="c""#);
        let d = compare_alto(&parse_alto(A), &parse_alto(&b));
        assert_eq!(
            (d.word_matched, d.word_unmatched_a, d.word_unmatched_b),
            (1, 1, 1)
        );
    }

    #[test]
    fn distance_counts_edits() {
        assert_eq!(distance(&flatten("kitten"), &flatten("sitting")), 3);
        assert_eq!(distance(&flatten("a  b"), &flatten("a b")), 0);
        assert_eq!(flatten("Clare- \r\n  mont"), flatten("Claremont"));
        assert_eq!(flatten("Clare-x\nmont"), flatten("Clare-x mont"));
    }
}
