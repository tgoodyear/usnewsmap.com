//! `usnm-glyphs`: build and clean a glyph bank of the *Cherokee Phoenix*'s
//! own type from LoC's ALTO and page scans (#1620). See the library docs.
//!
//! ```sh
//! usnm-glyphs build --alto-dir loc --images images --out bank
//! usnm-glyphs clean --bank bank
//! usnm-glyphs add-checked --bank bank --out bank-checked --export check-chr-rare.jsonl \
//!     --index glyph-index.jsonl --images images --exclude-manifest manifest.json
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
use usnm_glyphs::{agreement, feature, split_word, word_crop, Ink, FEATURE_SIDE};

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
    /// Add the glyphs of words people settled on check.usnewsmap.com to a copy of a bank.
    AddChecked {
        /// The bank to start from (after `clean`).
        #[arg(long)]
        bank: PathBuf,
        /// The new bank; it must not exist yet. Glyph pictures are hard-linked from `--bank` where possible.
        #[arg(long)]
        out: PathBuf,
        /// The check site's `GET /api/export?lang=<code>`, JSON lines.
        #[arg(long)]
        export: PathBuf,
        /// `glyph-index.jsonl` of the build that made the tasks (`edge/check/tools/phoenix_lines.py`);
        /// repeat it for each set.
        #[arg(long, required = true)]
        index: Vec<PathBuf>,
        /// The page scans, `<lccn>_<yyyy-mm-dd>_ed-N_seq-N.jpg`.
        #[arg(long)]
        images: PathBuf,
        /// A test-set manifest whose pages (`sets.*.pages`) never contribute a glyph.
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
        Cmd::AddChecked {
            bank,
            out,
            export,
            index,
            images,
            exclude_manifest,
        } => {
            let summary = add_checked(
                &bank,
                &out,
                &export,
                &index,
                &images,
                exclude_manifest.as_deref(),
            )?;
            println!("{summary}");
            Ok(())
        }
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
                let Some([x0, y0, x1, y1]) = word_crop(
                    word.rect.x0 * sx,
                    word.rect.x1 * sx,
                    line.rect.y0 * sy,
                    line.rect.y1 * sy,
                    scan.width(),
                    scan.height(),
                ) else {
                    continue;
                };
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
    anyhow::ensure!(k > 0, "--k must be at least 1");
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
        // A glyph from a word people checked is kept whatever its neighbours say: it is how
        // letters with no neighbours yet get into the bank.
        let kept = g >= min_agree || r["how"] == "checked";
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

/// Hard-links (or copies) every file under `from` into `to`, keeping the layout.
fn link_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from).with_context(|| from.display().to_string())? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            link_tree(&e.path(), &target)?;
        } else if fs::hard_link(e.path(), &target).is_err() {
            fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    BufReader::new(fs::File::open(path).with_context(|| path.display().to_string())?)
        .lines()
        .filter(|l| l.as_ref().map_or(true, |l| !l.trim().is_empty()))
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect()
}

/// Copies `bank` to `out` and adds the glyphs of words people settled on the check site.
///
/// A task is used when it settled on reading `a` or `b` (so every letter of that reading matches
/// the print), its word is in the build's `glyph-index.jsonl` with the same readings, and its
/// page isn't in `exclude_manifest`. The word is cut from the scan as `build` does and split into
/// as many glyphs as the reading has letters; each is labelled with its letter, `how: "checked"`
/// and kept by `clean` whatever its neighbours say. Words that don't split plausibly add nothing.
fn add_checked(
    bank: &Path,
    out: &Path,
    export: &Path,
    index: &[PathBuf],
    images: &Path,
    exclude_manifest: Option<&Path>,
) -> Result<Value> {
    anyhow::ensure!(
        !out.exists(),
        "{} exists; add-checked writes a new bank",
        out.display()
    );
    let excluded = exclude_manifest
        .map(manifest_pages)
        .transpose()?
        .unwrap_or_default();
    let mut words: HashMap<String, Value> = HashMap::new();
    for p in index {
        for v in read_jsonl(p)? {
            if let Some(w) = v["word"].as_str() {
                let word = w.to_owned();
                if let Some(old) = words.insert(word.clone(), v.clone()) {
                    anyhow::ensure!(old == v, "{word} is in the index twice, differently");
                }
            }
        }
    }
    // A bank that already holds checked words (an earlier add-checked) has them by task. A task there
    // is skipped when it settled the same way, so a newer export can be added to the earlier result; one
    // that settled differently would need the old glyphs replaced, which this never does.
    let old_rows = read_jsonl(&bank.join("index.jsonl"))?;
    let already: HashMap<String, (String, String)> = old_rows
        .iter()
        .filter(|r| r["how"] == "checked")
        .filter_map(|r| {
            Some((
                r["checked_task"].as_str()?.to_owned(),
                (
                    r["outcome"].as_str()?.to_owned(),
                    r["word"].as_str()?.to_owned(),
                ),
            ))
        })
        .collect();
    let mut stats: BTreeMap<String, u64> = BTreeMap::new();
    let mut count = |k: &str| *stats.entry(k.to_owned()).or_insert(0u64) += 1;
    // (page, word key, task id, outcome, letters of the settled reading)
    let mut picks: Vec<(String, String, String, String, String)> = Vec::new();
    for r in read_jsonl(export)? {
        count("tasks");
        if r["settled"] != true {
            count("unsettled");
            continue;
        }
        let outcome = r["outcome"].as_str().unwrap_or_default();
        if outcome != "a" && outcome != "b" {
            count("no_winner");
            continue;
        }
        let id = r["id"].as_str().unwrap_or_default();
        let key = id.rsplit('/').next().unwrap_or_default();
        let Some(w) = words.get(key) else {
            count("not_in_index");
            continue;
        };
        let shown = r[outcome].as_str().unwrap_or_default();
        if w[outcome].as_str() != Some(shown) {
            count("index_differs");
            continue;
        }
        let page = w["page"].as_str().unwrap_or_default();
        if excluded.contains(page) {
            count("excluded_page");
            continue;
        }
        let letters: String = shown.chars().filter(|c| is_syllabary(*c)).collect();
        if letters.is_empty() {
            count("no_letters");
            continue;
        }
        if let Some((was_outcome, was_word)) = already.get(id) {
            anyhow::ensure!(
                was_outcome == outcome && *was_word == letters,
                "{id} is in the bank as {was_outcome} {was_word} but settled as {outcome} {letters}; \
                 start again from a bank without it"
            );
            count("already_in_bank");
            continue;
        }
        picks.push((
            page.to_owned(),
            key.to_owned(),
            id.to_owned(),
            outcome.to_owned(),
            letters,
        ));
    }
    picks.sort();

    let old_features = fs::read(bank.join("features.f32"))?;
    anyhow::ensure!(
        old_features.len() == old_rows.len() * FEATURE_SIDE * FEATURE_SIDE * 4,
        "features.f32 doesn't match index.jsonl"
    );
    link_tree(&bank.join("glyphs"), &out.join("glyphs"))?;
    fs::copy(bank.join("index.jsonl"), out.join("index.jsonl"))?;
    fs::copy(bank.join("features.f32"), out.join("features.f32"))?;
    let mut index_out = BufWriter::new(
        fs::OpenOptions::new()
            .append(true)
            .open(out.join("index.jsonl"))?,
    );
    let mut features_out = BufWriter::new(
        fs::OpenOptions::new()
            .append(true)
            .open(out.join("features.f32"))?,
    );
    let mut per: BTreeMap<String, u64> = BTreeMap::new();
    let mut scan: Option<(String, GrayImage)> = None;
    for (page, key, task, outcome, letters) in &picks {
        if scan.as_ref().map(|(p, _)| p) != Some(page) {
            let path = images.join(format!("{}.jpg", page.replace('/', "_")));
            let img = image::open(&path)
                .with_context(|| path.display().to_string())?
                .to_luma8();
            scan = Some((page.clone(), img));
        }
        let img = &scan.as_ref().expect("a scan").1;
        let w = &words[key];
        let f = |v: &Value, i: usize| v[i].as_f64().unwrap_or_default();
        let Some([x0, y0, x1, y1]) = word_crop(
            f(&w["box"], 0),
            f(&w["box"], 2),
            f(&w["line"], 1),
            f(&w["line"], 3),
            img.width(),
            img.height(),
        ) else {
            count("crop_too_small");
            continue;
        };
        let crop = image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image();
        let ink = Ink::of(&crop);
        let chars: Vec<char> = letters.chars().collect();
        let Some(boxes) = split_word(&ink, chars.len()) else {
            count("split_failed");
            continue;
        };
        count("words_cut");
        for (i, (ch, b)) in chars.iter().zip(&boxes).enumerate() {
            let id = format!(
                "{}_chk_{}_{i}",
                page.replace('/', "_"),
                key.replace('~', "_k")
            );
            let dir = out.join("glyphs").join(format!("{:04x}", u32::from(*ch)));
            fs::create_dir_all(&dir)?;
            let (gx0, gy0) = (b[0].saturating_sub(2), b[1].saturating_sub(2));
            let (gx1, gy1) = ((b[2] + 2).min(crop.width()), (b[3] + 2).min(crop.height()));
            let glyph: GrayImage =
                image::imageops::crop_imm(&crop, gx0, gy0, gx1 - gx0, gy1 - gy0).to_image();
            let png = dir.join(format!("{id}.png"));
            // Pictures are hard-linked from the starting bank: never write through one of them.
            anyhow::ensure!(!png.exists(), "{} is already in the bank", png.display());
            glyph.save(&png)?;
            for x in feature(&ink, *b) {
                features_out.write_all(&x.to_le_bytes())?;
            }
            writeln!(
                index_out,
                "{}",
                json!({
                    "id": id, "letter": ch.to_string(), "page": page, "date": page.split('/').nth(1),
                    "word": letters, "loc_word": null, "how": "checked", "position": i, "initial": i == 0,
                    "read_as": null, "box": [x0 + b[0], y0 + b[1], x0 + b[2], y0 + b[3]],
                    "checked_task": task, "outcome": outcome, "agree": 1.0, "kept": true,
                })
            )?;
            *per.entry(ch.to_string()).or_default() += 1;
        }
    }
    index_out.flush()?;
    features_out.flush()?;
    let had: HashSet<String> = old_rows
        .iter()
        .filter(|r| r["kept"] == true)
        .filter_map(|r| r["letter"].as_str().map(str::to_owned))
        .collect();
    let new_letters: String = per.keys().filter(|l| !had.contains(*l)).cloned().collect();
    let summary = json!({
        "stats": stats, "glyphs_added": per.values().sum::<u64>(), "per_letter": per,
        "letters_new_to_the_bank": new_letters, "excluded_pages": excluded.len(),
    });
    fs::write(
        out.join("summary-checked.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("usnm-glyphs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_lines(path: &Path, rows: &[Value]) {
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        fs::write(path, text).unwrap();
    }

    /// A 300 x 120 scan with three glyph-sized bars in a word at x 20..140, y 40..80.
    fn scan() -> GrayImage {
        let mut img = GrayImage::from_pixel(300, 120, image::Luma([235]));
        for x0 in [30u32, 70, 110] {
            for y in 45..75 {
                for x in x0..x0 + 20 {
                    img.put_pixel(x, y, image::Luma([20]));
                }
            }
        }
        img
    }

    #[test]
    fn checked_words_add_glyphs_and_test_pages_never_do() {
        let dir = scratch("checked");
        let (bank, images) = (dir.join("bank"), dir.join("images"));
        fs::create_dir_all(bank.join("glyphs/13a0")).unwrap();
        fs::create_dir_all(&images).unwrap();
        fs::write(bank.join("glyphs/13a0/old.png"), b"png").unwrap();
        write_lines(
            &bank.join("index.jsonl"),
            &[json!({"id": "old", "letter": "Ꭰ", "kept": true})],
        );
        fs::write(
            bank.join("features.f32"),
            vec![0u8; FEATURE_SIDE * FEATURE_SIDE * 4],
        )
        .unwrap();
        for page in ["sn1_1828-03-06_ed-1_seq-1", "sn1_1828-03-13_ed-1_seq-1"] {
            scan().save(images.join(format!("{page}.jpg"))).unwrap();
        }
        let word = |key: &str, page: &str| {
            json!({"word": key, "page": page, "a": "ᎠᎴᎣ", "b": "ᎠᎴᏀ",
                   "line": [0.0, 40.0, 300.0, 80.0], "box": [20.0, 40.0, 140.0, 80.0]})
        };
        write_lines(
            &dir.join("index-in.jsonl"),
            &[
                word("l1~0", "sn1/1828-03-06/ed-1/seq-1"),
                word("l2~0", "sn1/1828-03-06/ed-1/seq-1"),
                word("l3~0", "sn1/1828-03-06/ed-1/seq-1"),
                word("l4~0", "sn1/1828-03-13/ed-1/seq-1"),
            ],
        );
        let task = |key: &str, settled: bool, outcome: Value, b: &str| {
            json!({"id": format!("chr-rare/rare-v1/{key}"), "a": "ᎠᎴᎣ", "b": b,
                   "settled": settled, "outcome": outcome})
        };
        write_lines(
            &dir.join("export.jsonl"),
            &[
                task("l1~0", true, json!("b"), "ᎠᎴᏀ"), // used: Ꮐ is new to the bank
                task("l2~0", true, json!("neither"), "ᎠᎴᏀ"), // no winner
                task("l3~0", false, json!(null), "ᎠᎴᏀ"), // not settled
                task("l4~0", true, json!("a"), "ᎠᎴᏀ"), // a test-set page
                task("l9~0", true, json!("a"), "ᎠᎴᏀ"), // not in the index
            ],
        );
        fs::write(
            dir.join("manifest.json"),
            json!({"sets": {"t": {"pages": ["sn1/1828-03-13/ed-1/seq-1"]}}}).to_string(),
        )
        .unwrap();
        let out = dir.join("out");
        let s = add_checked(
            &bank,
            &out,
            &dir.join("export.jsonl"),
            &[dir.join("index-in.jsonl")],
            &images,
            Some(&dir.join("manifest.json")),
        )
        .unwrap();
        assert_eq!(s["glyphs_added"], 3, "{s}");
        assert_eq!(s["stats"]["tasks"], 5);
        assert_eq!(s["stats"]["no_winner"], 1);
        assert_eq!(s["stats"]["unsettled"], 1);
        assert_eq!(s["stats"]["excluded_page"], 1);
        assert_eq!(s["stats"]["not_in_index"], 1);
        assert_eq!(s["per_letter"]["Ꮐ"], 1);
        assert_eq!(s["letters_new_to_the_bank"], "ᎴᏀ"); // Ꭰ was in the bank already
        let rows = read_jsonl(&out.join("index.jsonl")).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3]["letter"], "Ꮐ");
        assert_eq!(rows[3]["how"], "checked");
        assert_eq!(rows[3]["kept"], true);
        assert_eq!(rows[3]["initial"], false);
        assert_eq!(
            fs::metadata(out.join("features.f32")).unwrap().len() as usize,
            4 * FEATURE_SIDE * FEATURE_SIDE * 4
        );
        assert!(out.join("glyphs/13a0/old.png").exists());
        assert!(out.join("glyphs/13c0").is_dir());
        // The bank it started from is unchanged, and a second run onto the same path is refused.
        assert_eq!(read_jsonl(&bank.join("index.jsonl")).unwrap().len(), 1);
        assert!(add_checked(
            &bank,
            &out,
            &dir.join("export.jsonl"),
            &[dir.join("index-in.jsonl")],
            &images,
            None
        )
        .is_err());
        // A newer export added onto the result skips the words it already holds...
        let again = add_checked(
            &out,
            &dir.join("out2"),
            &dir.join("export.jsonl"),
            &[dir.join("index-in.jsonl")],
            &images,
            Some(&dir.join("manifest.json")),
        )
        .unwrap();
        assert_eq!(again["glyphs_added"], 0, "{again}");
        assert_eq!(again["stats"]["already_in_bank"], 1);
        assert_eq!(read_jsonl(&dir.join("out2/index.jsonl")).unwrap().len(), 4);
        // ...and refuses one that settled differently.
        write_lines(
            &dir.join("export-changed.jsonl"),
            &[task("l1~0", true, json!("a"), "ᎠᎴᏀ")],
        );
        assert!(add_checked(
            &out,
            &dir.join("out3"),
            &dir.join("export-changed.jsonl"),
            &[dir.join("index-in.jsonl")],
            &images,
            None
        )
        .is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reading_that_differs_from_the_index_adds_nothing() {
        let dir = scratch("differs");
        let bank = dir.join("bank");
        fs::create_dir_all(bank.join("glyphs")).unwrap();
        write_lines(&bank.join("index.jsonl"), &[]);
        fs::write(bank.join("features.f32"), b"").unwrap();
        write_lines(
            &dir.join("index-in.jsonl"),
            &[
                json!({"word": "l1~0", "page": "p", "a": "ᎠᎴ", "b": "ᎠᎵ", "line": [0, 0, 1, 1], "box": [0, 0, 1, 1]}),
            ],
        );
        write_lines(
            &dir.join("export.jsonl"),
            &[
                json!({"id": "chr-rare/rare-v1/l1~0", "a": "ᎠᎴ", "b": "ᎠᏢ", "settled": true, "outcome": "b"}),
            ],
        );
        let s = add_checked(
            &bank,
            &dir.join("out"),
            &dir.join("export.jsonl"),
            &[dir.join("index-in.jsonl")],
            &dir,
            None,
        )
        .unwrap();
        assert_eq!(s["stats"]["index_differs"], 1);
        assert_eq!(s["glyphs_added"], 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
