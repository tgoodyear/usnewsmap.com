//! Scoring and checks for the GPU studies. Everything an engine returned is read as data.
//!
//! cut     crops from `usnm-boinc qwen local-unit` directories, as PNGs
//! score   pooled CER and WER by group, with page-clustered bootstrap limits and paired differences
//! agree   pairwise symmetric edit rate and word-bag Dice between readers; never accuracy
//! invent  the invented-text check of protocol.md
//! order   rank correlation of a reader's word order against a geometric column order
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use usnm_ocr_survey::score::{cer_wer, read_jsonl, Counts};
use usnm_ocr_survey::{distance, text};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// PNG pages (from opj_decompress) to JPEG at quality 95
    Jpeg {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Cut {
        #[arg(long)]
        units: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Score {
        #[arg(long)]
        refs: PathBuf,
        /// NAME=FILE; repeatable
        #[arg(long = "hyp")]
        hyps: Vec<String>,
        /// Reader the others are paired against
        #[arg(long)]
        baseline: Option<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 2000)]
        resamples: usize,
        #[arg(long, default_value_t = 202611)]
        seed: u64,
    },
    Agree {
        /// JSONL rows {"id","page","group"}
        #[arg(long)]
        items: PathBuf,
        #[arg(long = "hyp")]
        hyps: Vec<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 2000)]
        resamples: usize,
        #[arg(long, default_value_t = 202611)]
        seed: u64,
    },
    Invent {
        #[arg(long)]
        refs: PathBuf,
        #[arg(long = "hyp")]
        hyps: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    Order {
        /// JSONL rows {"id","words":[...]} in geometric column order
        #[arg(long)]
        reference: PathBuf,
        #[arg(long = "hyp")]
        hyps: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn hyp_file(path: &Path) -> Result<HashMap<String, String>> {
    let mut m = HashMap::new();
    for r in read_jsonl(path)? {
        let id = r["id"].as_str().context("output row without id")?.to_string();
        // Non-string text is treated as a missing output, not an error of the scorer.
        if let Some(t) = r["text"].as_str() {
            m.insert(id, t.to_string());
        }
    }
    Ok(m)
}

fn specs(list: &[String]) -> Result<Vec<(String, HashMap<String, String>)>> {
    list.iter()
        .map(|s| {
            let (n, f) = s.split_once('=').context("--hyp NAME=FILE")?;
            Ok((n.to_string(), hyp_file(Path::new(f))?))
        })
        .collect()
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

/// Percentile limits of a ratio statistic, resampling whole pages.
fn boot(pages: &[Vec<(f64, f64)>], n: usize, rng: &mut Rng) -> (f64, f64) {
    let mut stats = Vec::with_capacity(n);
    for _ in 0..n {
        let (mut a, mut b) = (0.0, 0.0);
        for _ in 0..pages.len() {
            for (x, y) in &pages[(rng.next() % pages.len() as u64) as usize] {
                a += x;
                b += y;
            }
        }
        stats.push(if b > 0.0 { a / b } else { 0.0 });
    }
    (pct(&mut stats, 0.025), pct(&mut stats, 0.975))
}

fn paged<T: Clone>(rows: &[(String, T)]) -> Vec<Vec<T>> {
    let mut m: BTreeMap<&str, Vec<T>> = BTreeMap::new();
    for (p, v) in rows {
        m.entry(p).or_default().push(v.clone());
    }
    m.into_values().collect()
}

fn score(refs: &Path, hyps: &[String], baseline: Option<String>, out: &Path, n: usize, seed: u64) -> Result<()> {
    let refs = read_jsonl(refs)?;
    let hyps = specs(hyps)?;
    let mut rng = Rng(seed);
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for r in &refs {
        groups.entry("all".into()).or_default().push(r);
        for k in ["source", "lang"] {
            if let Some(g) = r[k].as_str() {
                groups.entry(format!("{k}:{g}")).or_default().push(r);
            }
        }
    }
    let counts = |name: usize, r: &Value| -> (Counts, bool) {
        let id = r["id"].as_str().unwrap_or("");
        match hyps[name].1.get(id) {
            Some(t) => (cer_wer(r["ref"].as_str().unwrap_or(""), t), true),
            None => (cer_wer(r["ref"].as_str().unwrap_or(""), ""), false),
        }
    };
    let mut rows = vec![];
    for (g, members) in &groups {
        let page_count = members.iter().map(|r| r["page"].as_str().unwrap_or("")).collect::<HashSet<_>>().len();
        let base = baseline.as_ref().and_then(|b| hyps.iter().position(|h| &h.0 == b));
        for (i, (name, _)) in hyps.iter().enumerate() {
            let mut c = Counts { ref_chars: 0, char_edits: 0, ref_words: 0, word_edits: 0 };
            let (mut missing, mut cr, mut wr, mut diff) = (0, vec![], vec![], vec![]);
            for r in members {
                let page = r["page"].as_str().unwrap_or("").to_string();
                let (x, present) = counts(i, r);
                missing += usize::from(!present);
                c.ref_chars += x.ref_chars;
                c.char_edits += x.char_edits;
                c.ref_words += x.ref_words;
                c.word_edits += x.word_edits;
                cr.push((page.clone(), (x.char_edits as f64, x.ref_chars as f64)));
                wr.push((page.clone(), (x.word_edits as f64, x.ref_words as f64)));
                if let Some(b) = base {
                    let (y, _) = counts(b, r);
                    diff.push((page, (x.char_edits as f64 - y.char_edits as f64, x.ref_chars as f64)));
                }
            }
            let (cl, ch) = boot(&paged(&cr), n, &mut rng);
            let (wl, wh) = boot(&paged(&wr), n, &mut rng);
            let mut row = json!({
                "group": g, "reader": name, "items": members.len(), "pages": page_count, "missing": missing,
                "ref_chars": c.ref_chars, "char_edits": c.char_edits, "ref_words": c.ref_words, "word_edits": c.word_edits,
                "cer": c.char_edits as f64 / c.ref_chars.max(1) as f64, "cer_lo": cl, "cer_hi": ch,
                "wer": c.word_edits as f64 / c.ref_words.max(1) as f64, "wer_lo": wl, "wer_hi": wh,
            });
            if base.is_some() && base != Some(i) {
                let (dl, dh) = boot(&paged(&diff), n, &mut rng);
                let d: f64 = diff.iter().map(|x| x.1 .0).sum::<f64>() / c.ref_chars.max(1) as f64;
                row["cer_diff"] = json!(d);
                row["cer_diff_lo"] = json!(dl);
                row["cer_diff_hi"] = json!(dh);
            }
            rows.push(row);
        }
    }
    write_rows(out, &rows)
}

fn write_rows(out: &Path, rows: &[Value]) -> Result<()> {
    let s: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(out, s).with_context(|| format!("writing {}", out.display()))
}

fn tokens(s: &str) -> Vec<String> {
    let n = text::normalize(s, false);
    text::py_split(&n)
        .iter()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn dice(a: &[String], b: &[String]) -> f64 {
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
    if a.len() + b.len() == 0 {
        return 0.0;
    }
    2.0 * hit as f64 / (a.len() + b.len()) as f64
}

fn agree(items: &Path, hyps: &[String], out: &Path, n: usize, seed: u64) -> Result<()> {
    let items = read_jsonl(items)?;
    let hyps = specs(hyps)?;
    let mut rng = Rng(seed);
    let mut rows = vec![];
    for i in 0..hyps.len() {
        for j in i + 1..hyps.len() {
            let (mut e, mut l, mut d, mut both, mut sym) = (0usize, 0usize, 0f64, 0usize, vec![]);
            for it in &items {
                let id = it["id"].as_str().unwrap_or("");
                let (Some(a), Some(b)) = (hyps[i].1.get(id), hyps[j].1.get(id)) else { continue };
                let (na, nb) = (text::normalize(a, false), text::normalize(b, false));
                let x = distance::chars(&na, &nb);
                let y = na.chars().count().max(nb.chars().count());
                e += x;
                l += y;
                d += dice(&tokens(a), &tokens(b));
                both += 1;
                sym.push((it["page"].as_str().unwrap_or("").to_string(), (x as f64, y as f64)));
            }
            if both == 0 {
                continue;
            }
            let (lo, hi) = boot(&paged(&sym), n, &mut rng);
            rows.push(json!({"a": hyps[i].0, "b": hyps[j].0, "items": both, "pages": paged(&sym).len(),
                "symmetric_edit_rate": e as f64 / l.max(1) as f64, "lo": lo, "hi": hi, "mean_dice": d / both as f64,
                "kind": "agreement, not accuracy"}));
        }
    }
    write_rows(out, &rows)
}

const FUZZY: f64 = 0.34;
const RUN: usize = 5;

fn fuzzy(a: &str, b: &str) -> bool {
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if x == y {
        return true;
    }
    let m = x.len().max(y.len());
    (x.len().abs_diff(y.len()) as f64) <= FUZZY * m as f64 && (distance::levenshtein(&x, &y) as f64) <= FUZZY * m as f64
}

/// Longest common subsequence under fuzzy equality; marks which tokens of each side are supported.
fn support(r: &[String], h: &[String]) -> (Vec<bool>, Vec<bool>) {
    let (n, m) = (r.len(), h.len());
    let mut t = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i][j] = if fuzzy(&r[i], &h[j]) { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
        }
    }
    let (mut rs, mut hs, mut i, mut j) = (vec![false; n], vec![false; m], 0, 0);
    while i < n && j < m {
        if fuzzy(&r[i], &h[j]) {
            rs[i] = true;
            hs[j] = true;
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (rs, hs)
}

fn invent(refs: &Path, hyps: &[String], out: &Path) -> Result<()> {
    let refs = read_jsonl(refs)?;
    let hyps = specs(hyps)?;
    let mut rows = vec![];
    for (name, map) in &hyps {
        let mut groups: BTreeMap<String, [usize; 7]> = BTreeMap::new();
        let mut detail = vec![];
        for r in &refs {
            let id = r["id"].as_str().unwrap_or("");
            let Some(hyp) = map.get(id) else { continue };
            let reference = r["ref"].as_str().unwrap_or("");
            let (rt, ht) = (tokens(&text::normalize(reference, true)), tokens(hyp));
            if rt.is_empty() {
                continue;
            }
            let (rs, hs) = support(&rt, &ht);
            let (mut longest, mut cur) = (0, 0);
            for s in &hs {
                cur = if *s { 0 } else { cur + 1 };
                longest = longest.max(cur);
            }
            let chars = |s: &str| text::normalize(s, false).chars().count() as f64;
            let ratio = chars(hyp) / chars(reference).max(1.0);
            let overlap = rs.iter().filter(|x| **x).count() as f64 / rt.len() as f64;
            let run = longest >= RUN;
            let long = ratio > 1.0 / 0.8;
            let short = ratio < 0.8;
            let low = overlap < 0.6;
            let major = run || (long && low);
            for g in ["all".to_string(), format!("source:{}", r["source"].as_str().unwrap_or("?"))] {
                let e = groups.entry(g).or_default();
                e[0] += 1;
                e[1] += usize::from(major);
                e[2] += usize::from(run);
                e[3] += usize::from(long);
                e[4] += usize::from(short);
                e[5] += usize::from(low);
                e[6] += longest;
            }
            if major {
                detail.push(json!({"reader": name, "id": id, "longest_run": longest, "chars_ratio": ratio, "word_overlap": overlap}));
            }
        }
        for (g, e) in groups {
            rows.push(json!({"reader": name, "group": g, "crops": e[0], "major": e[1], "invented_run": e[2],
                "over_length_guard": e[3], "under_length_guard": e[4], "under_word_overlap_guard": e[5]}));
        }
        rows.extend(detail);
    }
    write_rows(out, &rows)
}

/// Kendall's tau over words that occur exactly once on both sides.
fn tau(r: &[String], h: &[String]) -> Option<(f64, usize)> {
    let once = |v: &[String]| {
        let mut m: HashMap<String, (usize, usize)> = HashMap::new();
        for (i, w) in v.iter().enumerate() {
            let e = m.entry(w.clone()).or_insert((0, i));
            e.0 += 1;
        }
        m.into_iter().filter(|(k, v)| v.0 == 1 && k.chars().count() >= 4).map(|(k, v)| (k, v.1)).collect::<HashMap<_, _>>()
    };
    let (a, b) = (once(r), once(h));
    let mut pairs: Vec<(usize, usize)> = a.iter().filter_map(|(k, i)| b.get(k).map(|j| (*i, *j))).collect();
    if pairs.len() < 20 {
        return None;
    }
    pairs.sort_unstable();
    let (mut c, mut d) = (0f64, 0f64);
    for i in 0..pairs.len() {
        for j in i + 1..pairs.len() {
            if pairs[i].1 < pairs[j].1 {
                c += 1.0
            } else {
                d += 1.0
            }
        }
    }
    Some(((c - d) / (c + d), pairs.len()))
}

fn order(reference: &Path, hyps: &[String], out: &Path) -> Result<()> {
    let refs = read_jsonl(reference)?;
    let hyps = specs(hyps)?;
    let mut rows = vec![];
    for r in &refs {
        let id = r["id"].as_str().unwrap_or("");
        let words: Vec<String> = r["words"].as_array().context("words")?.iter().filter_map(|w| w.as_str()).map(|w| w.to_lowercase()).collect();
        for (name, map) in &hyps {
            if let Some(t) = map.get(id) {
                let (v, n) = tau(&words, &tokens(t)).map_or((None, 0), |(v, n)| (Some(v), n));
                rows.push(json!({"id": id, "reader": name, "tau": v, "matched": n}));
            }
        }
    }
    write_rows(out, &rows)
}

fn cut(units: &Path, out: &Path) -> Result<()> {
    fs::create_dir_all(out)?;
    let mut rows = vec![];
    let mut dirs: Vec<_> = fs::read_dir(units)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        let Ok(raw) = fs::read(dir.join("qwen.json")) else { continue };
        let job: Value = serde_json::from_slice(&raw)?;
        let page = image::ImageReader::open(dir.join("page.jpg"))?.with_guessed_format()?.decode()?;
        // local units all carry doc_id "local"; the directory names the page.
        let doc = dir.file_name().context("unit name")?.to_string_lossy().into_owned();
        let Some(crops) = job["crops"].as_array() else { bail!("no crops in {}", dir.display()) };
        for c in crops {
            let b: Vec<u32> = c["box"].as_array().context("box")?.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).collect();
            let name = format!("{doc}.{}", c["id"].as_str().context("id")?);
            let file = format!("{name}.png");
            page.crop_imm(b[0], b[1], b[2] - b[0], b[3] - b[1]).save(out.join(&file))?;
            rows.push(json!({"id": name, "page": doc, "file": file, "loc_text": c["loc_text"], "stratum": c["stratum"], "box": c["box"]}));
        }
    }
    write_rows(&out.join("crops.jsonl"), &rows)
}

fn jpeg(input: &Path, out: &Path) -> Result<()> {
    let page = image::ImageReader::open(input)?.with_guessed_format()?.decode()?.into_luma8();
    let file = std::io::BufWriter::new(fs::File::create(out)?);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 95);
    enc.encode_image(&page)?;
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Jpeg { input, out } => jpeg(&input, &out),
        Cmd::Cut { units, out } => cut(&units, &out),
        Cmd::Score { refs, hyps, baseline, out, resamples, seed } => score(&refs, &hyps, baseline, &out, resamples, seed),
        Cmd::Agree { items, hyps, out, resamples, seed } => agree(&items, &hyps, &out, resamples, seed),
        Cmd::Invent { refs, hyps, out } => invent(&refs, &hyps, &out),
        Cmd::Order { reference, hyps, out } => order(&reference, &hyps, &out),
    }
}
