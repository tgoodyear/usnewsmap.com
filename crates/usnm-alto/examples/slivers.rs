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
//! [`MIN_LETTER`]), and a `word` row per word narrower than `--below` line
//! heights a letter (its text, letters, box and ratio), so the threshold can
//! be read off the distribution. A file that doesn't parse gets an `error`
//! row. The text after is the worker's: [`kept`] decides it, and a page
//! without word boxes is left as it was.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Parser;
use usnm_alto::slivers::{kept, letters, line_height, MIN_LETTER};
use usnm_alto::{drop_slivers, parse_with_languages, Granularity, Line, Page, ParseOptions};

#[derive(Parser)]
#[command(about = "Measures the sliver filter on ALTO files")]
struct Args {
    /// A file listing ALTO paths, one a line.
    #[arg(long)]
    files: PathBuf,
    /// Where the JSON lines go.
    #[arg(long)]
    out: PathBuf,
    /// The dpi for `inch1200` and `mm10` ALTO.
    #[arg(long, default_value_t = 300.0)]
    dpi: f64,
    /// Word rows are written for words narrower than this, in line heights a letter.
    #[arg(long, default_value_t = 0.6)]
    below: f64,
    /// The threshold the text after is filtered at.
    #[arg(long, default_value_t = MIN_LETTER)]
    min_letter: f64,
}

/// A line's text as `usnm_alto::text` joins it, without cleaning: the words
/// `keep` marks, and the hyphen if it says so.
fn line_text(l: &Line, keep: &(Vec<bool>, bool)) -> String {
    let mut s = l
        .words
        .iter()
        .zip(&keep.0)
        .filter(|(_, k)| **k)
        .map(|(w, _)| w.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(hy) = &l.hyphen {
        if !s.is_empty() && keep.1 {
            s.push_str(hy);
        }
    }
    s
}

/// Early NDNP files name ALTO 1.x's CCS namespace; read them as v2.
fn normalize(xml: Vec<u8>) -> Vec<u8> {
    const CCS: &[u8] = b"http://schema.ccs-gmbh.com/ALTO";
    const V2: &[u8] = b"http://www.loc.gov/standards/alto/ns-v2#";
    let head = &xml[..xml.len().min(4096)];
    match head.windows(CCS.len()).position(|w| w == CCS) {
        Some(i) => [&xml[..i], V2, &xml[i + CCS.len()..]].concat(),
        None => xml,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = Args::parse();
    let opts = ParseOptions {
        dpi: Some(a.dpi),
        ..ParseOptions::default()
    };
    let list = std::fs::read_to_string(&a.files)?;
    let paths: Vec<&str> = list
        .lines()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    // `--out` is created (and truncated) before anything is read, so it
    // mustn't be the list or one of the files it names, by any alias.
    if let Ok(out) = std::fs::canonicalize(&a.out) {
        let inputs = std::iter::once(a.files.as_path()).chain(paths.iter().map(Path::new));
        if inputs
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .any(|p| p == out)
        {
            return Err(format!("--out {} is one of the inputs", a.out.display()).into());
        }
    }
    let mut w = std::io::BufWriter::new(std::fs::File::create(&a.out)?);
    for path in paths {
        let page: Page = match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|x| parse_with_languages(&normalize(x), &opts).map_err(|e| e.to_string()))
        {
            Ok(p) => p,
            Err(e) => {
                writeln!(
                    w,
                    "{}",
                    serde_json::json!({"kind": "error", "file": path, "error": e})
                )?;
                continue;
            }
        };
        // As `drop_slivers`: only a page with word boxes is filtered.
        let filtered = page.granularity == Granularity::Word;
        let words = page.counts().2;
        let mut dropped = 0;
        for b in page.blocks.iter().filter(|b| b.kind.is_text()) {
            for l in &b.lines {
                if l.words.is_empty() {
                    continue;
                }
                let all = (vec![true; l.words.len()], true);
                let keep = if filtered {
                    kept(l, a.min_letter)
                } else {
                    all.clone()
                };
                dropped += keep.0.iter().filter(|k| !**k).count();
                writeln!(
                    w,
                    "{}",
                    serde_json::json!({
                        "kind": "line", "file": path,
                        "box": [l.rect.x0, l.rect.y0, l.rect.x1, l.rect.y1],
                        "before": line_text(l, &all),
                        "after": line_text(l, &keep),
                    })
                )?;
                let h = line_height(l);
                if !filtered || h.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                    continue;
                }
                for wd in &l.words {
                    let n = letters(wd.text.trim());
                    if n == 0 {
                        continue;
                    }
                    let ratio = wd.rect.width() / (h * n as f64);
                    if ratio < a.below {
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
        writeln!(
            w,
            "{}",
            serde_json::json!({
                "kind": "page", "file": path, "width": page.width, "height": page.height,
                "granularity": page.granularity.as_str(), "words": words,
                "dropped": s.words, "lines_dropped": s.lines, "dropped_at_min_letter": dropped,
            })
        )?;
    }
    w.flush()?;
    Ok(())
}
