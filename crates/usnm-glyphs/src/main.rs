//! `usnm-glyphs`: build and clean a glyph bank of the *Cherokee Phoenix*'s
//! own type from LoC's ALTO and page scans (#1620). See the library docs.
//!
//! ```sh
//! usnm-glyphs build --alto-dir loc --images images --out bank
//! usnm-glyphs clean --bank bank
//! ```
//!
//! `--alto-dir` holds LoC's batches unpacked (`<lccn>/<yyyy>/<mm>/<dd>/ed-N/seq-N/ocr.xml`);
//! `--images` the page scans as `<lccn>_<yyyy-mm-dd>_ed-N_seq-N.jpg`. The bank is
//! `index.jsonl` (one glyph a line), `glyphs/<code point>/<id>.png`, `features.f32`
//! (each glyph's feature, little-endian f32) and summaries.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use image::GrayImage;
use serde_json::{json, Value};
use usnm_core::cherokee_type::{fix_word, is_known, is_syllabary};
use usnm_glyphs::{agreement, feature, split_word, Ink, FEATURE_SIDE};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Cut and label the glyphs of every trusted syllabary word.
    Build {
        #[arg(long)]
        alto_dir: PathBuf,
        #[arg(long)]
        images: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 8)]
        threads: usize,
    },
    /// Keep the glyphs whose neighbours on other pages agree with their label.
    Clean {
        #[arg(long)]
        bank: PathBuf,
        #[arg(long, default_value_t = 9)]
        k: usize,
        #[arg(long, default_value_t = 0.5)]
        min_agree: f32,
        #[arg(long, default_value_t = 8)]
        threads: usize,
        /// A test-set manifest whose pages (`sets.*.pages`) never vote, so no frozen test page
        /// decides which glyphs of other pages are kept. Their own glyphs are still judged.
        #[arg(long)]
        exclude_manifest: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Build {
            alto_dir,
            images,
            out,
            threads,
        } => build(&alto_dir, &images, &out, threads),
        Cmd::Clean {
            bank,
            k,
            min_agree,
            threads,
            exclude_manifest,
        } => clean(&bank, k, min_agree, threads, exclude_manifest.as_deref()),
    }
}

/// Each page's ALTO file and slug (`sn83020866/1828-03-06/ed-1/seq-4`), sorted.
fn pages(alto_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack = vec![alto_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).with_context(|| dir.display().to_string())? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "ocr.xml") {
                let rel: Vec<String> = p
                    .strip_prefix(alto_dir)?
                    .iter()
                    .map(|c| c.to_string_lossy().into_owned())
                    .collect();
                if let [lccn, y, m, d, ed, seq, _] = rel.as_slice() {
                    out.push((format!("{lccn}/{y}-{m}-{d}/{ed}/{seq}"), p));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

const PUNCT: &[char] = &[
    '.', ',', ';', ':', '!', '?', '"', '\'', '“', '”', '‘', '’', '(', ')', '[', ']', '-', '—',
];

/// The glyph rows of one page, its counters, and the features in row order.
type PageOut = (Vec<Value>, BTreeMap<String, u64>, Vec<Vec<f32>>);

fn build_page(slug: &str, alto: &Path, images: &Path, out: &Path) -> Result<Option<PageOut>> {
    let img_path = images.join(format!("{}.jpg", slug.replace('/', "_")));
    if !img_path.exists() {
        return Ok(None);
    }
    let xml = fs::read(alto)?;
    let opts = usnm_alto::ParseOptions {
        dpi: Some(300.0),
        ..Default::default()
    };
    let page = usnm_alto::parse(&xml, &opts).with_context(|| alto.display().to_string())?;
    let scan = image::open(&img_path)
        .with_context(|| img_path.display().to_string())?
        .to_luma8();
    let (sx, sy) = (
        f64::from(scan.width()) / page.width,
        f64::from(scan.height()) / page.height,
    );
    let (mut rows, mut stats, mut feats) = (Vec::new(), BTreeMap::new(), Vec::new());
    let mut count = |k: &str| *stats.entry(k.to_owned()).or_insert(0u64) += 1;
    count("pages");
    for (bi, block) in page.blocks.iter().enumerate() {
        for (li, line) in block.lines.iter().enumerate() {
            for (wi, word) in line.words.iter().enumerate() {
                if !word.text.chars().any(is_syllabary) {
                    continue;
                }
                count("words");
                let core = word.text.trim_matches(PUNCT);
                if core.is_empty() || !core.chars().all(is_syllabary) {
                    count("not_clean");
                    continue;
                }
                let (label, how) = if is_known(core) {
                    (core.to_owned(), "known")
                } else if let Some(f) = fix_word(core) {
                    (f, "fixed")
                } else {
                    count("unknown");
                    continue;
                };
                let line_h = line.rect.height() * sy;
                let pad = (0.15 * line_h).max(2.0);
                let clamp = |v: f64, hi: u32| v.max(0.0).min(f64::from(hi)) as u32;
                let x0 = clamp(word.rect.x0 * sx - pad, scan.width());
                let x1 = clamp(word.rect.x1 * sx + pad, scan.width());
                let y0 = clamp(line.rect.y0 * sy - pad, scan.height());
                let y1 = clamp(line.rect.y1 * sy + pad, scan.height());
                if x1 <= x0 + 2 || y1 <= y0 + 2 {
                    continue;
                }
                let crop = image::imageops::crop_imm(&scan, x0, y0, x1 - x0, y1 - y0).to_image();
                let ink = Ink::of(&crop);
                let letters: Vec<char> = label.chars().collect();
                let core_letters: Vec<char> = core.chars().collect();
                let Some(boxes) = split_word(&ink, letters.len()) else {
                    count(&format!("split_failed_{how}"));
                    continue;
                };
                count(&format!("split_ok_{how}"));
                for (i, (ch, b)) in letters.iter().zip(&boxes).enumerate() {
                    // Block, line and word index make the id unique on its page.
                    let id = format!("{}_b{bi}_l{li}_w{wi}_{i}", slug.replace('/', "_"));
                    let dir = out.join("glyphs").join(format!("{:04x}", u32::from(*ch)));
                    fs::create_dir_all(&dir)?;
                    let (gx0, gy0) = (b[0].saturating_sub(2), b[1].saturating_sub(2));
                    let (gx1, gy1) = ((b[2] + 2).min(crop.width()), (b[3] + 2).min(crop.height()));
                    let glyph: GrayImage =
                        image::imageops::crop_imm(&crop, gx0, gy0, gx1 - gx0, gy1 - gy0).to_image();
                    glyph.save(dir.join(format!("{id}.png")))?;
                    feats.push(feature(&ink, *b));
                    rows.push(json!({
                        "id": id, "letter": ch.to_string(), "page": slug, "date": slug.split('/').nth(1),
                        "word": label, "loc_word": core, "how": how, "position": i, "initial": i == 0,
                        "read_as": (core_letters.len() == letters.len()).then(|| core_letters[i].to_string()),
                        "box": [x0 + b[0], y0 + b[1], x0 + b[2], y0 + b[3]],
                    }));
                }
            }
        }
    }
    Ok(Some((rows, stats, feats)))
}

fn build(alto_dir: &Path, images: &Path, out: &Path, threads: usize) -> Result<()> {
    fs::create_dir_all(out.join("glyphs"))?;
    let pages = pages(alto_dir)?;
    let results: Vec<Result<Option<PageOut>>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads.max(1))
            .map(|t| {
                let pages = &pages;
                s.spawn(move || {
                    pages
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| i % threads.max(1) == t)
                        .map(|(i, (slug, alto))| (i, build_page(slug, alto, images, out)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all: Vec<(usize, Result<Option<PageOut>>)> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("a page thread"))
            .collect();
        all.sort_by_key(|(i, _)| *i);
        all.into_iter().map(|(_, r)| r).collect()
    });
    let mut index = BufWriter::new(fs::File::create(out.join("index.jsonl"))?);
    let mut fout = BufWriter::new(fs::File::create(out.join("features.f32"))?);
    let mut stats: BTreeMap<String, u64> = BTreeMap::new();
    let mut per: BTreeMap<String, u64> = BTreeMap::new();
    let mut glyphs = 0u64;
    for r in results {
        let Some((rows, st, feats)) = r? else {
            continue;
        };
        for (k, v) in st {
            *stats.entry(k).or_default() += v;
        }
        for (row, f) in rows.iter().zip(&feats) {
            writeln!(index, "{row}")?;
            for x in f {
                fout.write_all(&x.to_le_bytes())?;
            }
            *per.entry(row["letter"].as_str().unwrap_or_default().to_owned())
                .or_default() += 1;
            glyphs += 1;
        }
    }
    index.flush()?;
    fout.flush()?;
    let missing: String = (0x13A0..0x13F5)
        .filter_map(char::from_u32)
        .filter(|c| !per.contains_key(&c.to_string()))
        .collect();
    let summary = json!({"stats": stats, "glyphs": glyphs, "letters": per.len(), "per_letter": per,
        "missing": missing, "feature_side": FEATURE_SIDE});
    fs::write(
        out.join("summary-build.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    println!(
        "{}",
        json!({"stats": summary["stats"], "glyphs": glyphs, "letters": per.len(), "missing": missing})
    );
    Ok(())
}

/// The page slugs listed in a test-set manifest's `sets.*.pages` arrays.
fn manifest_pages(path: &Path) -> Result<HashSet<String>> {
    let m: Value = serde_json::from_slice(&fs::read(path)?)?;
    let mut out = HashSet::new();
    for set in m["sets"].as_object().into_iter().flat_map(|o| o.values()) {
        for p in set["pages"].as_array().into_iter().flatten() {
            if let Some(p) = p.as_str() {
                out.insert(p.to_owned());
            }
        }
    }
    anyhow::ensure!(!out.is_empty(), "{} lists no pages", path.display());
    Ok(out)
}

fn clean(
    bank: &Path,
    k: usize,
    min_agree: f32,
    threads: usize,
    exclude_manifest: Option<&Path>,
) -> Result<()> {
    let excluded = exclude_manifest
        .map(manifest_pages)
        .transpose()?
        .unwrap_or_default();
    let rows: Vec<Value> = BufReader::new(fs::File::open(bank.join("index.jsonl"))?)
        .lines()
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect::<Result<_>>()?;
    let bytes = fs::read(bank.join("features.f32"))?;
    let dim = FEATURE_SIDE * FEATURE_SIDE;
    anyhow::ensure!(
        bytes.len() == rows.len() * dim * 4,
        "features.f32 doesn't match index.jsonl"
    );
    let features: Vec<Vec<f32>> = bytes
        .chunks_exact(dim * 4)
        .map(|c| {
            c.as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        })
        .collect();
    // Word-initial Ꮎ, printed as a block G, is its own class in the vote.
    let class_of = |r: &Value| {
        let l = r["letter"].as_str().unwrap_or_default();
        if l == "Ꮎ" && r["initial"].as_bool() == Some(true) {
            "Ꮎ^".to_owned()
        } else {
            l.to_owned()
        }
    };
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut intern = |s: String| {
        let n = ids.len() as u32;
        *ids.entry(s).or_insert(n)
    };
    let classes: Vec<u32> = rows.iter().map(|r| intern(class_of(r))).collect();
    let pages: Vec<u32> = rows
        .iter()
        .map(|r| intern(format!("page:{}", r["page"])))
        .collect();
    let voters: Vec<bool> = rows
        .iter()
        .map(|r| !excluded.contains(r["page"].as_str().unwrap_or_default()))
        .collect();
    let agree = agreement(&features, &classes, &pages, &voters, k, threads);
    let mut index = BufWriter::new(fs::File::create(bank.join("index.jsonl.tmp"))?);
    let mut per: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut kept_n = 0u64;
    for (mut r, g) in rows.into_iter().zip(agree) {
        let kept = g >= min_agree;
        let key = class_of(&r).replace("Ꮎ^", "Ꮎ (word-initial)");
        let e = per.entry(key).or_default();
        e.0 += 1;
        e.1 += u64::from(kept);
        kept_n += u64::from(kept);
        r["agree"] = json!((g * 1000.0).round() / 1000.0);
        r["kept"] = json!(kept);
        writeln!(index, "{r}")?;
    }
    index.flush()?;
    drop(index);
    fs::rename(bank.join("index.jsonl.tmp"), bank.join("index.jsonl"))?;
    let per: BTreeMap<String, Value> = per
        .into_iter()
        .map(|(k, (a, b))| (k, json!({"all": a, "kept": b})))
        .collect();
    let total = per
        .values()
        .map(|v| v["all"].as_u64().unwrap_or(0))
        .sum::<u64>();
    let summary = json!({"excluded_voter_pages": excluded.len(), "glyphs": total, "k": k, "kept": kept_n,
        "min_agree": min_agree, "per_letter": per});
    fs::write(
        bank.join("summary-clean.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    println!("{}", json!({"glyphs": total, "kept": kept_n}));
    Ok(())
}
