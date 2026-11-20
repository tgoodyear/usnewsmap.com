//! The page-mode collector (#1998; design `#1997`, "Gating" and "Cost accounting").
//!
//! `usnm-boinc qwen page-collect` reads a page-mode staging root, the batch-109 spool, the
//! crop-state records `qwen compare --page-rule page-blocks-v1` keeps and, for cost, every
//! checked result (`check --keep`'s `seen` directory), and writes under a HELD root:
//!
//! - `<doc_id>/ours.xml` and `<doc_id>/blocks.json`: the page assembled from LoC's ALTO with
//!   the canonical result's text for each region an agreeing pair judged `agree`
//!   ([`super::page_assemble`]);
//! - `ocr_outputs.jsonl`: one row per held page, for `quality.ocr_outputs` as the reader
//!   `ours-xiaomi` (`load.py xiaomi`), with the engine tag, the damage rate, the completeness
//!   gate's result and the page's agreeing share;
//! - `vlm_page_cost.jsonl`: one row per result and page, for `quality.vlm_page_cost`: GPU
//!   seconds for every returned result, a second replica, a third and an invalid one included;
//! - `summary.json`.
//!
//! Nothing here writes `ocr-local`, `ocr-out` or the search index, and the held root may not be
//! under a directory with one of those names. A page is acceptable ([`PageRow::accepted`]) when
//! its agreeing share by characters reaches `--acceptance`; with none given nothing is
//! accepted, as the design has it until the Science Coordinator sets `A`. Every page whose pair
//! was judged is held either way, because `quality_gain` and the blind picks need them to
//! measure it.
use super::consensus::{self, Record, MAX_RECORD};
use super::page_assemble::{self, Facts, Inputs, RegionIn};
use super::page_mode::{BundleProvenance, PageProvenance, PROVENANCE_FILE};
use super::{units, validate, JOB_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use usnm_core::ocr_engine::{Digest, Fields, OcrEngineTag};
use usnm_ocr_contract::{
    boinc::{is_sha256, sha256},
    qwen::{Engine, Job, Outcome, Readings, ResultManifest, MAX_JOB_JSON, MAX_RESULT_JSON},
};

/// `boinc/apps/qwen/weights.lock`: the two Xiaomi files' checksums make the tag's `models=`.
pub const WEIGHTS_LOCK: &str = include_str!("../../../../boinc/apps/qwen/weights.lock");
/// The upstream revision `xiaomi-ocr-0-q8_0.gguf` was converted from, short.
pub const XIAOMI_REVISION: &str = "bbd8cf8d21f2";
/// Quantisation and request version.
pub const PROFILE: &str = "q8_0.rv5";
pub const ROUTE: &str = "boinc-page";
/// The reader of `quality.ocr_outputs` and the `ocr_source` of the rows.
pub const READER: &str = "ours-xiaomi";
pub const SOURCE: &str = "usnm-xiaomi";
pub const OCR_OUTPUTS_FILE: &str = "ocr_outputs.jsonl";
pub const COST_FILE: &str = "vlm_page_cost.jsonl";
pub const SUMMARY_FILE: &str = "summary.json";
pub const ALTO_OUT: &str = "ours.xml";
pub const BLOCKS_OUT: &str = "blocks.json";
/// Directories a held page may never be written under.
const FORBIDDEN: [&str; 4] = ["ocr-local", "ocr-out", "ocr-sync", "search"];

/// `models=` of the tag: the first 12 hex digits of the sha256 of the sorted `<name> <sha256>`
/// lines of the model and projector files of `lock` (`weights.lock`'s lines are
/// `<name> <url> <sha256>`).
pub fn models_digest(lock: &str) -> anyhow::Result<String> {
    let mut lines: Vec<String> = Vec::new();
    for name in [super::XIAOMI_FILE, super::XIAOMI_MMPROJ] {
        let line = lock
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name))
            .with_context(|| format!("weights.lock has no {name}"))?;
        let sum = line
            .split_whitespace()
            .last()
            .context("weights.lock line")?;
        ensure!(is_sha256(sum), "weights.lock: {name} has no sha256");
        lines.push(format!("{name} {sum}"));
    }
    lines.sort();
    Ok(sha256(lines.join("\n").as_bytes())[..12].to_owned())
}

fn short(digest: &str) -> anyhow::Result<String> {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    ensure!(hex.len() >= 12, "digest too short");
    Ok(hex[..12].to_owned())
}

/// The `ocr_engine` tag of a page read with `engine` (13 §13.8.1).
pub fn engine_tag(engine: &Engine, lock: &str) -> anyhow::Result<OcrEngineTag> {
    Ok(OcrEngineTag::new(Fields {
        family: "xiaomi-ocr-0",
        version: XIAOMI_REVISION,
        profile: PROFILE,
        image: Digest::Sha256(short(&engine.image)?),
        models: Digest::Sha256(models_digest(lock)?),
        route: ROUTE,
        route_version: 1,
        contract: 0,
    })?)
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The page-mode staging root (`qwen stage --page-mode --root`).
    pub root: PathBuf,
    /// The `usnm_quality` staging root the pages were cut from, for LoC's ALTO.
    pub quality_root: PathBuf,
    /// Where the batch-109 canonical results land.
    pub spool: PathBuf,
    /// `qwen compare --crop-states`.
    pub crop_states: PathBuf,
    /// `qwen check --keep`: every checked result, for cost. Without it only canonical results
    /// have cost rows.
    pub seen: Option<PathBuf>,
    /// The held root.
    pub held: PathBuf,
    /// The agreeing share by characters a page needs to be acceptable. Unset: none is.
    pub acceptance: Option<f64>,
    /// `weights.lock`'s text (default [`WEIGHTS_LOCK`]).
    pub weights_lock: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub units: usize,
    /// Pages assembled and held.
    pub held: usize,
    pub accepted: usize,
    /// Units with no canonical result yet.
    pub pending: usize,
    /// Units whose canonical result no agreeing pair judged region by region.
    pub unjudged: usize,
    /// Units whose canonical result did not finish.
    pub unfinished: usize,
    /// Pages refused: LoC's ALTO is not the one staged, or the regions do not fit it.
    pub refused: usize,
    pub cost_rows: usize,
    /// The staged pages of those units, for the pilot's stop conditions: all of them, those
    /// whose unit has no canonical result yet, and those whose unit came back unfinished or
    /// unjudged (a page `refused` is counted by `refused`).
    pub pages: usize,
    /// Units BOINC gave up (`_error`, `_no_output_files`): settled, invalid, no canonical result.
    pub gave_up: usize,
    /// Units that came back invalid (unfinished, unjudged, given up, or with a page refused):
    /// the pilot's stop condition counts units, as #1998 defines it.
    pub units_invalid: usize,
    pub pages_pending: usize,
    pub pages_invalid: usize,
}

/// One row of `ocr_outputs.jsonl`: the `quality.ocr_outputs` columns (`doc_id`, `reader`,
/// `engine`, `source`, `ocred_at`, `text_chars`, `damage_rate`) and what the page measured.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageRow {
    pub doc_id: String,
    pub reader: &'static str,
    pub engine: String,
    pub source: &'static str,
    pub ocred_at: String,
    pub text_chars: usize,
    pub damage_rate: Option<f64>,
    pub loc_damage_rate: Option<f64>,
    /// The #1915 completeness gate on the assembled text against LoC's: `pass`, or `short` or
    /// `cover`.
    pub gate: String,
    pub gate_len_ratio: Option<f64>,
    pub gate_cover: Option<f64>,
    pub campaign: String,
    pub job_id: String,
    pub regions: usize,
    pub xiaomi_regions: usize,
    pub loc_by_reason: BTreeMap<&'static str, usize>,
    pub agree_share_regions: Option<f64>,
    pub agree_share_chars: Option<f64>,
    /// Whether `agree_share_chars` reaches `--acceptance`. Never true while it is unset.
    pub accepted: bool,
    pub decade: Option<i32>,
    pub band: Option<String>,
}

/// One row of `vlm_page_cost.jsonl` (`quality.vlm_page_cost`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostRow {
    pub doc_id: String,
    pub campaign: String,
    pub job_id: String,
    /// The checked result's name (BOINC's result name); `result_id`, `host_id`, `app_version`
    /// and `platform` are not in a result, and are joined from the BOINC provenance.
    pub result_name: String,
    pub plan_class: Option<String>,
    pub gpu: Option<String>,
    pub gpu_mem_total_mib: Option<u64>,
    pub slots: Option<u32>,
    pub ctx_size: Option<u32>,
    pub load_ms: u64,
    pub engine_ms: u64,
    /// The page's share of the unit's completion tokens.
    pub page_share: f64,
    /// `(load_ms + engine_ms) * page_share / 1000`.
    pub gpu_seconds: f64,
    pub regions: usize,
    pub tokens: u64,
    pub chars_out: usize,
    pub gpu_mem_peak_mib: Option<u64>,
    pub outcome: Outcome,
    /// An agreeing pair includes this result.
    pub validated: bool,
    /// This is the unit's canonical result.
    pub canonical: bool,
}

/// A staged page of a unit: its own regions, and where it was cut from.
struct UnitPage<'a> {
    doc_id: &'a str,
    /// A bundle's crop-id prefix, `<doc_id>:`; empty for a single page.
    prefix: String,
    prov: PageProvenance,
    file_name: String,
}

fn unit_pages<'a>(unit: &Path, job: &'a Job) -> anyhow::Result<Vec<UnitPage<'a>>> {
    let bytes = read(&unit.join(PROVENANCE_FILE), 4 << 20)
        .with_context(|| format!("{}: {PROVENANCE_FILE}", unit.display()))?;
    if job.pages.is_empty() {
        let prov: PageProvenance = serde_json::from_slice(&bytes)
            .with_context(|| format!("{}: {PROVENANCE_FILE}", unit.display()))?;
        ensure!(prov.doc_id == job.doc_id, "{}: wrong page", unit.display());
        return Ok(vec![UnitPage {
            doc_id: &job.doc_id,
            prefix: String::new(),
            prov,
            file_name: job.input.source.clone(),
        }]);
    }
    let bundle: BundleProvenance = serde_json::from_slice(&bytes)
        .with_context(|| format!("{}: {PROVENANCE_FILE}", unit.display()))?;
    job.pages
        .iter()
        .map(|bp| {
            let prov = bundle
                .pages
                .iter()
                .find(|p| p.doc_id == bp.doc_id)
                .cloned()
                .with_context(|| format!("{}: no provenance for {}", unit.display(), bp.doc_id))?;
            Ok(UnitPage {
                doc_id: &bp.doc_id,
                prefix: format!("{}:", bp.doc_id),
                prov,
                file_name: bp.input.source.clone(),
            })
        })
        .collect()
}

/// The `Loaded` of the canonical result of `job`, when its files are in the spool.
fn canonical(spool: &Path, job: &Job) -> anyhow::Result<Option<validate::Loaded>> {
    let wu = job.workunit();
    let files = crate::quality::collect::spool_files(spool, &wu, &super::outputs());
    if !files.iter().all(|f| f.exists()) {
        return Ok(None);
    }
    let got = validate::load(&files).with_context(|| format!("canonical result {wu}"))?;
    ensure!(
        got.result.job_id == job.id,
        "{wu}: result does not belong to its job"
    );
    Ok(Some(got))
}

/// The `readings_sha256` of every result an agreeing pair of `job_id` includes.
fn agreeing(dir: &Path, job_id: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for path in consensus::record_paths(dir, job_id)? {
        let r: Record = serde_json::from_slice(&read(&path, MAX_RECORD)?)
            .with_context(|| format!("crop states {}", path.display()))?;
        if r.agree {
            out.extend(r.readings_sha256);
        }
    }
    Ok(out)
}

fn is_held_path(p: &Path) -> bool {
    p.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|n| FORBIDDEN.contains(&n))
    })
}

fn diag_u32(r: &Readings, key: &str) -> Option<u32> {
    r.diag.get(key)?.parse().ok()
}

/// Every checked result of `job`, by name: `usnm_qwen_<job id>_...` under `seen`.
fn seen_results(seen: &Path, job: &Job) -> anyhow::Result<Vec<(String, ResultManifest, Readings)>> {
    let prefix = job.workunit();
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(seen) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).context("reading the seen results"),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) {
            continue;
        }
        let dir = entry.path();
        let Ok(manifest) = read(&dir.join(super::RESULT_FILE), MAX_RESULT_JSON) else {
            continue;
        };
        let Ok(readings) = read(
            &dir.join(super::READINGS_FILE),
            usnm_ocr_contract::qwen::MAX_READINGS,
        ) else {
            continue;
        };
        let (Ok(m), Ok(r)) = (
            serde_json::from_slice::<ResultManifest>(&manifest),
            serde_json::from_slice::<Readings>(&readings),
        ) else {
            continue;
        };
        if m.job_id == job.id && m.readings_sha256 == sha256(&readings) {
            out.push((name, m, r));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn cost_rows(
    pages: &[UnitPage<'_>],
    job: &Job,
    results: &[(String, ResultManifest, Readings)],
    canonical_sha: Option<&str>,
    agreeing: &BTreeSet<String>,
) -> Vec<CostRow> {
    let mut rows = Vec::new();
    for (name, m, r) in results {
        let total: u64 = r.readings.iter().map(|x| u64::from(x.tokens)).sum();
        for p in pages {
            let mine: Vec<_> = r
                .readings
                .iter()
                .filter(|x| p.prefix.is_empty() || x.id.starts_with(&p.prefix))
                .collect();
            let tokens: u64 = mine.iter().map(|x| u64::from(x.tokens)).sum();
            // A result that read nothing still held the GPU: its time goes to the pages in
            // equal shares.
            let page_share = if total > 0 {
                tokens as f64 / total as f64
            } else {
                1.0 / pages.len() as f64
            };
            rows.push(CostRow {
                doc_id: p.doc_id.to_owned(),
                campaign: p.prov.campaign.clone(),
                job_id: job.id.clone(),
                result_name: name.clone(),
                plan_class: m.host.plan_class.clone(),
                gpu: m.host.gpu.clone(),
                gpu_mem_total_mib: m.host.gpu_mem_total_mib,
                slots: diag_u32(r, "slots"),
                ctx_size: diag_u32(r, "ctx_size"),
                load_ms: m.load_ms,
                engine_ms: m.engine_ms,
                page_share,
                gpu_seconds: (m.load_ms + m.engine_ms) as f64 * page_share / 1000.0,
                regions: mine.len(),
                tokens,
                chars_out: mine.iter().map(|x| x.text.chars().count()).sum(),
                gpu_mem_peak_mib: m.gpu_mem_peak_mib,
                outcome: m.outcome,
                validated: agreeing.contains(&m.readings_sha256),
                canonical: canonical_sha == Some(m.readings_sha256.as_str()),
            });
        }
    }
    rows
}

fn jsonl<T: Serialize>(rows: &[T]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut out, r)?;
        out.push(b'\n');
    }
    Ok(out)
}

/// The source of LoC's ALTO for `doc` in a `usnm_quality` staging root.
fn loc_alto(quality_root: &Path, doc: &str) -> anyhow::Result<Vec<u8>> {
    for dir in [
        quality_root.join("pages").join(doc),
        crate::quality::units(quality_root).join(doc),
    ] {
        let path = dir.join(crate::quality::ALTO_FILE);
        if path.is_file() {
            return read(&path, usnm_ocr_contract::quality::MAX_ALTO);
        }
    }
    anyhow::bail!("{doc}: no staged ALTO under {}", quality_root.display())
}

/// Collects the root: assembles and holds each page whose canonical result an agreeing pair
/// judged region by region, and writes the rows. Run again, it rewrites the same bytes for
/// the same inputs.
pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    ensure!(
        !is_held_path(&opts.held),
        "{}: held pages are never written under ocr-local, ocr-out, ocr-sync or search",
        opts.held.display()
    );
    if let Some(a) = opts.acceptance {
        ensure!((0.0..=1.0).contains(&a), "--acceptance is 0 to 1");
    }
    std::fs::create_dir_all(&opts.held)?;
    // The destination as the filesystem has it: a relative path run from inside a forbidden
    // tree, or a symlink into one, is refused on what it resolves to.
    let held = std::fs::canonicalize(&opts.held)
        .with_context(|| format!("resolving {}", opts.held.display()))?;
    ensure!(
        !is_held_path(&held),
        "{} resolves to {}: held pages are never written under ocr-local, ocr-out, ocr-sync or search",
        opts.held.display(),
        held.display()
    );
    let resolved = Options {
        held,
        ..opts.clone()
    };
    let opts = &resolved;
    let dir = units(&opts.root);
    let mut unit_dirs: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(e) => e
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join(JOB_FILE).exists())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).context("reading the staging root"),
    };
    unit_dirs.sort();
    let mut summary = Summary::default();
    let mut pages: Vec<PageRow> = Vec::new();
    let mut costs: Vec<CostRow> = Vec::new();
    for unit in unit_dirs {
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("{}", unit.display()))?;
        let Some(page_mode) = &job.page_mode else {
            continue;
        };
        summary.units += 1;
        let staged = unit_pages(&unit, &job)?;
        summary.pages += staged.len();
        let canon = canonical(&opts.spool, &job)?;
        let canon_sha = canon.as_ref().map(|c| c.result.readings_sha256.clone());
        let agreeing_shas = agreeing(&opts.crop_states, &job.id)?;
        // A unit is settled once it has a canonical result or BOINC gave it up.
        let gave_up =
            canon.is_none() && crate::quality::collect::gave_up(&opts.spool, &job.workunit());
        // Cost counts every checked result (`--seen`, whose directory names are BOINC's result
        // names: the key the cost table is unique on), and only for a settled unit, so a row's
        // `validated` and `canonical` are final when it is loaded. Without `--seen` there is
        // no result identity, and no cost row is made.
        let results = match &opts.seen {
            Some(seen) if canon.is_some() || gave_up => seen_results(seen, &job)?,
            _ => Vec::new(),
        };
        let rows = cost_rows(
            &staged,
            &job,
            &results,
            canon_sha.as_deref(),
            &agreeing_shas,
        );
        summary.cost_rows += rows.len();
        costs.extend(rows);
        let Some(canon) = canon else {
            if gave_up {
                // `_error` or `_no_output_files`: settled, and invalid.
                summary.gave_up += 1;
                summary.units_invalid += 1;
                summary.pages_invalid += staged.len();
            } else {
                summary.pending += 1;
                summary.pages_pending += staged.len();
            }
            continue;
        };
        if canon.result.outcome != Outcome::Ok {
            summary.unfinished += 1;
            summary.units_invalid += 1;
            summary.pages_invalid += staged.len();
            continue;
        }
        let Some(states) =
            consensus::agreed_states(&opts.crop_states, &job.id, &canon.result.readings_sha256)?
        else {
            summary.unjudged += 1;
            summary.units_invalid += 1;
            summary.pages_invalid += staged.len();
            continue;
        };
        let tag = engine_tag(&job.engine, &opts.weights_lock)?.to_string();
        let pair = states.results.clone();
        let canonical_name = results
            .iter()
            .find(|(_, m, _)| Some(&m.readings_sha256) == canon_sha.as_ref())
            .map(|(n, _, _)| n.clone());
        let refused_before = summary.refused;
        for p in &staged {
            match hold_page(
                opts,
                &job,
                page_mode,
                p,
                &canon,
                &states,
                &tag,
                &pair,
                canonical_name.as_deref(),
            ) {
                Ok(row) => {
                    summary.held += 1;
                    summary.accepted += usize::from(row.accepted);
                    pages.push(row);
                }
                Err(e) => {
                    eprintln!("{}: not held: {e:#}", p.doc_id);
                    summary.refused += 1;
                }
            }
        }
        summary.units_invalid += usize::from(summary.refused > refused_before);
    }
    pages.sort_by(|a, b| a.doc_id.cmp(&b.doc_id));
    costs.sort_by(|a, b| (&a.doc_id, &a.result_name).cmp(&(&b.doc_id, &b.result_name)));
    atomic(&opts.held.join(OCR_OUTPUTS_FILE), &jsonl(&pages)?)?;
    atomic(&opts.held.join(COST_FILE), &jsonl(&costs)?)?;
    atomic(
        &opts.held.join(SUMMARY_FILE),
        &serde_json::to_vec_pretty(&summary)?,
    )?;
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn hold_page(
    opts: &Options,
    job: &Job,
    page_mode: &usnm_ocr_contract::qwen::PageMode,
    p: &UnitPage<'_>,
    canon: &validate::Loaded,
    states: &consensus::PairStates,
    tag: &str,
    pair: &[String],
    canonical_name: Option<&str>,
) -> anyhow::Result<PageRow> {
    usnm_ocr_contract::quality::check_doc_id(p.doc_id)?;
    let alto = loc_alto(&opts.quality_root, p.doc_id)?;
    ensure!(
        sha256(&alto) == p.prov.loc_alto_sha256,
        "LoC's ALTO changed since staging"
    );
    let regions: Vec<RegionIn<'_>> = page_mode
        .regions
        .iter()
        .filter(|r| p.prefix.is_empty() || r.id.starts_with(&p.prefix))
        .map(|r| RegionIn {
            region: r,
            crop: &r.id[p.prefix.len()..],
            reading: canon
                .readings
                .readings
                .iter()
                .find(|x| x.id == r.id)
                .map(|x| x.text.as_str()),
            state: states.regions.get(&r.id).map(|v| (v.state, v.score)),
        })
        .collect();
    ensure!(!regions.is_empty(), "the unit has no region of this page");
    let other = canonical_name.and_then(|c| pair.iter().find(|n| n.as_str() != c));
    let mut notes = vec![
        ("usnm_engine_image".to_owned(), job.engine.image.clone()),
        ("usnm_llama_image".to_owned(), job.engine.llama.clone()),
        ("usnm_job_id".to_owned(), job.id.clone()),
    ];
    if let Some(c) = &canon.result.host.plan_class {
        notes.push(("usnm_plan_class".to_owned(), c.clone()));
    }
    if let Some(n) = canonical_name {
        notes.push(("usnm_boinc_result".to_owned(), n.to_owned()));
    }
    if let Some(n) = other {
        notes.push(("usnm_pair_result".to_owned(), n.clone()));
    }
    let facts = Facts {
        tag,
        model: &job.engine.models[0].name,
        revision: XIAOMI_REVISION,
        prompt_version: job.prompt_version,
        image: &job.engine.image,
        llama: &job.engine.llama,
        weights: &job.engine.weights,
        completed_at: Some(&canon.result.completed_at),
    };
    let assembled = page_assemble::assemble(&Inputs {
        doc_id: p.doc_id,
        loc_alto: &alto,
        dims: (p.prov.page_width, p.prov.page_height),
        file_name: &p.file_name,
        language: &job.language,
        regions,
        facts,
        notes,
    })?;
    let page_dir = opts.held.join(p.doc_id);
    std::fs::create_dir_all(&page_dir)?;
    // A page directory that is a symlink out of the held root is not written through.
    let resolved = std::fs::canonicalize(&page_dir)?;
    ensure!(
        resolved.starts_with(&opts.held) && !is_held_path(&resolved),
        "{}: the page directory resolves outside the held root",
        p.doc_id
    );
    atomic(&page_dir.join(ALTO_OUT), assembled.alto.as_bytes())?;
    atomic(
        &page_dir.join(BLOCKS_OUT),
        &serde_json::to_vec_pretty(&assembled.blocks)?,
    )?;
    // LoC's text, for the gate and the damage rate it is compared with.
    let loc_page = crate::quality::parse_alto(
        &alto,
        f64::from(p.prov.page_width),
        f64::from(p.prov.page_height),
    )?;
    let loc_text = usnm_alto::text(&loc_page);
    let langs = [job.language.as_str()];
    let ours = usnm_ocr_local::pages::Scores::of(&assembled.text, &langs);
    let loc = usnm_ocr_local::pages::Scores::of(&loc_text, &langs);
    let (measure, verdict) =
        usnm_ocr_local::rule::Completeness::default().check(&assembled.text, Some(&loc_text));
    let b = &assembled.blocks;
    Ok(PageRow {
        doc_id: p.doc_id.to_owned(),
        reader: READER,
        engine: tag.to_owned(),
        source: SOURCE,
        ocred_at: chrono::DateTime::parse_from_rfc3339(&canon.result.completed_at)?
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        text_chars: assembled.text.chars().count(),
        damage_rate: ours.damage_rate,
        loc_damage_rate: loc.damage_rate,
        gate: verdict.map_or_else(|k| k.as_str().to_owned(), |()| "pass".to_owned()),
        gate_len_ratio: measure.map(|m| m.len_ratio),
        gate_cover: measure.map(|m| m.cover),
        campaign: p.prov.campaign.clone(),
        job_id: job.id.clone(),
        regions: b.regions,
        xiaomi_regions: b.xiaomi,
        loc_by_reason: b.loc_by_reason.clone(),
        agree_share_regions: b.agree_share_regions,
        agree_share_chars: b.agree_share_chars,
        accepted: opts
            .acceptance
            .zip(b.agree_share_chars)
            .is_some_and(|(a, s)| s >= a),
        decade: p.prov.stratum.as_ref().map(|s| s.decade),
        band: p.prov.stratum.as_ref().map(|s| s.band.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qwen::page_mode::{Params, Stats};
    use crate::qwen::validate::{compare_with, load, BundleVerdict, Compare, Rule};
    use usnm_ocr_contract::{
        quality::Input,
        qwen::{Crop, Model, PageMode, Reading, Region, PROMPT_VERSION_XIAOMI, VERSION},
    };

    const DOC: &str = "sn1_1900-01-01_ed-1_seq-1";
    const ALTO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><MeasurementUnit>pixel</MeasurementUnit></Description><Layout><Page ID="P1" WIDTH="1000" HEIGHT="1400" PHYSICAL_IMG_NR="1"><PrintSpace>
<TextBlock ID="B1" HPOS="0" VPOS="0" WIDTH="1000" HEIGHT="100"><TextLine ID="B1L1" HPOS="0" VPOS="0" WIDTH="600" HEIGHT="30"><String CONTENT="The" HPOS="0" VPOS="0" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="shlp" HPOS="110" VPOS="0" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="sailed" HPOS="220" VPOS="0" WIDTH="150" HEIGHT="30"/><SP/><String CONTENT="for" HPOS="380" VPOS="0" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="England" HPOS="490" VPOS="0" WIDTH="100" HEIGHT="30"/></TextLine></TextBlock>
<TextBlock ID="B2" HPOS="0" VPOS="200" WIDTH="1000" HEIGHT="100"><TextLine ID="B2L1" HPOS="0" VPOS="200" WIDTH="600" HEIGHT="30"><String CONTENT="Eggs" HPOS="0" VPOS="200" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="twelve" HPOS="110" VPOS="200" WIDTH="150" HEIGHT="30"/><SP/><String CONTENT="cents" HPOS="270" VPOS="200" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="dozen" HPOS="380" VPOS="200" WIDTH="100" HEIGHT="30"/></TextLine></TextBlock>
<TextBlock ID="B3" HPOS="0" VPOS="400" WIDTH="1000" HEIGHT="100"><TextLine ID="B3L1" HPOS="0" VPOS="400" WIDTH="600" HEIGHT="30"><String CONTENT="Coal" HPOS="0" VPOS="400" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="sold" HPOS="110" VPOS="400" WIDTH="100" HEIGHT="30"/></TextLine></TextBlock>
</PrintSpace></Page></Layout></alto>"#;

    fn engine() -> Engine {
        Engine {
            image: format!("sha256:{}", "a".repeat(64)),
            llama: format!("sha256:{}", "b".repeat(64)),
            models: vec![Model {
                file: super::super::XIAOMI_FILE.into(),
                min_gpu_mb: 2000,
                mmproj: super::super::XIAOMI_MMPROJ.into(),
                name: super::super::XIAOMI_MODEL.into(),
            }],
            weights: format!("sha256:{}", "c".repeat(64)),
        }
    }

    fn crop(id: &str, y: u32, text: &str) -> Crop {
        Crop {
            rect: [0, y, 700, y + 40],
            id: id.into(),
            loc_text: text.into(),
            stratum: "region".into(),
        }
    }

    fn job() -> Job {
        let mut j = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: 600,
            crops: vec![
                crop("r0001", 0, "The shlp sailed for England"),
                crop("r0002", 200, "Eggs twelve cents dozen"),
                crop("r0003", 400, "Coal sold"),
            ],
            doc_id: DOC.into(),
            engine: engine(),
            gpu_timeout_secs: 600,
            group: "x08-eng-pilot".into(),
            input: Input {
                bytes: 3,
                sha256: sha256(b"jpg"),
                source: "https://tile.loc.gov/img".into(),
            },
            language: "eng".into(),
            max_tokens_per_1000px2: 4,
            pages: vec![],
            prompt_version: PROMPT_VERSION_XIAOMI,
            model: Some(super::super::XIAOMI_MODEL.into()),
            page_mode: Some(PageMode {
                regions: vec![
                    Region {
                        id: "r0001".into(),
                        block: "B1".into(),
                        lines: vec!["B1L1".into()],
                    },
                    Region {
                        id: "r0002".into(),
                        block: "B2".into(),
                        lines: vec!["B2L1".into()],
                    },
                    Region {
                        id: "r0003".into(),
                        block: "B3".into(),
                        lines: vec!["B3L1".into()],
                    },
                ],
                slots: None,
            }),
        };
        j.id = j.identity().unwrap();
        j
    }

    /// A finished Xiaomi result of `job` reading `texts`, as the worker writes it: the three
    /// files in `dir`, named `name`.
    fn write_result(
        dir: &Path,
        name: &str,
        job: &Job,
        texts: &[(&str, &str)],
        slots: &str,
    ) -> Vec<PathBuf> {
        let readings = Readings {
            v: VERSION,
            job_id: job.id.clone(),
            device: "vulkan".into(),
            diag: [
                ("slots".to_owned(), slots.to_owned()),
                ("ctx_size".to_owned(), "16384".to_owned()),
            ]
            .into(),
            model: job.model.clone(),
            outcome: Outcome::Ok,
            prompt_version: PROMPT_VERSION_XIAOMI,
            readings: texts
                .iter()
                .map(|(id, t)| Reading {
                    id: (*id).to_owned(),
                    ms: 10,
                    text: (*t).to_owned(),
                    tokens: t.len() as u32,
                })
                .collect(),
        };
        let rb = serde_json::to_vec(&readings).unwrap();
        let host = usnm_ocr_contract::surya::Host {
            plan_class: Some("nvidia_vulkan_x08p".into()),
            gpu: Some("GPU 0".into()),
            ..Default::default()
        };
        let manifest = ResultManifest {
            v: VERSION,
            job_id: job.id.clone(),
            completed_at: "2026-11-20T10:00:00Z".into(),
            crops: job.crops.len() as u32,
            device: "vulkan".into(),
            diag_bytes: 4,
            diag_sha256: sha256(b"diag"),
            engine: job.engine.clone(),
            engine_ms: 90_000,
            gpu_mem_peak_mib: Some(1500),
            host,
            load_ms: 10_000,
            model: job.model.clone(),
            outcome: Outcome::Ok,
            peak_rss_kib: None,
            read: texts.len() as u32,
            readings_bytes: rb.len() as u64,
            readings_sha256: sha256(&rb),
            unit_error: None,
        };
        let files: Vec<PathBuf> = ["rd", "r", "d"]
            .iter()
            .map(|n| dir.join(format!("{name}_{n}")))
            .collect();
        std::fs::write(&files[0], rb).unwrap();
        std::fs::write(&files[1], serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(&files[2], "diag").unwrap();
        files
    }

    struct World {
        _tmp: tempfile::TempDir,
        opts: Options,
        job: Job,
    }

    fn world(a: &[(&str, &str)], b: &[(&str, &str)]) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let job = job();
        let unit = units(&root.join("stage")).join(DOC);
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(JOB_FILE), serde_json::to_vec(&job).unwrap()).unwrap();
        let prov = PageProvenance {
            schema: 1,
            campaign: "x08-eng-pilot".into(),
            doc_id: DOC.into(),
            quality_sha256: sha256(b"q"),
            loc_alto_sha256: sha256(ALTO.as_bytes()),
            page_sha256: sha256(b"jpg"),
            page_width: 1000,
            page_height: 1400,
            params: Params::default(),
            stats: Stats::default(),
            est_fast_secs: 400,
            deadline_secs: 3600,
            stratum: None,
        };
        std::fs::write(
            unit.join(PROVENANCE_FILE),
            serde_json::to_vec(&prov).unwrap(),
        )
        .unwrap();
        let q = root.join("quality").join("pages").join(DOC);
        std::fs::create_dir_all(&q).unwrap();
        std::fs::write(q.join(crate::quality::ALTO_FILE), ALTO).unwrap();
        // Two hosts' results, compared under page-blocks-v1.
        let res = root.join("res");
        std::fs::create_dir_all(&res).unwrap();
        let fa = write_result(&res, &format!("{}_0", job.workunit()), &job, a, "4");
        let fb = write_result(&res, &format!("{}_1", job.workunit()), &job, b, "2");
        let states = root.join("states");
        let _ = compare_with(
            &fa,
            &fb,
            Compare {
                min_dice: 0.5,
                rule: Rule::PageBlocksV1,
                bundle_verdict: BundleVerdict::Unit,
                log: None,
                pages: None,
                crop_states: Some(&states),
                guard_job: None,
            },
        );
        // The canonical result is A's, in the spool as BOINC leaves it; every result is kept.
        let spool = root.join("spool");
        std::fs::create_dir_all(&spool).unwrap();
        for (src, out) in fa.iter().zip(super::super::outputs()) {
            std::fs::copy(src, spool.join(format!("{}__file_{out}", job.workunit()))).unwrap();
        }
        let seen = root.join("seen");
        for (files, n) in [(&fa, 0), (&fb, 1)] {
            let d = seen.join(format!("{}_{n}", job.workunit()));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::copy(&files[0], d.join(super::super::READINGS_FILE)).unwrap();
            std::fs::copy(&files[1], d.join(super::super::RESULT_FILE)).unwrap();
        }
        let _ = load(&fa).unwrap();
        World {
            opts: Options {
                root: root.join("stage"),
                quality_root: root.join("quality"),
                spool,
                crop_states: states,
                seen: Some(seen),
                held: root.join("held"),
                acceptance: None,
                weights_lock: WEIGHTS_LOCK.to_owned(),
            },
            _tmp: tmp,
            job,
        }
    }

    const R1: &str = "The ship sailed for England on Tuesday morning with a cargo of cotton";
    const R2: &str = "Eggs were sold at twelve cents the dozen in town on Saturday";

    #[test]
    fn a_judged_page_is_held_with_provenance_and_rows_and_nothing_is_accepted_without_a_threshold()
    {
        // The hosts agree on two regions of three and not on the second.
        let w = world(
            &[("r0001", R1), ("r0002", R2), ("r0003", R1)],
            &[
                ("r0001", R1),
                ("r0002", "Quite another sentence about coal"),
                ("r0003", R1),
            ],
        );
        let s = run(&w.opts).unwrap();
        assert_eq!(
            (
                s.units,
                s.held,
                s.accepted,
                s.pending,
                s.unjudged,
                s.cost_rows
            ),
            (1, 1, 0, 0, 0, 2)
        );
        let held = w.opts.held.join(DOC);
        let alto = std::fs::read_to_string(held.join(ALTO_OUT)).unwrap();
        assert!(
            alto.contains("PROCESSINGREFS=\"PROC_XIAOMI\"")
                && alto.contains("PROCESSINGREFS=\"PROC_LOC\"")
        );
        assert!(alto.contains("usnm_boinc_result"));
        usnm_ocr_local::checks::check_alto(alto.as_bytes()).unwrap();
        let blocks: serde_json::Value =
            serde_json::from_slice(&std::fs::read(held.join(BLOCKS_OUT)).unwrap()).unwrap();
        assert_eq!(blocks["xiaomi"], 2);
        assert_eq!(blocks["blocks"][0]["source"], "xiaomi");
        assert_eq!(blocks["blocks"][1]["kept"], "disagree");
        let rows: Vec<serde_json::Value> =
            std::fs::read_to_string(w.opts.held.join(OCR_OUTPUTS_FILE))
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["reader"], "ours-xiaomi");
        assert_eq!(rows[0]["source"], "usnm-xiaomi");
        assert_eq!(rows[0]["accepted"], false);
        assert!(rows[0]["engine"].as_str().unwrap().starts_with(
            "xiaomi-ocr-0/bbd8cf8d21f2 profile=q8_0.rv5 image=sha256:aaaaaaaaaaaa models=sha256:"
        ));
        assert!(rows[0]["agree_share_chars"].as_f64().unwrap() > 0.0);
        // Every result of the unit has a cost row: GPU seconds are load plus engine time.
        let costs: Vec<serde_json::Value> = std::fs::read_to_string(w.opts.held.join(COST_FILE))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(costs.len(), 2);
        assert_eq!(costs[0]["gpu_seconds"], 100.0);
        assert_eq!(
            (costs[0]["slots"].as_u64(), costs[1]["slots"].as_u64()),
            (Some(4), Some(2))
        );
        assert_eq!(
            (
                costs[0]["canonical"].as_bool(),
                costs[1]["canonical"].as_bool()
            ),
            (Some(true), Some(false))
        );
        // With a threshold the page's share decides; the share is the agreeing characters.
        let share = rows[0]["agree_share_chars"].as_f64().unwrap();
        let mut accepting = w.opts.clone();
        accepting.acceptance = Some(share);
        assert_eq!(run(&accepting).unwrap().accepted, 1);
        accepting.acceptance = Some((share + 0.01).min(1.0));
        assert_eq!(
            run(&accepting).unwrap().accepted,
            usize::from(share + 0.01 > 1.0)
        );
    }

    #[test]
    fn a_result_nobody_judged_and_a_changed_alto_are_not_held() {
        let all = [("r0001", R1), ("r0002", R2), ("r0003", R1)];
        let w = world(&all, &all);
        // No crop-state record: not held, the strict rule's vouch is never taken.
        let mut none = w.opts.clone();
        none.crop_states = w.opts.held.join("empty-states");
        std::fs::create_dir_all(&none.crop_states).unwrap();
        let s = run(&none).unwrap();
        assert_eq!((s.held, s.unjudged), (0, 1));
        // LoC's ALTO changed since staging.
        let alto = w
            .opts
            .quality_root
            .join("pages")
            .join(DOC)
            .join(crate::quality::ALTO_FILE);
        std::fs::write(&alto, ALTO.replace("shlp", "ship")).unwrap();
        let s = run(&w.opts).unwrap();
        assert_eq!((s.held, s.refused), (0, 1));
        // No result yet: pending, and the held root is not under ocr-local.
        let mut early = w.opts.clone();
        early.spool = w.opts.held.join("no-spool");
        assert_eq!(run(&early).unwrap().pending, 1);
        let mut bad = w.opts.clone();
        bad.held = w.opts.held.join("ocr-local").join("pages");
        assert!(run(&bad).is_err());
        // A symlink into a forbidden tree is refused on what it resolves to.
        let target = w.opts.held.join("ocr-out");
        std::fs::create_dir_all(&target).unwrap();
        let link = w.opts.held.join("innocent");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut linked = w.opts.clone();
        linked.held = link;
        let err = run(&linked).unwrap_err().to_string();
        assert!(err.contains("resolves to"), "{err}");
        // BOINC giving the unit up settles it as invalid instead of leaving it pending.
        let mut given_up = w.opts.clone();
        given_up.spool = w.opts.held.join("gave-up-spool");
        std::fs::create_dir_all(&given_up.spool).unwrap();
        std::fs::write(
            given_up.spool.join(format!("{}_error", w.job.workunit())),
            "",
        )
        .unwrap();
        let s = run(&given_up).unwrap();
        assert_eq!((s.pending, s.gave_up, s.units_invalid), (0, 1, 1));
        assert_eq!(s.pages_invalid, s.pages);
        // Without --seen there is no result identity, so no cost row is made.
        let mut unseen = w.opts.clone();
        unseen.seen = None;
        assert_eq!(run(&unseen).unwrap().cost_rows, 0);
    }

    #[test]
    fn the_tag_is_the_designs_and_parses() {
        let tag = engine_tag(&engine(), WEIGHTS_LOCK).unwrap().to_string();
        let digest = models_digest(WEIGHTS_LOCK).unwrap();
        assert_eq!(
            tag,
            format!(
                "xiaomi-ocr-0/bbd8cf8d21f2 profile=q8_0.rv5 image=sha256:aaaaaaaaaaaa models=sha256:{digest} route=boinc-page@r1 contract=0"
            )
        );
        assert!(usnm_core::ocr_engine::EngineTag::parse(&tag).is_ok());
        assert!(models_digest("nothing").is_err());
    }
}
