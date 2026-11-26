//! `alto_cross_share` per page (#2167): the height-weighted share of an ALTO
//! page's text lines that [`usnm_alto::split_columns`] would cut, at its
//! current settings, from the ALTO alone.
//!
//!     cargo run --release -p usnm-alto --example alto_cross_share -- \
//!         --language eng --dpi 1200 --legacy-ns FILE_OR_DIR...
//!
//! LoC ALTO is mostly in `inch1200`; the reader needs a dpi to map it to
//! pixels, and `--dpi 1200` maps it one to one. The pass works in line
//! heights, so the scale doesn't change what it finds. With `--legacy-ns`,
//! a page in the pre-v2 namespace `http://schema.ccs-gmbh.com/ALTO` (which
//! the reader refuses) is read as ALTO v2, the namespace URI swapped in
//! memory, and the row says `legacy_ns`.
//!
//! One JSON line per page: `gutters` and `lines_split` as the pass reports
//! them, `lines` and `cut` (text lines with words, and those cut), the share
//! weighted by line height, the ALTO size in bytes, and the time to parse and
//! run the pass on one thread. A page whose parse fails gets `error`.
//!
//! Which lines were cut is read from the pass's own output: every word is
//! tagged with its index (in its confidence, which the pass carries through
//! and doesn't read), and a line is cut when its words end up in more than
//! one output line. The count is checked against `Repair::lines_split`.

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use usnm_alto::{default_dir, parse, split_columns, ParseOptions};

#[derive(Parser)]
struct Args {
    /// Language the page direction is taken from (`default_dir`).
    #[arg(long, default_value = "eng")]
    language: String,
    /// The image dpi given to the reader (needed for `inch1200` ALTO).
    #[arg(long)]
    dpi: Option<f64>,
    /// Read the pre-v2 CCS namespace as ALTO v2.
    #[arg(long)]
    legacy_ns: bool,
    /// ALTO files, or directories of `*.xml`.
    paths: Vec<PathBuf>,
}

fn files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(p)
                .expect("listing a directory")
                .map(|e| e.expect("directory entry").path())
                .filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml")))
                .collect();
            v.sort();
            out.extend(v);
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn main() {
    let args = Args::parse();
    let dir = default_dir(&args.language, None);
    let opts = ParseOptions {
        dpi: args.dpi,
        ..ParseOptions::default()
    };
    const CCS: &[u8] = b"\"http://schema.ccs-gmbh.com/ALTO\"";
    const V2: &[u8] = b"\"http://www.loc.gov/standards/alto/ns-v2#\"";
    for f in files(&args.paths) {
        let mut bytes = std::fs::read(&f).expect("reading ALTO");
        let head = bytes.len().min(4096);
        let legacy = match bytes[..head].windows(CCS.len()).position(|w| w == CCS) {
            Some(i) if args.legacy_ns => {
                bytes.splice(i..i + CCS.len(), V2.iter().copied());
                true
            }
            _ => false,
        };
        let t = Instant::now();
        let mut page = match parse(&bytes, &opts) {
            Ok(p) => p,
            Err(e) => {
                println!(
                    "{}",
                    serde_json::json!({"alto": f.display().to_string(), "bytes": bytes.len(), "legacy_ns": legacy, "error": e.to_string()})
                );
                continue;
            }
        };
        // Tag words; record each text line's height and word ids.
        let mut owner: Vec<usize> = Vec::new();
        let mut heights: Vec<f64> = Vec::new();
        for b in page.blocks.iter_mut().filter(|b| b.kind.is_text()) {
            for l in b.lines.iter_mut().filter(|l| !l.words.is_empty()) {
                let li = heights.len();
                heights.push(l.rect.height());
                for w in &mut l.words {
                    w.conf = Some(owner.len() as f64);
                    owner.push(li);
                }
            }
        }
        let out = split_columns(&page, dir);
        let secs = t.elapsed().as_secs_f64();
        let mut pieces = vec![0usize; heights.len()];
        let (gutters, lines_split) = match &out {
            None => (0, 0),
            Some((p, r)) => {
                for b in p.blocks.iter().filter(|b| b.kind.is_text()) {
                    for l in &b.lines {
                        let mut seen: Vec<usize> = l
                            .words
                            .iter()
                            .map(|w| owner[w.conf.expect("tagged word") as usize])
                            .collect();
                        seen.sort_unstable();
                        seen.dedup();
                        for li in seen {
                            pieces[li] += 1;
                        }
                    }
                }
                (r.gutters, r.lines_split)
            }
        };
        let cut: Vec<bool> = pieces.iter().map(|&n| n > 1).collect();
        let ncut = cut.iter().filter(|&&c| c).count();
        let tot: f64 = heights.iter().filter(|h| **h > 0.0).sum();
        let cutw: f64 = heights
            .iter()
            .zip(&cut)
            .filter(|(h, c)| **c && **h > 0.0)
            .map(|(h, _)| *h)
            .sum();
        let share = if tot > 0.0 { Some(cutw / tot) } else { None };
        println!(
            "{}",
            serde_json::json!({
                "alto": f.display().to_string(),
                "bytes": bytes.len(),
                "legacy_ns": legacy,
                "gutters": gutters,
                "lines_split": lines_split,
                "lines": heights.len(),
                "cut": ncut,
                "cut_matches_repair": ncut == lines_split,
                "alto_cross_share": share,
                "secs": secs,
            })
        );
    }
}
