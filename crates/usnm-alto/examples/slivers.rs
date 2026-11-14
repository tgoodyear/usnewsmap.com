//! Measures [`usnm_alto::drop_slivers`] on ALTO files: what it drops, and
//! each line's text before and after, for scoring against references.
//!
//! ```sh
//! cargo run --release -p usnm-alto --example slivers -- \
//!   --files <list of ALTO paths, one a line> --dpi 300 --out <out.jsonl> [--below 0.6] [--min-letter K]
//! ```
//!
//! Writes JSON lines: a `page` row per file (words, words dropped, lines
//! dropped), a `line` row per text line with words (its box in pixels, its
//! text before and after, the filter at `--min-letter`, by default
//! [`MIN_LETTER`]), and a `word` row per word narrower than
//! `--below` line heights a letter (its text, letters, box and ratio), so
//! the threshold can be read off the distribution. A file that doesn't parse
//! gets an `error` row.

use std::io::{BufRead, Write};

use usnm_alto::slivers::{is_sliver_below, letters, line_height, MIN_LETTER};
use usnm_alto::{drop_slivers, parse_with_languages, Line, Page, ParseOptions};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// A line's text as `usnm_alto::text` joins it, without cleaning.
fn line_text(l: &Line, h: f64, drop: Option<f64>) -> String {
    let mut s = l
        .words
        .iter()
        .filter(|w| !drop.is_some_and(|k| is_sliver_below(w.rect.width(), w.text.trim(), h, k)))
        .map(|w| w.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(hy) = &l.hyphen {
        if !s.is_empty() {
            s.push_str(hy);
        }
    }
    s
}

/// Early NDNP files name ALTO 1.x's CCS namespace; read them as v2.
fn normalize(xml: Vec<u8>) -> Vec<u8> {
    let s = String::from_utf8_lossy(&xml);
    if s[..s.len().min(4096)].contains("http://schema.ccs-gmbh.com/ALTO") {
        s.replacen(
            "http://schema.ccs-gmbh.com/ALTO",
            "http://www.loc.gov/standards/alto/ns-v2#",
            1,
        )
        .into_bytes()
    } else {
        xml
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let files = arg(&args, "--files").ok_or("--files is required")?;
    let out = arg(&args, "--out").ok_or("--out is required")?;
    let dpi: f64 = arg(&args, "--dpi").map_or(Ok(300.0), |s| s.parse())?;
    let below: f64 = arg(&args, "--below").map_or(Ok(0.6), |s| s.parse())?;
    let min_letter: f64 = arg(&args, "--min-letter").map_or(Ok(MIN_LETTER), |s| s.parse())?;
    let opts = ParseOptions {
        dpi: Some(dpi),
        ..ParseOptions::default()
    };
    let mut w = std::io::BufWriter::new(std::fs::File::create(out)?);
    let list = std::io::BufReader::new(std::fs::File::open(files)?);
    for path in list.lines() {
        let path = path?;
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let page: Page = match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|x| parse_with_languages(&normalize(x), &opts).map_err(|e| e.to_string()))
        {
            Ok(p) => p,
            Err(e) => {
                writeln!(w, "{}", serde_json::json!({"kind": "error", "file": path, "error": e}))?;
                continue;
            }
        };
        let words = page.counts().2;
        for b in page.blocks.iter().filter(|b| b.kind.is_text()) {
            for l in &b.lines {
                if l.words.is_empty() {
                    continue;
                }
                let h = line_height(l);
                writeln!(
                    w,
                    "{}",
                    serde_json::json!({
                        "kind": "line", "file": path,
                        "box": [l.rect.x0, l.rect.y0, l.rect.x1, l.rect.y1],
                        "before": line_text(l, h, None),
                        "after": line_text(l, h, Some(min_letter)),
                    })
                )?;
                for wd in &l.words {
                    let n = letters(wd.text.trim());
                    if n == 0 || h.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                        continue;
                    }
                    let ratio = wd.rect.width() / (h * n as f64);
                    if ratio < below {
                        writeln!(
                            w,
                            "{}",
                            serde_json::json!({
                                "kind": "word", "file": path, "text": wd.text,
                                "letters": n, "ratio": ratio, "conf": wd.conf,
                                "box": [wd.rect.x0, wd.rect.y0, wd.rect.x1, wd.rect.y1],
                                "line_h": h, "line_box_h": l.rect.height(), "page_w": page.width,
                            })
                        )?;
                    }
                }
            }
        }
        let mut cut = page.clone();
        let s = drop_slivers(&mut cut);
        let below_k: usize = page
            .blocks
            .iter()
            .filter(|b| b.kind.is_text())
            .flat_map(|b| &b.lines)
            .map(|l| {
                let h = line_height(l);
                l.words
                    .iter()
                    .filter(|w| is_sliver_below(w.rect.width(), w.text.trim(), h, min_letter))
                    .count()
            })
            .sum();
        writeln!(
            w,
            "{}",
            serde_json::json!({
                "kind": "page", "file": path, "width": page.width, "height": page.height,
                "granularity": page.granularity.as_str(), "words": words,
                "dropped": s.words, "lines_dropped": s.lines, "dropped_at_min_letter": below_k,
            })
        )?;
    }
    w.flush()?;
    Ok(())
}
