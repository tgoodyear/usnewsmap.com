//! The Yiddish test set (November 2026): the Jochre Yiddish corpus's test split, read by Qwen through
//! the native `usnm_qwen` app, by Tesseract and by Jochre 3, and scored the way the corpus paper
//! (arXiv:2501.08442) scores it, plus our own folded CER. Also agreement on newspaper crops that have
//! no transcription. Everything an engine returned is read as data.
//!
//! units    `usnm_qwen` bundle units (qwen.json + page.jpg) of ALTO line crops, for one request version
//! collect  readings, generation records and timings out of finished units, as JSONL
//! yivo     the corpus's YIVO simplification, checked against its own `_simplified.txt` files
//! score    per-page and per-line CER against the gold text, with page-clustered bootstrap limits
//! agree    pairwise distance between readers on untranscribed crops; agreement, never accuracy
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;
use usnm_ocr_contract::quality::{BundleSlice, Input};
use usnm_ocr_contract::qwen::{BundlePage, Crop, Engine, Job, VERSION};
use usnm_ocr_survey::score::read_jsonl;
use usnm_ocr_survey::text;

use crate::{pct, write_rows, Rng};

// ---------------------------------------------------------------- ALTO lines

pub struct AltoLine {
    pub rect: [u32; 4],
    pub text: String,
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')?;
    Some(unescape(&tag[i..i + j]))
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(j) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let ent = &tail[1..j];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=j]),
        }
        rest = &tail[j + 1..];
    }
    out.push_str(rest);
    out
}

/// The text lines of a Jochre ALTO 4 file in file order, with their boxes clamped to the image and
/// their text as the corpus's `_alto4.txt` has it: `String` contents, `SP` as one space, `HYP` contents.
pub fn alto_lines(xml: &str, width: u32, height: u32) -> Vec<AltoLine> {
    let mut out = vec![];
    for part in xml.split("<TextLine").skip(1) {
        let head = &part[..part.find('>').unwrap_or(0)];
        let num = |n: &str| attr(head, n).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let (x, y, w, h) = (num("HPOS"), num("VPOS"), num("WIDTH"), num("HEIGHT"));
        let body = &part[..part.find("</TextLine").unwrap_or(part.len())];
        let mut t = String::new();
        for el in body.split('<').skip(1) {
            let tag = &el[..el.find('>').unwrap_or(el.len())];
            if tag.starts_with("String ") || tag.starts_with("String\t") {
                t.push_str(&attr(tag, "CONTENT").unwrap_or_default());
            } else if tag.starts_with("SP ") || tag.starts_with("SP/") || tag == "SP" {
                t.push(' ');
            } else if tag.starts_with("HYP ") {
                t.push_str(&attr(tag, "CONTENT").unwrap_or_default());
            }
        }
        let clamp = |v: f64, max: u32| (v.max(0.0).round() as u32).min(max);
        let rect = [clamp(x, width), clamp(y, height), clamp(x + w, width), clamp(y + h, height)];
        out.push(AltoLine { rect, text: t });
    }
    out
}

// ---------------------------------------------------------------- units

#[allow(clippy::too_many_arguments)]
pub fn units(pages: &Path, list: &Path, engine: &Path, prompt_version: u32, group: &str, max_crops: usize, timeout_secs: u32, out: &Path) -> Result<()> {
    let engine: Engine = serde_json::from_slice(&fs::read(engine)?).context("--engine is not an Engine")?;
    ensure!(max_crops >= 1 && max_crops <= 256, "--max-crops 1 to 256");
    fs::create_dir_all(out)?;
    let ids: Vec<String> = fs::read_to_string(list)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    // (doc_id, page id, png bytes, crops)
    let mut docs: Vec<(String, String, Vec<u8>, Vec<Crop>)> = vec![];
    let mut refs = vec![];
    for id in &ids {
        let png = fs::read(pages.join(format!("{id}.png")))?;
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(&png)).with_guessed_format()?.into_dimensions()?;
        let xml = fs::read_to_string(pages.join(format!("{id}_alto4.xml")))?;
        let lines = alto_lines(&xml, w, h);
        let mut n = 0usize;
        for (part, chunk) in lines.chunks(64).enumerate() {
            let doc = if part == 0 { id.clone() } else { format!("{id}-p{}", part + 1) };
            let mut crops = vec![];
            for l in chunk {
                n += 1;
                let cid = format!("l{n:03}");
                if l.rect[0] >= l.rect[2] || l.rect[1] >= l.rect[3] {
                    refs.push(json!({"id": format!("{doc}:{cid}"), "page": id, "line": n, "ref": l.text, "box": l.rect, "skipped": "empty box"}));
                    continue;
                }
                refs.push(json!({"id": format!("{doc}:{cid}"), "page": id, "line": n, "ref": l.text, "box": l.rect}));
                // No reference text enters the unit: the app never sends loc_text to the model, and the
                // gold stays out of the workunit all the same.
                crops.push(Crop { rect: l.rect, id: cid, loc_text: String::new(), stratum: "line".into() });
            }
            if !crops.is_empty() {
                docs.push((doc, id.clone(), png.clone(), crops));
            }
        }
    }
    // Pack whole page chunks into bundles of at most max_crops crops and 32 pages, in list order.
    let mut bundles: Vec<Vec<usize>> = vec![vec![]];
    let mut count = 0;
    for (i, d) in docs.iter().enumerate() {
        let cur = bundles.last().unwrap();
        if !cur.is_empty() && (count + d.3.len() > max_crops || cur.len() == 32) {
            bundles.push(vec![]);
            count = 0;
        }
        bundles.last_mut().unwrap().push(i);
        count += d.3.len();
    }
    let mut manifest = vec![];
    for (b, members) in bundles.iter().enumerate() {
        let name = format!("b{b:03}");
        let mut blob = vec![];
        let mut bpages = vec![];
        let mut crops = vec![];
        for &i in members {
            let (doc, page, png, cs) = &docs[i];
            let slice = BundleSlice::of(blob.len() as u64, png, &format!("jochre-yiddish-corpus/test/{page}.png"));
            blob.extend_from_slice(png);
            for c in cs {
                crops.push(Crop { rect: c.rect, id: format!("{doc}:{}", c.id), loc_text: String::new(), stratum: c.stratum.clone() });
            }
            bpages.push(BundlePage { crops: cs.clone(), doc_id: doc.clone(), group: group.into(), input: slice, language: "yid".into() });
        }
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: timeout_secs,
            crops,
            doc_id: format!("{group}-{name}"),
            engine: engine.clone(),
            gpu_timeout_secs: timeout_secs,
            group: group.into(),
            input: Input::of(&blob, &format!("jochre-yiddish-corpus/test/bundle-{name}")),
            language: "yid".into(),
            max_tokens_per_1000px2: 4,
            pages: bpages,
            prompt_version,
        };
        job.id = job.identity()?;
        job.validate()?;
        let dir = out.join(&name);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("qwen.json"), serde_json::to_vec_pretty(&job)?)?;
        fs::write(dir.join("page.jpg"), &blob)?;
        manifest.push(json!({"unit": name, "job": job.id, "crops": job.crops.len(), "pages": members.len(), "bytes": blob.len(), "prompt_version": prompt_version}));
    }
    write_rows(&out.join("units.jsonl"), &manifest)?;
    write_rows(&out.join("refs.jsonl"), &refs)?;
    eprintln!("{} units, {} crops", manifest.len(), docs.iter().map(|d| d.3.len()).sum::<usize>());
    Ok(())
}

/// The same line crops the units hold, as PNG files `<doc>_<crop>.png`, cut the way the app cuts them
/// (`crop_imm` on the decoded page, PNG out), for engines that read crops outside the app.
pub fn cut_lines(pages: &Path, refs: &Path, out: &Path) -> Result<()> {
    fs::create_dir_all(out)?;
    let mut current: Option<(String, image::DynamicImage)> = None;
    let mut rows = vec![];
    for r in read_jsonl(refs)? {
        if r.get("skipped").is_some() {
            continue;
        }
        let page = r["page"].as_str().context("page")?.to_string();
        if current.as_ref().is_none_or(|(p, _)| p != &page) {
            let img = image::ImageReader::open(pages.join(format!("{page}.png")))?.with_guessed_format()?.decode()?;
            current = Some((page.clone(), img));
        }
        let img = &current.as_ref().unwrap().1;
        let b: Vec<u32> = r["box"].as_array().context("box")?.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).collect();
        let id = r["id"].as_str().context("id")?;
        let file = format!("{}.png", id.replace(':', "_"));
        img.crop_imm(b[0], b[1], b[2] - b[0], b[3] - b[1]).save(out.join(&file))?;
        rows.push(json!({"id": id, "page": page, "file": file}));
    }
    write_rows(&out.join("crops.jsonl"), &rows)
}

/// Silver-set units: the selected crop of each production unit (`selection.jsonl` rows {doc, crop}),
/// same box, same page JPEG and same token budget, rebundled for one request version. Also the crops
/// as PNG for the engines that read outside the app, and `items.jsonl` with LoC's text for each crop.
#[allow(clippy::too_many_arguments)]
pub fn silver_units(selection: &Path, src: &Path, engine: &Path, prompt_version: u32, pages_per_unit: usize, timeout_secs: u32, out: &Path, crops_out: Option<&Path>) -> Result<()> {
    let engine: Engine = serde_json::from_slice(&fs::read(engine)?)?;
    fs::create_dir_all(out)?;
    let sel = read_jsonl(selection)?;
    let mut items = vec![];
    let mut manifest = vec![];
    if let Some(c) = crops_out {
        fs::create_dir_all(c)?;
    }
    for (b, chunk) in sel.chunks(pages_per_unit.clamp(1, 32)).enumerate() {
        let name = format!("s{b:03}");
        let (mut blob, mut bpages, mut crops) = (vec![], vec![], vec![]);
        let mut per = None;
        for s in chunk {
            let doc = s["doc"].as_str().context("doc")?;
            let cid = s["crop"].as_str().context("crop")?;
            let job: Job = serde_json::from_slice(&fs::read(src.join(doc).join("qwen.json"))?)?;
            ensure!(per.is_none_or(|p| p == job.max_tokens_per_1000px2), "mixed token budgets");
            per = Some(job.max_tokens_per_1000px2);
            let jpg = fs::read(src.join(doc).join("page.jpg"))?;
            ensure!(job.input.matches(&jpg), "{doc}: page.jpg does not match its job");
            let c = job.crops.iter().find(|c| c.id == cid).with_context(|| format!("{doc}: no crop {cid}"))?.clone();
            items.push(json!({"id": format!("{doc}:{cid}"), "page": doc, "title": s["title"], "stratum": c.stratum, "box": c.rect, "loc_text": c.loc_text, "max_tokens": job.max_tokens(&c)}));
            if let Some(dir) = crops_out {
                let img = image::load_from_memory(&jpg)?;
                let [x0, y0, x1, y1] = c.rect;
                img.crop_imm(x0, y0, x1 - x0, y1 - y0).save(dir.join(format!("{doc}_{cid}.png")))?;
            }
            let slice = BundleSlice::of(blob.len() as u64, &jpg, &job.input.source);
            blob.extend_from_slice(&jpg);
            let mut pc = c.clone();
            pc.loc_text = String::new();
            crops.push(Crop { id: format!("{doc}:{cid}"), ..pc.clone() });
            bpages.push(BundlePage { crops: vec![pc], doc_id: doc.into(), group: "yid-silver".into(), input: slice, language: "yid".into() });
        }
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: timeout_secs,
            crops,
            doc_id: format!("yid-silver-{name}"),
            engine: engine.clone(),
            gpu_timeout_secs: timeout_secs,
            group: "yid-silver".into(),
            input: Input::of(&blob, &format!("yid-silver-202611/bundle-{name}")),
            language: "yid".into(),
            max_tokens_per_1000px2: per.unwrap_or(4),
            pages: bpages,
            prompt_version,
        };
        job.id = job.identity()?;
        job.validate()?;
        let dir = out.join(&name);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("qwen.json"), serde_json::to_vec_pretty(&job)?)?;
        fs::write(dir.join("page.jpg"), &blob)?;
        manifest.push(json!({"unit": name, "job": job.id, "crops": job.crops.len(), "bytes": blob.len(), "prompt_version": prompt_version}));
    }
    write_rows(&out.join("units.jsonl"), &manifest)?;
    write_rows(&out.join("items.jsonl"), &items)
}

// ---------------------------------------------------------------- collect

/// The generation record the app wrote for each crop, from the `generation_v1/<crop>` sections of diag.txt.
fn generation_sections(diag: &str) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    let mut lines = diag.lines().peekable();
    while let Some(l) = lines.next() {
        if let Some(crop) = l.strip_prefix("== generation_v1/") {
            if let Some(body) = lines.peek() {
                if let Ok(v) = serde_json::from_str::<Value>(body) {
                    m.insert(crop.to_string(), v);
                }
            }
        }
    }
    m
}

pub fn collect(units: &Path, out: &Path, summary: &Path) -> Result<()> {
    let mut rows = vec![];
    let mut sums = vec![];
    let mut dirs: Vec<_> = fs::read_dir(units)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("qwen.json").is_file()).collect();
    dirs.sort();
    for dir in dirs {
        let unit = dir.file_name().context("unit")?.to_string_lossy().into_owned();
        let job: Job = serde_json::from_slice(&fs::read(dir.join("qwen.json"))?)?;
        let Ok(res) = fs::read(dir.join("result.json")) else {
            sums.push(json!({"unit": unit, "missing": true}));
            continue;
        };
        let res: Value = serde_json::from_slice(&res)?;
        let readings: Value = serde_json::from_slice(&fs::read(dir.join("readings.json"))?)?;
        let diag = fs::read_to_string(dir.join("diag.txt")).unwrap_or_default();
        let mut gens = generation_sections(&diag);
        // The run kept the app's final checkpoint (an LD_PRELOAD shim renames it instead of deleting
        // it): every crop's generation record, where diag.txt holds only what fits in 64 KB.
        if let Ok(cp) = fs::read(dir.join("qwen_checkpoint.final.json")) {
            let cp: Value = serde_json::from_slice(&cp)?;
            if let Some(m) = cp["generation_metadata"].as_object() {
                for (k, v) in m {
                    if let Some(v) = v.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()) {
                        gens.insert(k.clone(), v);
                    }
                }
            }
        }
        let by_id: HashMap<&str, &Value> = readings["readings"].as_array().map(|a| a.iter().filter_map(|r| Some((r["id"].as_str()?, r))).collect()).unwrap_or_default();
        let mut with_record = 0;
        for c in &job.crops {
            let max = job.max_tokens(c);
            let Some(r) = by_id.get(c.id.as_str()) else {
                rows.push(json!({"id": c.id, "unit": unit, "missing": true, "max_tokens": max}));
                continue;
            };
            let g = gens.get(&c.id);
            with_record += usize::from(g.is_some());
            let flags: Vec<String> = g.and_then(|g| g["generation_flags"].as_array()).map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let has = |f: &str| flags.iter().any(|x| x == f);
            rows.push(json!({
                "id": c.id, "unit": unit, "text": r["text"], "ms": r["ms"], "tokens": r["tokens"], "max_tokens": max,
                "record": g.is_some(), "flags": flags,
                "loop_at_budget": has("repetition_loop_at_token_budget"), "tail_repetition": has("tail_periodic_repetition"),
                "guard_cut": has("repetition_loop_guard_cut"), "budget_exhausted": has("completion_token_budget_exhausted"),
                "guard_stopped": g.and_then(|g| g["loop_guard"]["stopped_generation"].as_bool()),
                "finish_reason": g.map(|g| g["finish_reason"].clone()),
            }));
        }
        sums.push(json!({
            "unit": unit, "job": job.id, "prompt_version": job.prompt_version, "crops": job.crops.len(), "records": with_record,
            "outcome": res["outcome"], "device": res["device"], "model": res["model"], "read": res["read"],
            "engine_ms": res["engine_ms"], "load_ms": res["load_ms"], "gpu_mem_peak_mib": res["gpu_mem_peak_mib"], "peak_rss_kib": res["peak_rss_kib"],
            "diag_cut": diag.contains("[diagnostics cut at the size limit]"),
            "server_version": readings["diag"].get("server_version").cloned().unwrap_or(Value::Null),
        }));
    }
    write_rows(out, &rows)?;
    write_rows(summary, &sums)
}

// ---------------------------------------------------------------- YIVO simplification

fn is_dagesh_base(c: char) -> bool {
    matches!(c, '\u{05D5}' | '\u{05DB}' | '\u{05E4}' | '\u{05D1}' | '\u{05EA}')
}

/// One regex pass `(?<!prev)target` -> "": `keep(prev)` says when the mark stays. Look-behind sees the
/// pass's input, as Java's `replaceAll` does.
fn drop_mark(s: &[char], target: char, keep: impl Fn(Option<char>) -> bool) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    for (i, &c) in s.iter().enumerate() {
        if c == target && !keep(if i == 0 { None } else { Some(s[i - 1]) }) {
            continue;
        }
        out.push(c);
    }
    out
}

/// `(ab)(?!not)` -> `rep`, left to right, without overlap.
fn pair_unless(s: &[char], a: char, b: char, not: char, rep: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if i + 1 < s.len() && s[i] == a && s[i + 1] == b && s.get(i + 2) != Some(&not) {
            out.extend_from_slice(rep);
            i += 2;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// Jochre 3's `YiddishTextSimpifier(replaceNonHebrewAlphabets = false)` at jochre3-ocr commit 023a30fd,
/// the normalization its `YiddishTextEvaluator` applies to both sides: NFD, then the same passes in the
/// same order. Its `verticalBar` pattern is `|`, which only matches the empty string, so it changes
/// nothing there and nothing here.
pub fn yivo_simplify(text: &str) -> String {
    let mut s: Vec<char> = text.nfd().collect();
    // nonYivoNikud
    s.retain(|c| !matches!(c, '\u{05B5}' | '\u{05C1}' | '\u{05B6}' | '\u{05B1}' | '\u{05B2}' | '\u{05B9}' | '\u{05BB}' | '\u{05B3}' | '\u{05B0}'));
    // nonYivoKomets
    s = drop_mark(&s, '\u{05B8}', |p| p == Some('\u{05D0}'));
    // pasekhTsveyYudnSeparate: יי + pasekh -> ײ + pasekh
    let mut t = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if i + 2 < s.len() && s[i] == '\u{05D9}' && s[i + 1] == '\u{05D9}' && s[i + 2] == '\u{05B7}' {
            t.extend_from_slice(&['\u{05F2}', '\u{05B7}']);
            i += 3;
        } else {
            t.push(s[i]);
            i += 1;
        }
    }
    s = t;
    s = pair_unless(&s, '\u{05D9}', '\u{05D9}', '\u{05B4}', &['\u{05F2}']);
    s = pair_unless(&s, '\u{05D5}', '\u{05D5}', '\u{05BC}', &['\u{05F0}']);
    s = pair_unless(&s, '\u{05D5}', '\u{05D9}', '\u{05B4}', &['\u{05F1}']);
    s = drop_mark(&s, '\u{05B7}', |p| matches!(p, Some('\u{05D0}' | '\u{05F2}')));
    s = drop_mark(&s, '\u{05B4}', |p| p == Some('\u{05D9}'));
    s = drop_mark(&s, '\u{05BC}', |p| p.is_some_and(is_dagesh_base));
    s = drop_mark(&s, '\u{05BF}', |p| matches!(p, Some('\u{05D1}' | '\u{05E4}' | '\u{05DB}')));
    s = drop_mark(&s, '\u{05C2}', |p| p == Some('\u{05E9}'));
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    for &c in &s {
        out.push(match c {
            '-' | '\u{2E17}' => '\u{05BE}',
            '\u{1D199}' | '\u{2190}' | '\u{2013}' | '\u{2014}' => '\u{2014}',
            '\'' | '\u{201B}' | '\u{2019}' | '\u{05F3}' => '\u{2019}',
            c => c,
        });
    }
    // nonStandardDoubleQuote: ["“״] | ‛‛ | ’’ | '' -> “ (after the single-quote pass, only ’’ can still pair)
    let mut t = Vec::with_capacity(out.len());
    let mut i = 0;
    while i < out.len() {
        let c = out[i];
        if matches!(c, '"' | '\u{201C}' | '\u{05F4}') {
            t.push('\u{201C}');
            i += 1;
        } else if i + 1 < out.len() && out[i + 1] == c && matches!(c, '\u{201B}' | '\u{2019}' | '\'') {
            t.push('\u{201C}');
            i += 2;
        } else {
            t.push(c);
            i += 1;
        }
    }
    // nonStandardLowerDoubleQuote: ,, | ‚‚ -> „
    let mut u = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if i + 1 < t.len() && t[i] == t[i + 1] && matches!(t[i], ',' | '\u{201A}') {
            u.push('\u{201E}');
            i += 2;
        } else {
            u.push(t[i]);
            i += 1;
        }
    }
    u.into_iter().map(|c| if matches!(c, '\u{25BC}' | '\u{25E6}' | '\u{2116}' | '\u{2042}') { '\u{2022}' } else { c }).collect()
}

/// A file's text as the evaluator reads it (lines joined by `\n`, so no final newline), simplified,
/// with paragraph breaks collapsed (`ignoreParagraphs`, as the paper's evaluation did).
pub fn paper_text(raw: &str) -> String {
    let mut lines: Vec<&str> = raw.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let s = yivo_simplify(&lines.join("\n"));
    let mut out = String::with_capacity(s.len());
    let mut nl = 0;
    for c in s.chars() {
        if c == '\n' {
            nl += 1;
            continue;
        }
        if nl > 0 {
            out.push('\n');
            nl = 0;
        }
        out.push(c);
    }
    if nl > 0 {
        out.push('\n');
    }
    out
}

/// Hebrew points and cantillation out, U+0591 to U+05C7 less the punctuation there: the same set as
/// `usnm-boinc`'s `qwen::consensus::without_hebrew_points`.
pub fn without_hebrew_points(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '\u{0591}'..='\u{05BD}' | '\u{05BF}' | '\u{05C1}' | '\u{05C2}' | '\u{05C4}' | '\u{05C5}' | '\u{05C7}')).collect()
}

/// Our raw folded text: the gold set's normalization (NFC, line-end hyphens joined, whitespace
/// collapsed) with Hebrew points left out.
pub fn folded(s: &str, truth: bool) -> String {
    without_hebrew_points(&text::normalize(s, truth))
}

pub fn yivo_check(pages: &Path, list: &Path, out: &Path) -> Result<()> {
    let mut rows = vec![];
    let (mut same, mut n) = (0, 0);
    for id in fs::read_to_string(list)?.lines().filter(|l| !l.trim().is_empty()) {
        let raw = fs::read_to_string(pages.join(format!("{id}_alto4.txt")))?;
        let theirs = fs::read_to_string(pages.join(format!("{id}_alto4_simplified.txt")))?;
        let mine = yivo_simplify(&raw);
        let (a, b): (Vec<char>, Vec<char>) = (mine.trim_end().chars().collect(), theirs.trim_end().nfd().collect::<String>().chars().collect());
        let e = edits(&a, &b);
        n += 1;
        same += usize::from(e.total() == 0);
        rows.push(json!({"page": id, "edits_vs_corpus_simplified": e.total(), "chars": b.len()}));
    }
    eprintln!("{same} of {n} pages equal to the corpus's own _simplified.txt (both NFD)");
    write_rows(out, &rows)
}

// ---------------------------------------------------------------- edit distance with operations

#[derive(Clone, Copy, Default, Debug)]
pub struct Ops {
    pub sub: u32,
    pub ins: u32,
    pub del: u32,
}
impl Ops {
    pub fn total(&self) -> u32 {
        self.sub + self.ins + self.del
    }
}

/// Levenshtein distance from `hyp` to `reference` with the operations of one optimal path: `del` is a
/// reference character the reading left out, `ins` one it added.
pub fn edits(hyp: &[char], reference: &[char]) -> Ops {
    let m = reference.len();
    let mut prev: Vec<Ops> = (0..=m).map(|j| Ops { del: j as u32, ..Default::default() }).collect();
    let mut cur = vec![Ops::default(); m + 1];
    for (i, &h) in hyp.iter().enumerate() {
        cur[0] = Ops { ins: i as u32 + 1, ..Default::default() };
        for j in 1..=m {
            let diag = prev[j - 1];
            let sub = if reference[j - 1] == h { diag } else { Ops { sub: diag.sub + 1, ..diag } };
            let up = Ops { ins: prev[j].ins + 1, ..prev[j] };
            let left = Ops { del: cur[j - 1].del + 1, ..cur[j - 1] };
            let mut best = sub;
            if left.total() < best.total() {
                best = left;
            }
            if up.total() < best.total() {
                best = up;
            }
            cur[j] = best;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

// ---------------------------------------------------------------- scoring

struct PageScore {
    yivo: Ops,
    yivo_len: usize,
    raw: Ops,
    raw_len: usize,
}

fn boot_mean(vals: &[f64], n: usize, rng: &mut Rng) -> (f64, f64) {
    let mut stats = Vec::with_capacity(n);
    for _ in 0..n {
        let mut s = 0.0;
        for _ in 0..vals.len() {
            s += vals[(rng.next() % vals.len() as u64) as usize];
        }
        stats.push(s / vals.len() as f64);
    }
    (pct(&mut stats, 0.025), pct(&mut stats, 0.975))
}

fn boot_ratio(pairs: &[(f64, f64)], n: usize, rng: &mut Rng) -> (f64, f64) {
    let mut stats = Vec::with_capacity(n);
    for _ in 0..n {
        let (mut a, mut b) = (0.0, 0.0);
        for _ in 0..pairs.len() {
            let p = pairs[(rng.next() % pairs.len() as u64) as usize];
            a += p.0;
            b += p.1;
        }
        stats.push(if b > 0.0 { a / b } else { 0.0 });
    }
    (pct(&mut stats, 0.025), pct(&mut stats, 0.975))
}

fn spec(s: &str) -> Result<(String, Vec<Value>)> {
    let (n, f) = s.split_once('=').context("NAME=FILE")?;
    Ok((n.to_string(), read_jsonl(Path::new(f))?))
}

#[allow(clippy::too_many_arguments)]
pub fn score(pages: &Path, list: &Path, refs: &Path, page_hyps: &[String], line_hyps: &[String], group_file: Option<&Path>, out: &Path, n: usize, seed: u64) -> Result<()> {
    fs::create_dir_all(out)?;
    let ids: Vec<String> = fs::read_to_string(list)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    let gold: BTreeMap<&str, String> = ids.iter().map(|id| Ok((id.as_str(), fs::read_to_string(pages.join(format!("{id}_alto4.txt")))?))).collect::<Result<_>>()?;
    let listed: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let refs: Vec<Value> = read_jsonl(refs)?.into_iter().filter(|r| r["page"].as_str().is_some_and(|p| listed.contains(p))).collect();
    let groups: HashMap<String, String> = match group_file {
        Some(f) => fs::read_to_string(f)?.lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        None => HashMap::new(),
    };
    // Page texts per reader. Line readers are joined line by line in ALTO order, so they get the
    // corpus's line segmentation and reading order for free: their page CER measures recognition only.
    let mut readers: Vec<(String, &'static str, HashMap<String, String>)> = vec![];
    let mut line_maps: Vec<(String, HashMap<String, Value>)> = vec![];
    for s in page_hyps {
        let (name, rows) = spec(s)?;
        let m = rows.iter().filter_map(|r| Some((r["page"].as_str()?.to_string(), r["text"].as_str()?.to_string()))).collect();
        readers.push((name, "page", m));
    }
    for s in line_hyps {
        let (name, rows) = spec(s)?;
        let by: HashMap<String, Value> = rows.into_iter().filter_map(|r| Some((r["id"].as_str()?.to_string(), r))).collect();
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for r in &refs {
            let id = r["id"].as_str().unwrap_or("");
            let page = r["page"].as_str().unwrap_or("").to_string();
            let t = by.get(id).and_then(|x| x["text"].as_str()).unwrap_or("").replace('\n', " ");
            m.entry(page).or_default().push(t);
        }
        readers.push((name.clone(), "line", m.into_iter().map(|(k, v)| (k, v.join("\n"))).collect()));
        line_maps.push((name, by));
    }
    let mut rng = Rng(seed);
    let mut detail = vec![];
    let mut scores: Vec<Vec<PageScore>> = vec![];
    for (name, kind, m) in &readers {
        let mut v = vec![];
        for id in &ids {
            let g = &gold[id.as_str()];
            let h = m.get(id).map(String::as_str).unwrap_or("");
            let (gy, hy): (Vec<char>, Vec<char>) = (paper_text(g).chars().collect(), paper_text(h).chars().collect());
            let (gr, hr): (Vec<char>, Vec<char>) = (folded(g, true).chars().collect(), folded(h, false).chars().collect());
            let ps = PageScore { yivo: edits(&hy, &gy), yivo_len: gy.len(), raw: edits(&hr, &gr), raw_len: gr.len() };
            detail.push(json!({"reader": name, "kind": kind, "page": id, "group": groups.get(id),
                "present": m.contains_key(id), "yivo_cer": ps.yivo.total() as f64 / ps.yivo_len.max(1) as f64, "yivo_chars": ps.yivo_len,
                "yivo_sub": ps.yivo.sub, "yivo_ins": ps.yivo.ins, "yivo_del": ps.yivo.del,
                "raw_cer": ps.raw.total() as f64 / ps.raw_len.max(1) as f64, "raw_chars": ps.raw_len, "raw_edits": ps.raw.total()}));
            v.push(ps);
        }
        scores.push(v);
    }
    write_rows(&out.join("pages.jsonl"), &detail)?;
    let mut set_names: Vec<String> = vec!["all".into()];
    let mut gs: Vec<String> = groups.values().cloned().collect();
    gs.sort();
    gs.dedup();
    set_names.extend(gs.into_iter().map(|g| format!("group:{g}")));
    let mut rows = vec![];
    for set in &set_names {
        let idx: Vec<usize> = ids.iter().enumerate().filter(|(_, id)| set == "all" || groups.get(*id).map(|g| format!("group:{g}")).as_deref() == Some(set)).map(|(i, _)| i).collect();
        for (r, (name, kind, m)) in readers.iter().enumerate() {
            let s: Vec<&PageScore> = idx.iter().map(|&i| &scores[r][i]).collect();
            let per: Vec<f64> = s.iter().map(|p| p.yivo.total() as f64 / p.yivo_len.max(1) as f64).collect();
            let yp: Vec<(f64, f64)> = s.iter().map(|p| (p.yivo.total() as f64, p.yivo_len as f64)).collect();
            let rp: Vec<(f64, f64)> = s.iter().map(|p| (p.raw.total() as f64, p.raw_len as f64)).collect();
            let dp: Vec<(f64, f64)> = s.iter().map(|p| (p.yivo.del as f64, p.yivo_len as f64)).collect();
            let sum = |f: &dyn Fn(&PageScore) -> f64| s.iter().map(|p| f(p)).sum::<f64>();
            let (ml, mh) = boot_mean(&per, n, &mut rng);
            let (yl, yh) = boot_ratio(&yp, n, &mut rng);
            let (rl, rh) = boot_ratio(&rp, n, &mut rng);
            let (dl, dh) = boot_ratio(&dp, n, &mut rng);
            let mut med = per.clone();
            let ylen = sum(&|p| p.yivo_len as f64);
            rows.push(json!({"set": set, "reader": name, "kind": kind, "pages": s.len(), "pages_present": idx.iter().filter(|&&i| m.contains_key(&ids[i])).count(),
                "paper_mean_page_cer": per.iter().sum::<f64>() / per.len().max(1) as f64, "paper_mean_lo": ml, "paper_mean_hi": mh, "median_page_cer": if med.is_empty() { 0.0 } else { pct(&mut med, 0.5) },
                "yivo_pooled_cer": sum(&|p| p.yivo.total() as f64) / ylen.max(1.0), "yivo_pooled_lo": yl, "yivo_pooled_hi": yh,
                "deletion_rate": sum(&|p| p.yivo.del as f64) / ylen.max(1.0), "deletion_lo": dl, "deletion_hi": dh,
                "insertion_rate": sum(&|p| p.yivo.ins as f64) / ylen.max(1.0), "substitution_rate": sum(&|p| p.yivo.sub as f64) / ylen.max(1.0),
                "raw_pooled_cer": sum(&|p| p.raw.total() as f64) / sum(&|p| p.raw_len as f64).max(1.0), "raw_lo": rl, "raw_hi": rh}));
        }
        // Paired differences, a minus b, resampling pages.
        for a in 0..readers.len() {
            for b in 0..readers.len() {
                if a == b {
                    continue;
                }
                let d: Vec<f64> = idx.iter().map(|&i| {
                    let (x, y) = (&scores[a][i], &scores[b][i]);
                    x.yivo.total() as f64 / x.yivo_len.max(1) as f64 - y.yivo.total() as f64 / y.yivo_len.max(1) as f64
                }).collect();
                let rd: Vec<(f64, f64)> = idx.iter().map(|&i| (scores[a][i].raw.total() as f64 - scores[b][i].raw.total() as f64, scores[a][i].raw_len as f64)).collect();
                let (l, h) = boot_mean(&d, n, &mut rng);
                let (rl, rh) = boot_ratio(&rd, n, &mut rng);
                let rsum: f64 = rd.iter().map(|x| x.0).sum::<f64>() / rd.iter().map(|x| x.1).sum::<f64>().max(1.0);
                rows.push(json!({"set": set, "diff": format!("{} - {}", readers[a].0, readers[b].0), "a": readers[a].0, "b": readers[b].0,
                    "paper_mean_diff": d.iter().sum::<f64>() / d.len().max(1) as f64, "lo": l, "hi": h,
                    "raw_pooled_diff": rsum, "raw_lo": rl, "raw_hi": rh}));
            }
        }
    }
    write_rows(&out.join("summary.jsonl"), &rows)?;
    // Line level, for line readers: pooled line CER, omissions, loops and the guard, GPU time.
    let mut lrows = vec![];
    let mut ldetail = vec![];
    for (name, by) in &line_maps {
        let mut yp: Vec<(String, (f64, f64))> = vec![];
        let mut rp: Vec<(String, (f64, f64))> = vec![];
        let (mut lines, mut missing, mut empty, mut short, mut loops, mut tails, mut cuts, mut stopped, mut capped, mut records) = (0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        let mut ms = 0f64;
        let mut msv = vec![];
        for r in &refs {
            if r.get("skipped").is_some() {
                continue;
            }
            let id = r["id"].as_str().unwrap_or("");
            let page = r["page"].as_str().unwrap_or("").to_string();
            let reference = r["ref"].as_str().unwrap_or("");
            lines += 1;
            let x = by.get(id);
            missing += usize::from(x.is_none() || x.is_some_and(|x| x.get("missing").is_some()));
            let h = x.and_then(|x| x["text"].as_str()).unwrap_or("");
            let (gy, hy): (Vec<char>, Vec<char>) = (yivo_simplify(reference).chars().collect(), yivo_simplify(h.trim()).chars().collect());
            let (gr, hr): (Vec<char>, Vec<char>) = (folded(reference, true).chars().collect(), folded(h, false).chars().collect());
            let ey = edits(&hy, &gy);
            let er = edits(&hr, &gr);
            yp.push((page.clone(), (ey.total() as f64, gy.len() as f64)));
            rp.push((page.clone(), (er.total() as f64, gr.len() as f64)));
            let is_empty = h.trim().is_empty();
            empty += usize::from(is_empty);
            let is_short = !is_empty && (hr.len() as f64) < 0.5 * gr.len() as f64;
            short += usize::from(is_short);
            if let Some(x) = x {
                let flag = |k: &str| x[k].as_bool().unwrap_or(false);
                loops += usize::from(flag("loop_at_budget"));
                tails += usize::from(flag("tail_repetition"));
                cuts += usize::from(flag("guard_cut"));
                stopped += usize::from(flag("guard_stopped"));
                capped += usize::from(flag("budget_exhausted"));
                records += usize::from(flag("record"));
                let t = x["ms"].as_f64().unwrap_or(0.0);
                ms += t;
                msv.push(t);
            }
            if er.total() > 0 || is_empty {
                ldetail.push(json!({"reader": name, "id": id, "page": page, "ref_chars": gr.len(), "raw_edits": er.total(), "yivo_edits": ey.total(), "empty": is_empty, "short": is_short}));
            }
        }
        let pooled = |v: &[(String, (f64, f64))]| v.iter().map(|x| x.1 .0).sum::<f64>() / v.iter().map(|x| x.1 .1).sum::<f64>().max(1.0);
        let (yl, yh) = crate::boot(&crate::paged(&yp), n, &mut rng);
        let (rl, rh) = crate::boot(&crate::paged(&rp), n, &mut rng);
        let med_ms = if msv.is_empty() { 0.0 } else { pct(&mut msv, 0.5) };
        lrows.push(json!({"reader": name, "lines": lines, "missing": missing, "records": records,
            "yivo_line_cer": pooled(&yp), "yivo_lo": yl, "yivo_hi": yh, "raw_line_cer": pooled(&rp), "raw_lo": rl, "raw_hi": rh,
            "empty": empty, "under_half_length": short, "loop_at_budget": loops, "tail_repetition": tails, "guard_cut": cuts, "guard_stopped": stopped, "budget_exhausted": capped,
            "request_seconds": ms / 1000.0, "seconds_per_line": ms / 1000.0 / lines.max(1) as f64, "median_line_seconds": med_ms / 1000.0}));
    }
    write_rows(&out.join("lines.jsonl"), &lrows)?;
    write_rows(&out.join("line-errors.jsonl"), &ldetail)
}

// ---------------------------------------------------------------- agreement on untranscribed crops

/// Pairwise distance between readers, both sides YIVO-simplified with whitespace collapsed and points
/// left out: the symmetric edit rate (edits over the longer text), and each reader's CER taken against
/// `--reference` as if it were the truth. Neither is accuracy.
pub fn agree(items: &Path, hyps: &[String], reference: &str, out: &Path, n: usize, seed: u64) -> Result<()> {
    let items = read_jsonl(items)?;
    let mut readers = vec![];
    for s in hyps {
        let (name, rows) = spec(s)?;
        let m: HashMap<String, String> = rows.iter().filter_map(|r| Some((r["id"].as_str()?.to_string(), r["text"].as_str()?.to_string()))).collect();
        readers.push((name, m));
    }
    let Some(ri) = readers.iter().position(|r| r.0 == reference) else { bail!("--reference {reference} is not a --hyp") };
    let norm = |s: &str| -> Vec<char> { without_hebrew_points(&yivo_simplify(s)).split_whitespace().collect::<Vec<_>>().join(" ").chars().collect() };
    let mut rng = Rng(seed);
    let mut rows = vec![];
    let mut per_item = vec![];
    for i in 0..readers.len() {
        for j in 0..readers.len() {
            if i == j {
                continue;
            }
            let (mut sym, mut vs_ref) = (vec![], vec![]);
            for it in &items {
                let id = it["id"].as_str().unwrap_or("");
                let page = it["page"].as_str().unwrap_or(id).to_string();
                let (Some(a), Some(b)) = (readers[i].1.get(id), readers[j].1.get(id)) else { continue };
                let (na, nb) = (norm(a), norm(b));
                let e = edits(&na, &nb).total() as f64;
                sym.push((page.clone(), (e, na.len().max(nb.len()) as f64)));
                vs_ref.push((page, (e, nb.len() as f64)));
                if j == ri {
                    per_item.push(json!({"id": id, "page": it["page"], "reader": readers[i].0, "reference": reference, "edits": e, "reference_chars": nb.len(), "reader_chars": na.len(),
                        "cer_vs_reference": e / nb.len().max(1) as f64, "symmetric_edit_rate": e / na.len().max(nb.len()).max(1) as f64}));
                }
            }
            if sym.is_empty() {
                continue;
            }
            let pooled = |v: &[(String, (f64, f64))]| v.iter().map(|x| x.1 .0).sum::<f64>() / v.iter().map(|x| x.1 .1).sum::<f64>().max(1.0);
            if i < j {
                let (lo, hi) = crate::boot(&crate::paged(&sym), n, &mut rng);
                rows.push(json!({"kind": "pair", "a": readers[i].0, "b": readers[j].0, "items": sym.len(), "pages": crate::paged(&sym).len(),
                    "symmetric_edit_rate": pooled(&sym), "lo": lo, "hi": hi, "note": "agreement, not accuracy"}));
            }
            if j == ri {
                let (lo, hi) = crate::boot(&crate::paged(&vs_ref), n, &mut rng);
                rows.push(json!({"kind": "vs_reference", "reader": readers[i].0, "reference": reference, "items": vs_ref.len(),
                    "cer_vs_reference": pooled(&vs_ref), "lo": lo, "hi": hi, "note": "distance from the reference reader, not accuracy"}));
            }
        }
    }
    write_rows(out, &rows)?;
    let detail = out.with_extension("items.jsonl");
    write_rows(&detail, &per_item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplifies_like_jochre() {
        // Decomposed double vov and yud pairs become the ligatures unless a mark says otherwise.
        assert_eq!(yivo_simplify("וואַרט"), "װאַרט".nfd().collect::<String>());
        assert_eq!(yivo_simplify("ייִדיש"), "ייִדיש".nfd().collect::<String>());
        assert_eq!(yivo_simplify("מיין"), "מײן");
        assert_eq!(yivo_simplify("ייַ"), "ײַ");
        // Non-YIVO points go; a komets survives only under alef.
        assert_eq!(yivo_simplify("בָּ"), "בּ");
        assert_eq!(yivo_simplify("אָ"), "אָ".nfd().collect::<String>());
        assert_eq!(yivo_simplify("a-b ''x'' ,,y"), "a־b “x“ „y");
        assert_eq!(paper_text("a\n\n\nb\n"), "a\nb");
    }

    #[test]
    fn edit_operations() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        let e = edits(&c("abd"), &c("abcd"));
        assert_eq!((e.sub, e.ins, e.del), (0, 0, 1));
        let e = edits(&c("abxcd"), &c("abcd"));
        assert_eq!((e.sub, e.ins, e.del), (0, 1, 0));
        assert_eq!(edits(&c(""), &c("abc")).del, 3);
        assert_eq!(edits(&c("kitten"), &c("sitting")).total(), 3);
    }

    #[test]
    fn alto_line_text() {
        let xml = r#"<TextLine HPOS="10" VPOS="20" WIDTH="30" HEIGHT="5"><String CONTENT="a&quot;b"/><SP WIDTH="3"/><String CONTENT="c"/><HYP CONTENT="-"/></TextLine>"#;
        let l = alto_lines(xml, 100, 100);
        assert_eq!(l[0].text, "a\"b c-");
        assert_eq!(l[0].rect, [10, 20, 40, 25]);
    }
}
