//! Study 11 helpers. Everything an engine returned is read as data.
//!
//! prepare     region selection, region crops, page list and masked page images for one held-out set
//! line-crops  line crops from detected line boxes on whole pages, in detection order
//! bag         order-free word-bag precision, recall and F1 per page and pooled, with page-resampled limits
//! vote        per region, the reading with the smallest summed character edit distance to the others

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use image::{DynamicImage, GenericImage, GenericImageView, Rgb};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use usnm_ocr_survey::score::read_jsonl;
use usnm_ocr_survey::{distance, text};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Prepare {
        /// The s9-heldout directory
        #[arg(long)]
        heldout: PathBuf,
        /// en-asgold, de-rz or de-cg
        #[arg(long)]
        set: String,
        /// Keep the first N transcribed regions by sha256("s11:<set>:"+id+":"+region); all when absent
        #[arg(long)]
        max_regions: Option<usize>,
        /// With --max-regions: at most this many per page
        #[arg(long)]
        per_page: Option<usize>,
        /// Pages for the page arm, a prefix of selection.tsv
        #[arg(long)]
        pages: usize,
        #[arg(long)]
        out: PathBuf,
    },
    LineCrops {
        /// Directory the page images are relative to
        #[arg(long)]
        root: PathBuf,
        /// JSONL {id,image}
        #[arg(long)]
        pages: PathBuf,
        /// JSONL {id,boxes:[[[x,y]x4]...]} in detection order
        #[arg(long)]
        boxes: PathBuf,
        #[arg(long)]
        pad: u32,
        #[arg(long)]
        out: PathBuf,
    },
    Bag {
        /// JSONL {id,page,ref}
        #[arg(long)]
        refs: PathBuf,
        /// NAME=FILE; repeatable
        #[arg(long = "hyp")]
        hyps: Vec<String>,
        #[arg(long)]
        baseline: Option<String>,
        /// Fold long s, the double hyphen and superscript e on both sides first (gpu-study --fold)
        #[arg(long)]
        fold: bool,
        #[arg(long, default_value_t = 2000)]
        resamples: usize,
        #[arg(long, default_value_t = 202611)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
    },
    Vote {
        /// JSONL {id}; the regions to vote on
        #[arg(long)]
        refs: PathBuf,
        /// NAME=FILE in tie order; repeatable
        #[arg(long = "hyp")]
        hyps: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn write_rows(out: &Path, rows: &[Value]) -> Result<()> {
    let s: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(out, s).with_context(|| format!("writing {}", out.display()))
}

fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn open(path: &Path) -> Result<DynamicImage> {
    Ok(image::ImageReader::open(path)?.with_guessed_format()?.decode().with_context(|| format!("decoding {}", path.display()))?)
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}

/// A box clamped to the image, as (x, y, w, h); None when empty.
fn clamp(img: &impl GenericImageView, x0: i64, y0: i64, x1: i64, y1: i64) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (a, b, c, d) = (x0.clamp(0, w), y0.clamp(0, h), x1.clamp(0, w), y1.clamp(0, h));
    (c > a && d > b).then_some((a as u32, b as u32, (c - a) as u32, (d - b) as u32))
}

fn bx(r: &Value) -> Result<[i64; 4]> {
    let v: Vec<i64> = r["box"].as_array().context("box")?.iter().filter_map(|x| x.as_f64()).map(|x| x.round() as i64).collect();
    if v.len() != 4 {
        bail!("box needs 4 numbers");
    }
    Ok([v[0], v[1], v[2], v[3]])
}

fn prepare(heldout: &Path, set: &str, max_regions: Option<usize>, per_page: Option<usize>, pages: usize, out: &Path) -> Result<()> {
    let dir = heldout.join(set);
    let refs = read_jsonl(&dir.join("refs.jsonl"))?;
    let page_rows: HashMap<String, &Value> = refs.iter().map(|r| (r["id"].as_str().unwrap_or("").to_string(), r)).collect();
    let regions = read_jsonl(&dir.join("regions.jsonl"))?;
    let mut chosen: Vec<&Value> = regions.iter().filter(|r| r["transcribed"].as_bool() == Some(true)).collect();
    if let Some(n) = max_regions {
        let mut keyed: Vec<(String, &Value)> = chosen
            .iter()
            .map(|r| (sha(&format!("s11:{set}:{}:{}", r["id"].as_str().unwrap_or(""), r["region"].as_str().unwrap_or(""))), *r))
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        let cap = per_page.unwrap_or(usize::MAX);
        let mut count: HashMap<&str, usize> = HashMap::new();
        chosen = vec![];
        for (_, r) in keyed {
            if chosen.len() == n {
                break;
            }
            let c = count.entry(r["id"].as_str().unwrap_or("")).or_default();
            if *c < cap {
                *c += 1;
                chosen.push(r);
            }
        }
    }
    fs::create_dir_all(out.join("crops"))?;
    fs::create_dir_all(out.join("pages"))?;
    // Crops, one page image decoded at a time, written in the order chosen.
    let mut by_page: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, r) in chosen.iter().enumerate() {
        by_page.entry(r["id"].as_str().context("region id")?).or_default().push(i);
    }
    let mut files: Vec<Option<(String, u32, u32)>> = vec![None; chosen.len()];
    for (page, idx) in &by_page {
        let p = page_rows.get(*page).with_context(|| format!("no page row for {page}"))?;
        let img = open(&dir.join(p["image"].as_str().context("image")?))?;
        for &i in idx {
            let r = chosen[i];
            let [x0, y0, x1, y1] = bx(r)?;
            let Some((x, y, w, h)) = clamp(&img, x0, y0, x1, y1) else { continue };
            let file = format!("crops/{}__{}.png", safe(page), safe(r["region"].as_str().unwrap_or("")));
            img.crop_imm(x, y, w, h).save(out.join(&file))?;
            files[i] = Some((file, w, h));
        }
    }
    let (mut ref_rows, mut crop_rows, mut tsv, mut skipped, mut over) = (vec![], vec![], String::new(), 0, 0);
    for (i, r) in chosen.iter().enumerate() {
        let page = r["id"].as_str().unwrap_or("");
        let p = page_rows[page];
        let id = format!("{page}:{}", r["region"].as_str().unwrap_or(""));
        let Some((file, w, h)) = &files[i] else {
            skipped += 1;
            continue;
        };
        over += usize::from((*w).max(*h) > 2048);
        ref_rows.push(json!({"id": id, "page": page, "source": set, "lang": p["lang"], "ref": r["text"], "type": r["type"], "w": w, "h": h}));
        crop_rows.push(json!({"id": id, "image": file}));
        tsv.push_str(&format!("{id}\t{file}\n"));
    }
    write_rows(&out.join("refs.jsonl"), &ref_rows)?;
    write_rows(&out.join("crops.jsonl"), &crop_rows)?;
    fs::write(out.join("list.tsv"), tsv)?;
    // Page arm: a prefix of the frozen order; text in untranscribed regions is masked white,
    // and transcribed regions are restored over any overlap.
    let sel = fs::read_to_string(dir.join("selection.tsv"))?;
    let mut ids: Vec<(u64, String)> = sel
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some((f.first()?.parse().ok()?, f.get(2)?.to_string()))
        })
        .collect();
    ids.sort();
    let (mut prow, mut pref, mut ptsv, mut masked) = (vec![], vec![], String::new(), 0);
    for (_, id) in ids.iter().take(pages) {
        let p = page_rows[id.as_str()];
        let src = dir.join(p["image"].as_str().context("image")?);
        let mine: Vec<&Value> = regions.iter().filter(|r| r["id"].as_str() == Some(id)).collect();
        let hidden: Vec<&&Value> = mine.iter().filter(|r| r["transcribed"].as_bool() != Some(true)).collect();
        let file = if hidden.is_empty() {
            let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("jpg");
            let f = format!("pages/{id}.{ext}");
            fs::copy(&src, out.join(&f))?;
            f
        } else {
            masked += 1;
            let orig = open(&src)?.to_rgb8();
            let mut img = orig.clone();
            for r in &hidden {
                let [x0, y0, x1, y1] = bx(r)?;
                if let Some((x, y, w, h)) = clamp(&img, x0, y0, x1, y1) {
                    for yy in y..y + h {
                        for xx in x..x + w {
                            img.put_pixel(xx, yy, Rgb([255, 255, 255]));
                        }
                    }
                }
            }
            for r in mine.iter().filter(|r| r["transcribed"].as_bool() == Some(true)) {
                let [x0, y0, x1, y1] = bx(r)?;
                if let Some((x, y, w, h)) = clamp(&img, x0, y0, x1, y1) {
                    img.copy_from(&*orig.view(x, y, w, h), x, y)?;
                }
            }
            let f = format!("pages/{id}.png");
            img.save(out.join(&f))?;
            f
        };
        prow.push(json!({"id": id, "image": file}));
        pref.push(json!({"id": id, "page": id, "source": set, "lang": p["lang"], "ref": p["ref"], "masked_regions": hidden.len()}));
        ptsv.push_str(&format!("{id}\t{file}\n"));
    }
    write_rows(&out.join("pages.jsonl"), &prow)?;
    write_rows(&out.join("page-refs.jsonl"), &pref)?;
    fs::write(out.join("pages.tsv"), ptsv)?;
    eprintln!(
        "{set}: {} regions on {} pages, {skipped} empty boxes skipped, {over} over 2048 px; {} pages for the page arm, {masked} masked",
        ref_rows.len(),
        by_page.len(),
        prow.len()
    );
    Ok(())
}

fn line_crops(root: &Path, pages: &Path, boxes: &Path, pad: u32, out: &Path) -> Result<()> {
    let pages: HashMap<String, String> =
        read_jsonl(pages)?.iter().map(|r| (r["id"].as_str().unwrap_or("").to_string(), r["image"].as_str().unwrap_or("").to_string())).collect();
    fs::create_dir_all(out.join("lines"))?;
    let mut rows = vec![];
    for b in read_jsonl(boxes)? {
        let id = b["id"].as_str().context("id")?;
        let img = open(&root.join(pages.get(id).with_context(|| format!("no page {id}"))?))?;
        let list = b["boxes"].as_array().context("boxes")?;
        let mut n = 0;
        for quad in list {
            let pts: Vec<(f64, f64)> = quad
                .as_array()
                .context("quad")?
                .iter()
                .filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?)))
                .collect();
            if pts.is_empty() {
                continue;
            }
            let x0 = pts.iter().map(|p| p.0).fold(f64::MAX, f64::min).floor() as i64 - pad as i64;
            let y0 = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min).floor() as i64 - pad as i64;
            let x1 = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max).ceil() as i64 + pad as i64;
            let y1 = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max).ceil() as i64 + pad as i64;
            let Some((x, y, w, h)) = clamp(&img, x0, y0, x1, y1) else { continue };
            n += 1;
            let file = format!("lines/{}__L{n:05}.png", safe(id));
            img.crop_imm(x, y, w, h).save(out.join(&file))?;
            rows.push(json!({"id": format!("{id}:L{n:05}"), "page": id, "order": n, "image": file}));
        }
        eprintln!("{id}: {n} lines");
    }
    write_rows(&out.join("crops.jsonl"), &rows)
}

fn folded(s: &str) -> String {
    s.replace('ſ', "s").replace('⸗', "-").replace('\u{2E17}', "-").replace('\u{0364}', "\u{0308}").replace('ʒ', "z").replace('¬', "-")
}

/// gpu-study's tokens: normalized words, outer punctuation removed, lower-cased.
fn tokens(s: &str) -> Vec<String> {
    let n = text::normalize(s, false);
    text::py_split(&n)
        .iter()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn hits(a: &[String], b: &[String]) -> usize {
    let mut m: HashMap<&str, i64> = HashMap::new();
    for w in a {
        *m.entry(w).or_default() += 1;
    }
    let mut hit = 0;
    for w in b {
        let e = m.entry(w).or_default();
        if *e > 0 {
            *e -= 1;
            hit += 1;
        }
    }
    hit
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// (hits, hyp tokens, ref tokens) summed into precision, recall, F1.
fn prf(c: &[(f64, f64, f64)]) -> (f64, f64, f64) {
    let (h, n, r) = c.iter().fold((0.0, 0.0, 0.0), |a, x| (a.0 + x.0, a.1 + x.1, a.2 + x.2));
    let p = if n > 0.0 { h / n } else { 0.0 };
    let q = if r > 0.0 { h / r } else { 0.0 };
    (p, q, if p + q > 0.0 { 2.0 * p * q / (p + q) } else { 0.0 })
}

fn specs(list: &[String]) -> Result<Vec<(String, HashMap<String, String>)>> {
    list.iter()
        .map(|s| {
            let (n, f) = s.split_once('=').context("--hyp NAME=FILE")?;
            let mut m = HashMap::new();
            for r in read_jsonl(Path::new(f))? {
                if let (Some(id), Some(t)) = (r["id"].as_str(), r["text"].as_str()) {
                    m.insert(id.to_string(), t.to_string());
                }
            }
            Ok((n.to_string(), m))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn bag(refs: &Path, hyps: &[String], baseline: Option<String>, fold: bool, n: usize, seed: u64, out: &Path) -> Result<()> {
    let refs = read_jsonl(refs)?;
    let hyps = specs(hyps)?;
    let f = |x: &str| if fold { folded(x) } else { x.to_string() };
    // counts[reader][page] = (hits, hyp tokens, ref tokens)
    let mut counts: Vec<Vec<(f64, f64, f64)>> = vec![vec![]; hyps.len()];
    let mut rows = vec![];
    let mut missing = vec![0usize; hyps.len()];
    for r in &refs {
        let id = r["id"].as_str().unwrap_or("");
        let rt = tokens(&text::normalize(&f(r["ref"].as_str().unwrap_or("")), true));
        for (i, (name, m)) in hyps.iter().enumerate() {
            let h = m.get(id);
            missing[i] += usize::from(h.is_none());
            let ht = tokens(&f(h.map(String::as_str).unwrap_or("")));
            let c = (hits(&rt, &ht) as f64, ht.len() as f64, rt.len() as f64);
            let (p, q, f1) = prf(&[c]);
            rows.push(json!({"kind": "page", "id": id, "reader": name, "hits": c.0, "hyp_words": c.1, "ref_words": c.2, "precision": p, "recall": q, "f1": f1}));
            counts[i].push(c);
        }
    }
    let base = baseline.as_ref().and_then(|b| hyps.iter().position(|h| &h.0 == b));
    let mut rng = Rng(seed);
    let pages = refs.len();
    let mut draws: Vec<Vec<usize>> = Vec::with_capacity(n);
    for _ in 0..n {
        draws.push((0..pages).map(|_| (rng.next() % pages as u64) as usize).collect());
    }
    let stat = |i: usize, d: &[usize]| prf(&d.iter().map(|&k| counts[i][k]).collect::<Vec<_>>());
    for (i, (name, _)) in hyps.iter().enumerate() {
        let (p, q, f1) = prf(&counts[i]);
        let (mut ps, mut qs, mut fs_) = (vec![], vec![], vec![]);
        for d in &draws {
            let (a, b, c) = stat(i, d);
            ps.push(a);
            qs.push(b);
            fs_.push(c);
        }
        let mut per: Vec<f64> = counts[i].iter().map(|c| prf(&[*c]).2).collect();
        let mut row = json!({"kind": "reader", "reader": name, "folded": fold, "pages": pages, "missing": missing[i],
            "precision": p, "precision_lo": pct(&mut ps, 0.025), "precision_hi": pct(&mut ps, 0.975),
            "recall": q, "recall_lo": pct(&mut qs, 0.025), "recall_hi": pct(&mut qs, 0.975),
            "f1": f1, "f1_lo": pct(&mut fs_, 0.025), "f1_hi": pct(&mut fs_, 0.975), "median_page_f1": pct(&mut per, 0.5)});
        if let Some(b) = base.filter(|b| *b != i) {
            let mut ds: Vec<f64> = draws.iter().map(|d| stat(i, d).2 - stat(b, d).2).collect();
            row["f1_diff"] = json!(f1 - prf(&counts[b]).2);
            row["f1_diff_lo"] = json!(pct(&mut ds, 0.025));
            row["f1_diff_hi"] = json!(pct(&mut ds, 0.975));
            row["baseline"] = json!(hyps[b].0);
        }
        rows.push(row);
    }
    write_rows(out, &rows)
}

fn squash(s: &str) -> String {
    text::py_split(s).join(" ")
}

fn vote(refs: &Path, hyps: &[String], out: &Path) -> Result<()> {
    let refs = read_jsonl(refs)?;
    let hyps = specs(hyps)?;
    let mut rows = vec![];
    let mut won = vec![0usize; hyps.len()];
    for r in &refs {
        let id = r["id"].as_str().unwrap_or("");
        let texts: Vec<String> = hyps.iter().map(|(_, m)| m.get(id).cloned().unwrap_or_default()).collect();
        let norm: Vec<String> = texts.iter().map(|t| squash(t)).collect();
        let mut sums = vec![0usize; texts.len()];
        for i in 0..norm.len() {
            for j in i + 1..norm.len() {
                let d = distance::chars(&norm[i], &norm[j]);
                sums[i] += d;
                sums[j] += d;
            }
        }
        // The first minimum wins, so ties go to the first reader given.
        let best = (0..sums.len()).min_by_key(|&i| (sums[i], i)).context("no readers")?;
        won[best] += 1;
        rows.push(json!({"id": id, "text": texts[best], "chosen": hyps[best].0, "sums": sums}));
    }
    let summary: Vec<String> = hyps.iter().zip(&won).map(|((n, _), w)| format!("{n} {w}")).collect();
    eprintln!("chosen: {}", summary.join(", "));
    write_rows(out, &rows)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Prepare { heldout, set, max_regions, per_page, pages, out } => prepare(&heldout, &set, max_regions, per_page, pages, &out),
        Cmd::LineCrops { root, pages, boxes, pad, out } => line_crops(&root, &pages, &boxes, pad, &out),
        Cmd::Bag { refs, hyps, baseline, fold, resamples, seed, out } => bag(&refs, &hyps, baseline, fold, resamples, seed, &out),
        Cmd::Vote { refs, hyps, out } => vote(&refs, &hyps, &out),
    }
}
