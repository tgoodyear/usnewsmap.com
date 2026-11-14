//! What stock `script_validator` runs for `usnm_qwen`.
//!
//! It passes a result's three files in output template order
//! (`readings.json`, `result.json`, `diag.txt`) and reads the exit status.
//! `check` looks at one result; with `--keep` it also copies the result
//! into that directory, named after its uploaded `result.json`. `compare`
//! looks at two results of one unit (per-crop word-bag Dice of at least
//! the threshold, both read or neither) and, with `--log`, appends what it
//! found to a JSON Lines file, so the threshold can be measured from every
//! compared pair. The log line's `wordless` lists the crops that pass only
//! because neither reading has a letter or digit ([`wordless`], #1463); no
//! verdict reads it. A bundle unit's pair is also judged page by page
//! (`qwen::agree_pages`); with `--pages`, each page's verdict and both
//! hosts' readings of it are written once under
//! `<pages>/<job_id>/<doc_id>/<pair>.json`, so a page both hosts agree on is
//! kept even while another page leaves the unit inconclusive. Under the
//! default bundle verdict (`unit`) the unit's verdict, and so BOINC's
//! validate and canonical state, still needs every page. Under `page-v1`
//! (#1435) a bundle pair that judged every page, wrote every page record and
//! agreed on at least half of the pages settles the unit: the pair passes,
//! so BOINC makes it canonical and credits both results, the pages' own
//! records say which pages are accepted, and `qwen stage --page-records`
//! packs the disagreeing pages into a new bundle.
use super::{DIAG_FILE, READINGS_FILE, RESULT_FILE};
use crate::read;
use anyhow::{ensure, Context};
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::qwen::{
    self, Readings, ResultManifest, MAX_DIAG, MAX_READINGS, MAX_RESULT_JSON,
};

pub struct Loaded {
    pub result: ResultManifest,
    pub readings: Readings,
    pub readings_bytes: Vec<u8>,
    pub diag: Vec<u8>,
}

pub fn load(files: &[PathBuf]) -> anyhow::Result<Loaded> {
    ensure!(
        files.len() == 3,
        "expected 3 output files ({READINGS_FILE}, {RESULT_FILE}, {DIAG_FILE}), got {}",
        files.len()
    );
    let readings_bytes = read(&files[0], MAX_READINGS)?;
    let result: ResultManifest =
        serde_json::from_slice(&read(&files[1], MAX_RESULT_JSON)?).context("result.json")?;
    let diag = read(&files[2], MAX_DIAG)?;
    let readings = result.check(&readings_bytes, &diag)?;
    Ok(Loaded {
        result,
        readings,
        readings_bytes,
        diag,
    })
}

/// Whether a result is a benchmark bundle's (`qwen benchmark-stage --bundle-size`, #968): it
/// read at least one crop and every crop it read is `<doc_id>:<crop id>` with a doc id naming a
/// dataset sample or a benchmark crop, never a LoC page. The validator judges such a pair as it
/// judges any other but keeps nothing of it (no `seen` copy, crop states, page records, unit
/// verdict or pair log line), so no production collector or publish can pick a benchmark up.
pub fn is_benchmark(l: &Loaded) -> bool {
    use super::doc_id::DocId;
    use usnm_core::page_id::Source;
    let not_loc = |doc: &str| match DocId::parse(doc) {
        Ok(DocId::LegacyBenchmark { .. }) => true,
        Ok(DocId::Page(p)) => p.source() == Source::Sample,
        Err(_) => false,
    };
    !l.readings.readings.is_empty()
        && l.readings
            .readings
            .iter()
            .all(|r| r.id.split_once(':').is_some_and(|(doc, _)| not_loc(doc)))
}

pub fn check(files: &[PathBuf], keep: Option<&Path>) -> anyhow::Result<()> {
    check_with(files, keep, Guard::default())
}

/// The most crop readings a finished result may leave empty, as a percentage
/// of the crops counted: more than this is invalid (the empty-return guard).
pub const MAX_EMPTY_PERCENT: usize = 10;

/// What the empty-return guard counted in one result.
///
/// A reading is *empty* when its text has nothing left after trimming (the
/// analysis's "0 tokens after trimming"). A *looped* crop (the result's
/// loop-guard bit, or a repeated run ending the reading) is a different
/// failure and is counted apart, never as empty. Crops whose LoC text is
/// blank too are left out of both sides of the share: a blank reading of a
/// blank crop is right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyCounts {
    pub crops: usize,
    pub empty: usize,
    pub looped: usize,
    pub loc_blank: usize,
    /// Whether LoC's text per crop was known. When it was not, `loc_blank`
    /// is 0 and every empty reading counts.
    pub loc_known: bool,
}

impl EmptyCounts {
    /// Crops that count toward the share.
    pub fn counted(&self) -> usize {
        self.crops - self.loc_blank
    }

    /// Whether more than [`MAX_EMPTY_PERCENT`] of the counted crops are empty.
    pub fn too_empty(&self) -> bool {
        self.empty * 100 > self.counted() * MAX_EMPTY_PERCENT
    }
}

/// Counts a result's empty readings. `loc_blank` maps a crop id to whether
/// LoC's text for it is blank, when the unit's job was found.
pub fn count_empty(l: &Loaded, loc_blank: Option<&BTreeMap<String, bool>>) -> EmptyCounts {
    let sides = super::consensus::sides(l);
    let mut c = EmptyCounts {
        crops: l.readings.readings.len(),
        empty: 0,
        looped: 0,
        loc_blank: 0,
        loc_known: loc_blank.is_some(),
    };
    for r in &l.readings.readings {
        if loc_blank.is_some_and(|m| m.get(&r.id).copied().unwrap_or(false)) {
            c.loc_blank += 1;
        } else if sides.get(r.id.as_str()).is_some_and(|s| s.looped) {
            c.looped += 1;
        } else if r.text.trim().is_empty() {
            c.empty += 1;
        }
    }
    c
}

/// Which crops of the unit LoC's text is blank for, read from its `qwen.json`.
/// None when the file is missing or is not this result's job, so the guard
/// falls back to counting every empty reading. A bundle's crop ids carry the
/// `<doc_id>:` prefix in the job's flattened list, as in the readings.
pub fn loc_blank_crops(job: &Path, job_id: &str) -> Option<BTreeMap<String, bool>> {
    let job: qwen::Job = serde_json::from_slice(&read(job, qwen::MAX_JOB_JSON).ok()?).ok()?;
    (job.id == job_id).then(|| {
        job.crops
            .iter()
            .map(|c| (c.id.clone(), c.loc_text.trim().is_empty()))
            .collect()
    })
}

/// Finds the unit's `qwen.json` in a BOINC project's download tree with the
/// project's own `bin/dir_hier_path`.
pub fn project_job_file(project: &Path, job_id: &str) -> Option<PathBuf> {
    let out = std::process::Command::new(project.join("bin/dir_hier_path"))
        .arg(format!("{job_id}_qwen_json"))
        .current_dir(project)
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    // `dir_hier_path` may print a path relative to the project, and the
    // validator runs in `<project>/tmp_<host>`.
    let path = project.join(path);
    (out.status.success() && path.is_file()).then_some(path)
}

/// Where the guard looks for LoC's text per crop.
#[derive(Debug, Clone, Copy, Default)]
pub struct Guard<'a> {
    /// The unit's `qwen.json`.
    pub job: Option<&'a Path>,
    /// A BOINC project whose download tree holds it.
    pub project: Option<&'a Path>,
}

/// `check`, then the empty-return guard on a finished, non-benchmark
/// result: more than 10% empty readings (see [`EmptyCounts`]) is invalid.
/// The result is kept first, so an invalid one stays on record. One log line
/// says what was counted. A benchmark result and one that did not finish
/// (its outcome already says so) are not guarded.
pub fn check_with(files: &[PathBuf], keep: Option<&Path>, guard: Guard<'_>) -> anyhow::Result<()> {
    let l = load(files)?;
    let benchmark = is_benchmark(&l);
    if let Some(dir) = keep.filter(|_| !benchmark) {
        super::retain::capture(files, &l, dir).map_err(super::retain::CaptureError)?;
    }
    if benchmark || l.result.outcome != qwen::Outcome::Ok {
        return Ok(());
    }
    let job = guard.job.map(Path::to_path_buf).or_else(|| {
        guard
            .project
            .and_then(|p| project_job_file(p, &l.result.job_id))
    });
    let loc = job.and_then(|j| loc_blank_crops(&j, &l.result.job_id));
    let c = count_empty(&l, loc.as_ref());
    let invalid = c.too_empty();
    eprintln!(
        "usnm_qwen_check: empty-guard job={} model={} device={} crops={} empty={} looped={} loc_blank_excluded={} counted={} loc_text={} verdict={}",
        l.result.job_id.get(..12).unwrap_or(&l.result.job_id),
        l.result.model.as_deref().unwrap_or("none"),
        l.result.device,
        c.crops,
        c.empty,
        c.looped,
        c.loc_blank,
        c.counted(),
        if c.loc_known { "known" } else { "unknown" },
        if invalid { "invalid" } else { "ok" },
    );
    anyhow::ensure!(
        !invalid,
        "empty-return guard: {} of {} counted crops read empty, over {MAX_EMPTY_PERCENT}%",
        c.empty,
        c.counted()
    );
    Ok(())
}

/// How `compare` judges a pair. Every rule writes the same pair log; the
/// bundle-page verdicts stay on the strict rule's per-crop Dice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Rule {
    /// Every crop at the Dice floor: the rule in force.
    StrictV1,
    /// The strict rule where a crop with no word on either side (#1463)
    /// is neutral, not agreement: a pair or bundle page agrees only when at
    /// least one crop with a word on some side passed the Dice floor. Off
    /// until the validator wrapper passes it.
    StrictV2,
    /// Crop by crop; see [`super::consensus`]. Not in force until the
    /// validator wrapper passes it.
    CropConsensusV2,
}

/// What settles a bundle unit's pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum BundleVerdict {
    /// The unit's rule over every page at once: one disagreeing page leaves
    /// the unit inconclusive.
    Unit,
    /// Each page on its own (#1435): the pair passes once every page's
    /// record is written and at least half of the pages agree.
    PageV1,
}

/// Under [`BundleVerdict::PageV1`], the file the passing pair writes in
/// `<pages>/<job_id>/`, so staging knows the unit is settled and its
/// disagreeing pages can be packed again.
pub const UNIT_VERDICT_FILE: &str = "unit-verdict.json";

/// Whether `agreed` of `pages` judged pages settle a bundle under `page-v1`:
/// at least one, and at least half.
pub fn pages_settle(agreed: usize, pages: usize) -> bool {
    agreed > 0 && 2 * agreed >= pages
}

pub struct Compare<'a> {
    pub min_dice: f64,
    pub rule: Rule,
    pub bundle_verdict: BundleVerdict,
    pub log: Option<&'a Path>,
    pub pages: Option<&'a Path>,
    /// Under `crop-consensus-v2`, where each pair's crop states are kept.
    pub crop_states: Option<&'a Path>,
}

/// The strict rule, as the validator wrapper runs it.
pub fn compare(
    a: &[PathBuf],
    b: &[PathBuf],
    min_dice: f64,
    log: Option<&Path>,
    pages: Option<&Path>,
) -> anyhow::Result<()> {
    compare_with(
        a,
        b,
        Compare {
            min_dice,
            rule: Rule::StrictV1,
            bundle_verdict: BundleVerdict::Unit,
            log,
            pages,
            crop_states: None,
        },
    )
}

pub fn compare_with(a: &[PathBuf], b: &[PathBuf], opts: Compare<'_>) -> anyhow::Result<()> {
    let Compare {
        min_dice,
        rule,
        bundle_verdict,
        log,
        pages,
        crop_states,
    } = opts;
    anyhow::ensure!(
        bundle_verdict == BundleVerdict::Unit || pages.is_some(),
        "--bundle-verdict page-v1 needs --pages"
    );
    // The collector reads the kept states to learn which crops a pair vouched for.
    anyhow::ensure!(
        rule != Rule::CropConsensusV2 || crop_states.is_some(),
        "--rule crop-consensus-v2 needs --crop-states"
    );
    let (la, lb) = (load(a)?, load(b)?);
    // A benchmark pair is judged in memory only (see [`is_benchmark`]).
    let benchmark = is_benchmark(&la) || is_benchmark(&lb);
    let (log, pages, crop_states) = if benchmark {
        (None, None, None)
    } else {
        (log, pages, crop_states)
    };
    let (crops, mut strict) =
        qwen::agree(&la.result, &la.readings, &lb.result, &lb.readings, min_dice);
    let worded = (rule == Rule::StrictV2).then(|| worded_crops(&crops, &la, &lb));
    if let Some(worded) = &worded {
        let finished = [&la, &lb].iter().all(|l| {
            l.result.outcome == usnm_ocr_contract::qwen::Outcome::Ok
                && l.result.unit_error.is_none()
        });
        if strict.is_ok() && finished && worded.is_empty() {
            strict = Err("no crop with a word on either side".into());
        }
    }
    let consensus = (rule == Rule::CropConsensusV2).then(|| {
        super::consensus::judge(&la, &lb, super::consensus::Params::IN_RULE, strict.clone())
    });
    let mut verdict = consensus
        .as_ref()
        .map_or_else(|| strict.clone(), |j| j.verdict.clone());
    // The rule that decided the pair, as the pair log reports it.
    let mut basis = consensus.as_ref().map(|j| j.basis);
    if let (Some(judged), Some(dir)) = (&consensus, crop_states) {
        // Without its record the collector would take the result as the strict
        // rule accepted it, crop for crop, so the strict verdict decides instead.
        if let Err(e) = super::consensus::record(dir, a, b, &la, &lb, judged, verdict.is_ok()) {
            eprintln!("crop states not written, the strict rule decides: {e:#}");
            verdict = strict.clone();
            basis = Some(super::consensus::Basis::StrictV1);
        }
    }
    let mut page_verdicts = qwen::agree_pages(&la.result, &lb.result, &crops, min_dice);
    if let Some(worded) = &worded {
        // Any page that does not agree, a demoted one or a wholly unread one,
        // leaves the unit inconclusive, so under `page-v1` the pages' own count
        // decides it and the settlement marker is written.
        demote_wordless_pages(&mut page_verdicts, worded);
        if verdict.is_ok() {
            if let Some(p) = page_verdicts.iter().find(|p| !p.agree) {
                verdict = Err(p
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("{}: page does not agree", p.doc_id)));
            }
        }
    }
    let names = pages.filter(|_| !page_verdicts.is_empty()).and_then(|dir| {
        match (
            crate::quality::validate::result_name(&a[1]),
            crate::quality::validate::result_name(&b[1]),
        ) {
            (Ok(x), Ok(y)) => Some((dir, [x, y])),
            (x, y) => {
                // Like a record that cannot be written: no page is kept,
                // and the unit's verdict is unchanged.
                eprintln!(
                    "page records not written: {:#}",
                    x.err().or(y.err()).expect("one name failed")
                );
                None
            }
        }
    });
    let mut all_recorded = names.is_some();
    if let Some((dir, names)) = &names {
        for page in &page_verdicts {
            // A page record that cannot be written leaves that page unkept;
            // under the unit verdict it must not change the unit's verdict,
            // and under `page-v1` the pair cannot settle the unit.
            if let Err(e) = record_page(dir, page, min_dice, [(&names[0], &la), (&names[1], &lb)]) {
                eprintln!("{}: page record not written: {e:#}", page.doc_id);
                all_recorded = false;
            }
        }
    }
    let agreed_pages = page_verdicts.iter().filter(|p| p.agree).count();
    // Whether this pair settled the bundle by its pages (`page-v1`).
    let mut settled_by_pages = false;
    if benchmark
        && bundle_verdict == BundleVerdict::PageV1
        && verdict.is_err()
        && !page_verdicts.is_empty()
        && same_crops(&la, &lb)
        && pages_settle(agreed_pages, page_verdicts.len())
    {
        // A benchmark bundle settles by its pages as any bundle does, in memory: nothing
        // repacks its disagreeing pages, so it needs no marker.
        verdict = Ok(());
        settled_by_pages = true;
    } else if bundle_verdict == BundleVerdict::PageV1
        && verdict.is_err()
        && all_recorded
        && same_crops(&la, &lb)
        && pages_settle(agreed_pages, page_verdicts.len())
    {
        let (dir, names) = names.as_ref().expect("all_recorded needs names");
        match record_unit_verdict(dir, &la.result.job_id, names, &page_verdicts) {
            Ok(()) => {
                verdict = Ok(());
                settled_by_pages = true;
            }
            // Without the marker staging would never retry its disagreeing
            // pages, so the unit's rule decides as before.
            Err(e) => eprintln!("unit verdict not written, the unit's rule decides: {e:#}"),
        }
    }
    if let Some(log) = log {
        let mut line = serde_json::json!({
            "a": {"result": crate::quality::validate::result_name(&a[1])?, "device": la.result.device, "model": la.result.model, "gpu": la.result.host.gpu, "plan_class": la.result.host.plan_class},
            "b": {"result": crate::quality::validate::result_name(&b[1])?, "device": lb.result.device, "model": lb.result.model, "gpu": lb.result.host.gpu, "plan_class": lb.result.host.plan_class},
            "agree": verdict.is_ok(),
            "at": chrono::Utc::now().to_rfc3339(),
            "crops": crops,
            "job_id": la.result.job_id,
            "min_dice": min_dice,
            "outcomes": [la.result.outcome, lb.result.outcome],
            "pages": page_verdicts.iter().map(|p| serde_json::json!({"doc_id": p.doc_id, "agree": p.agree, "reason": p.reason})).collect::<Vec<_>>(),
            "bundle_verdict": match bundle_verdict { BundleVerdict::Unit => "unit", BundleVerdict::PageV1 => "page-v1" },
            "settled_by_pages": settled_by_pages,
            "reason": verdict.as_ref().err(),
            "wordless": wordless(&crops, &la, &lb),
        });
        if rule == Rule::StrictV2 {
            line["rule"] = "strict-v2".into();
        }
        if let Some(judged) = &consensus {
            line["rule"] = "crop-consensus-v2".into();
            line["basis"] = serde_json::to_value(basis.unwrap_or(judged.basis))?;
            line["strict_reason"] = serde_json::json!(strict.as_ref().err());
            line["crop_states"] = serde_json::to_value(&judged.crops)?;
        }
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?;
        // One write, so two validators appending at once cannot interleave a line.
        f.write_all(format!("{line}\n").as_bytes())?;
    }
    verdict.map_err(|e| anyhow::anyhow!(e))
}

/// The ids of the crops both results read where at least one reading has a
/// letter or digit: what `strict-v2` counts as agreement. The rest are
/// neutral (#1463).
fn worded_crops<'a>(
    crops: &'a [qwen::CropAgreement],
    a: &Loaded,
    b: &Loaded,
) -> std::collections::HashSet<&'a str> {
    let no_word = |l: &Loaded, id: &str| {
        l.readings
            .readings
            .iter()
            .find(|r| r.id == id)
            .is_none_or(|r| !r.text.chars().any(char::is_alphanumeric))
    };
    crops
        .iter()
        .filter(|c| c.read == (true, true) && !(no_word(a, &c.id) && no_word(b, &c.id)))
        .map(|c| c.id.as_str())
        .collect()
}

/// Under `strict-v2`, a page that agreed only on crops with no word on
/// either side no longer agrees.
fn demote_wordless_pages(
    pages: &mut [qwen::PageAgreement],
    worded: &std::collections::HashSet<&str>,
) {
    for p in pages.iter_mut().filter(|p| p.agree) {
        if !p.crops.iter().any(|c| worded.contains(c.id.as_str())) {
            p.agree = false;
            p.reason = Some(format!("{}: no crop with a word on either side", p.doc_id));
        }
    }
}

/// The crops both results read where neither reading has a letter or digit
/// (#1463): both word bags are empty, so the strict rule's Dice is 1.0
/// whatever the two readings say. Each names the crop, whether the readings
/// are the same once whitespace is collapsed, and whether each side looped
/// as `crop-consensus-v2` marks it (the result's loop-guard bit, or a
/// repeated run ending the reading). For the pair log only.
fn wordless(crops: &[qwen::CropAgreement], a: &Loaded, b: &Loaded) -> Vec<serde_json::Value> {
    let no_word = |l: &Loaded, id: &str| {
        l.readings
            .readings
            .iter()
            .find(|r| r.id == id)
            .is_some_and(|r| !r.text.chars().any(char::is_alphanumeric))
    };
    let ids: Vec<&str> = crops
        .iter()
        .filter(|c| c.read == (true, true) && no_word(a, &c.id) && no_word(b, &c.id))
        .map(|c| c.id.as_str())
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let (sa, sb) = (super::consensus::sides(a), super::consensus::sides(b));
    ids.into_iter()
        .filter_map(|id| {
            let (x, y) = (sa.get(id)?, sb.get(id)?);
            Some(serde_json::json!({
                "id": id,
                "identical": x.text.split_whitespace().eq(y.text.split_whitespace()),
                "looped": [x.looped, y.looped],
            }))
        })
        .collect()
}

/// Whether both results read the same crops, each one once, as many as
/// each says the unit has. Crop counts are the hosts' own, so a host that
/// left a crop out could otherwise settle a unit whose page the ledger then
/// never counts.
fn same_crops(a: &Loaded, b: &Loaded) -> bool {
    fn ids(l: &Loaded) -> Vec<&str> {
        let mut ids: Vec<&str> = l.readings.readings.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        ids
    }
    let (x, y) = (ids(a), ids(b));
    let unique = x.windows(2).all(|w| w[0] != w[1]);
    unique && x == y && a.result.crops == b.result.crops && x.len() == a.result.crops as usize
}

/// Writes `<pages>/<job_id>/unit-verdict.json` for a bundle pair that
/// settled its unit under `page-v1`: the pair, by result name, and each
/// page's verdict. The first pair to settle the unit keeps the file; a later
/// pair finding it there (BOINC compares later results with the canonical
/// one) leaves it.
fn record_unit_verdict(
    dir: &Path,
    job_id: &str,
    names: &[String; 2],
    pages: &[qwen::PageAgreement],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        usnm_ocr_contract::boinc::is_sha256(job_id),
        "invalid qwen job id {job_id:?}"
    );
    let mut pair = [names[0].as_str(), names[1].as_str()];
    pair.sort_unstable();
    let record = serde_json::json!({
        "v": 1,
        "job_id": job_id,
        "bundle_verdict": "page-v1",
        "results": pair,
        "pages": pages.iter().map(|p| serde_json::json!({"doc_id": p.doc_id, "agree": p.agree})).collect::<Vec<_>>(),
        "accuracy_established": false,
    });
    let parent = dir.join(job_id);
    std::fs::create_dir_all(&parent)?;
    let mut f = tempfile::NamedTempFile::new_in(&parent)?;
    f.write_all(&serde_json::to_vec_pretty(&record)?)?;
    f.as_file().sync_all()?;
    match f.persist_noclobber(parent.join(UNIT_VERDICT_FILE)) {
        Ok(_) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.error.into()),
    }
    // The pass must not outlive the marker: the file's entry in the job's
    // directory and that directory's entry in `dir` both reach the disk
    // before the pair is reported as agreeing, a marker found there included.
    std::fs::File::open(&parent)?.sync_all()?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// What a page record says: `page_agreed` when both hosts read the page
/// alike, else `page_disagreed`. Neither establishes accuracy.
pub const PAGE_AGREED: &str = "page_agreed";
pub const PAGE_DISAGREED: &str = "page_disagreed";

/// Writes one bundle page's verdict and both hosts' readings of it, once:
/// the record holds no time, so the same pair compared again writes the
/// same bytes, and a record already there is never replaced.
fn record_page(
    dir: &Path,
    page: &qwen::PageAgreement,
    min_dice: f64,
    results: [(&String, &Loaded); 2],
) -> anyhow::Result<()> {
    let mut results = results;
    let mut page = page.clone();
    if results[1].0 < results[0].0 {
        // One order for the whole record, crops' read flags included, so a
        // pair compared either way round writes the same bytes.
        results.swap(0, 1);
        for c in &mut page.crops {
            c.read = (c.read.1, c.read.0);
        }
    }
    let page = &page;
    let job_id = &results[0].1.result.job_id;
    usnm_ocr_contract::quality::check_doc_id(&page.doc_id)?;
    let prefix = format!("{}:", page.doc_id);
    let sides: Vec<serde_json::Value> = results
        .iter()
        .map(|(name, l)| {
            serde_json::json!({
                "result": name,
                "readings_sha256": l.result.readings_sha256,
                "device": l.result.device,
                "model": l.result.model,
                "plan_class": l.result.host.plan_class,
                "outcome": l.result.outcome,
                "readings": l.readings.readings.iter().filter(|r| r.id.starts_with(&prefix)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let record = serde_json::json!({
        "v": 1,
        "job_id": job_id,
        "doc_id": page.doc_id,
        "status": if page.agree { PAGE_AGREED } else { PAGE_DISAGREED },
        "reason": page.reason,
        "min_dice": min_dice,
        "crops": page.crops,
        "results": sides,
        "accuracy_established": false,
    });
    let bytes = serde_json::to_vec_pretty(&record)?;
    let pair =
        usnm_ocr_contract::boinc::sha256(format!("{}\n{}", results[0].0, results[1].0).as_bytes());
    let path = dir
        .join(job_id)
        .join(&page.doc_id)
        .join(format!("{}.json", &pair[..16]));
    let parent = path.parent().context("page record directory")?;
    std::fs::create_dir_all(parent)?;
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    f.write_all(&bytes)?;
    f.as_file().sync_all()?;
    // Never replaces a record, even one a concurrent comparison published
    // after this one started.
    match f.persist_noclobber(&path) {
        Ok(_) => {
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        }
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::ensure!(
                std::fs::read(&path)? == bytes,
                "{} already holds another record",
                path.display()
            );
            Ok(())
        }
        Err(e) => Err(e.error.into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use usnm_ocr_contract::{
        boinc::sha256,
        qwen::{Outcome, Reading, PROMPT_VERSION, VERSION},
    };

    pub(crate) fn write(dir: &Path, tag: &str, texts: &[(&str, &str)]) -> Vec<PathBuf> {
        write_crops(dir, tag, texts, None)
    }

    /// `write`, with the manifest saying the unit has `crops` crops when given
    /// (a unit of more crops than the local job cuts).
    pub(crate) fn write_crops(
        dir: &Path,
        tag: &str,
        texts: &[(&str, &str)],
        crops: Option<u32>,
    ) -> Vec<PathBuf> {
        let tmp = dir.join(format!("unit_{tag}"));
        let unit = dir.join(format!("q_{tag}"));
        crate::quality::work::tests::unit(&unit);
        let j = crate::qwen::stage::local_unit(
            &unit.join("page.jpg"),
            &unit.join("loc.xml"),
            &tmp,
            crate::qwen::stage::tests::engine(),
            1,
            4,
            600,
            4,
        )
        .unwrap();
        let readings = Readings {
            v: VERSION,
            job_id: j.id.clone(),
            device: "cuda".into(),
            diag: BTreeMap::new(),
            model: Some("qwen3-vl-8b-q4km".into()),
            outcome: Outcome::Ok,
            prompt_version: PROMPT_VERSION,
            readings: texts
                .iter()
                .map(|(id, t)| Reading {
                    id: id.to_string(),
                    ms: 1,
                    text: t.to_string(),
                    tokens: 3,
                })
                .collect(),
        };
        let rb = serde_json::to_vec(&readings).unwrap();
        let r = ResultManifest {
            v: VERSION,
            job_id: j.id,
            completed_at: "2026-10-14T00:00:00Z".into(),
            crops: crops.unwrap_or(j.crops.len() as u32),
            device: "cuda".into(),
            diag_bytes: 4,
            diag_sha256: sha256(b"diag"),
            engine: j.engine,
            engine_ms: 5,
            gpu_mem_peak_mib: None,
            host: Default::default(),
            load_ms: 1,
            model: Some("qwen3-vl-8b-q4km".into()),
            outcome: Outcome::Ok,
            peak_rss_kib: None,
            read: texts.len() as u32,
            readings_bytes: rb.len() as u64,
            readings_sha256: sha256(&rb),
            unit_error: None,
        };
        let files: Vec<PathBuf> = ["rd", "r", "d"]
            .iter()
            .map(|n| dir.join(format!("usnm_qwen_x_{tag}_{n}")))
            .collect();
        std::fs::write(&files[0], rb).unwrap();
        std::fs::write(&files[1], serde_json::to_vec(&r).unwrap()).unwrap();
        std::fs::write(&files[2], "diag").unwrap();
        files
    }

    // The empty-return guard (#1919).

    fn loaded(texts: &[(String, String)], loop_bits: Option<&str>) -> Loaded {
        let tmp = tempfile::tempdir().unwrap();
        let mut l = load(&write_crops(
            tmp.path(),
            "g",
            &borrow(texts),
            Some(texts.len() as u32),
        ))
        .unwrap();
        if let Some(bits) = loop_bits {
            l.readings
                .diag
                .insert("repetition_loop_crop_bits".into(), bits.into());
        }
        l
    }

    fn borrow(v: &[(String, String)]) -> Vec<(&str, &str)> {
        v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }

    /// `n` crops of which the first `empty` read blank.
    fn crops(n: usize, empty: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| {
                let text = if i < empty {
                    " \n\t ".to_string()
                } else {
                    format!("word{i} more")
                };
                (format!("c{i:03}"), text)
            })
            .collect()
    }

    /// LoC blank flags for crops `blank` of `texts`.
    fn loc(texts: &[(String, String)], blank: impl Fn(usize) -> bool) -> BTreeMap<String, bool> {
        texts
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id.clone(), blank(i)))
            .collect()
    }

    #[test]
    fn nine_percent_empty_passes_and_eleven_is_invalid() {
        for (empty, invalid) in [(9, false), (10, false), (11, true)] {
            let c = count_empty(&loaded(&crops(100, empty), None), None);
            assert_eq!((c.crops, c.empty, c.looped), (100, empty, 0));
            assert_eq!(c.too_empty(), invalid, "{empty} of 100");
        }
    }

    #[test]
    fn crops_loc_has_blank_are_left_out_of_both_sides() {
        // 100 crops, 12 read blank: 12%, invalid on its own.
        let texts = crops(100, 12);
        let l = loaded(&texts, None);
        assert!(count_empty(&l, None).too_empty());
        // LoC is blank for five of the empty ones: 7 of 95 is 7.4%.
        let c = count_empty(&l, Some(&loc(&texts, |i| i < 5)));
        assert_eq!((c.empty, c.loc_blank, c.counted()), (7, 5, 95));
        assert!(c.loc_known && !c.too_empty());
        // LoC blank on crops that were read leaves the share of the rest: 12 of 90.
        let c = count_empty(&l, Some(&loc(&texts, |i| i >= 90)));
        assert_eq!((c.empty, c.loc_blank, c.counted()), (12, 10, 90));
        assert!(c.too_empty());
        // LoC blank everywhere leaves nothing to count.
        let c = count_empty(&l, Some(&loc(&texts, |_| true)));
        assert_eq!((c.empty, c.counted()), (0, 0));
        assert!(!c.too_empty());
    }

    #[test]
    fn looped_crops_are_not_empty() {
        let mut texts = crops(10, 0);
        texts[1].1 = "[?] ".repeat(40);
        let c = count_empty(&loaded(&texts, Some("01")), None);
        assert_eq!((c.empty, c.looped), (0, 2));
        assert!(!c.too_empty());
        texts[3].1 = String::new();
        texts[5].1 = String::new();
        let c = count_empty(&loaded(&texts, None), None);
        assert_eq!((c.empty, c.looped), (2, 1));
        assert!(c.too_empty());
    }

    #[test]
    fn check_judges_an_empty_heavy_return_on_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let keep = tmp.path().join("seen");
        let bad = write_crops(tmp.path(), "bad", &borrow(&crops(20, 3)), Some(20));
        let err = check_with(&bad, Some(&keep), Guard::default()).unwrap_err();
        assert!(format!("{err}").contains("3 of 20 counted crops"), "{err}");
        // The invalid result is still kept, and is not a transient failure.
        assert!(keep.join("usnm_qwen_x_bad_r/readings.json").exists());
        assert!(err
            .downcast_ref::<super::super::retain::CaptureError>()
            .is_none());
        assert!(check(&bad, None).is_err());
        let good = write_crops(tmp.path(), "good", &borrow(&crops(20, 2)), Some(20));
        check_with(&good, Some(&keep), Guard::default()).unwrap();
    }

    #[test]
    fn a_39_percent_empty_return_is_invalid_before_any_pairing() {
        // WU 54155's pattern: init rejects the return, so page-v1 settling
        // never sees it as a result to settle.
        let tmp = tempfile::tempdir().unwrap();
        let texts = crops(100, 39);
        let files = write_crops(tmp.path(), "wu", &borrow(&texts), Some(100));
        let err = check_with(&files, None, Guard::default()).unwrap_err();
        assert!(
            format!("{err}").contains("39 of 100 counted crops"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_project_relative_job_path_is_found_from_another_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        std::fs::create_dir_all(project.join("download/ab")).unwrap();
        std::fs::write(project.join("download/ab/job_qwen_json"), b"{}").unwrap();
        let tool = project.join("bin/dir_hier_path");
        std::fs::write(&tool, "#!/bin/sh\necho download/ab/job_qwen_json\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The test's own working directory is not the project, as under the validator.
        assert_eq!(
            project_job_file(&project, &"0".repeat(64)),
            Some(project.join("download/ab/job_qwen_json"))
        );
    }

    #[test]
    fn check_reads_loc_blank_crops_from_the_units_job() {
        let tmp = tempfile::tempdir().unwrap();
        let texts = crops(20, 3);
        let files = write_crops(tmp.path(), "loc", &borrow(&texts), Some(20));
        let job_id = load(&files).unwrap().result.job_id;
        let job_file = tmp.path().join("unit_loc").join("qwen.json");
        let mut job: qwen::Job =
            serde_json::from_slice(&std::fs::read(&job_file).unwrap()).unwrap();
        // A job of this result's identity whose LoC text is blank on the three blank crops.
        job.id = job_id.clone();
        job.crops = texts
            .iter()
            .enumerate()
            .map(|(i, (id, _))| qwen::Crop {
                rect: [0, 0, 10, 10],
                id: id.clone(),
                loc_text: if i < 3 { "  ".into() } else { "words".into() },
                stratum: "s".into(),
            })
            .collect();
        let path = tmp.path().join("job.json");
        std::fs::write(&path, serde_json::to_vec(&job).unwrap()).unwrap();
        let map = loc_blank_crops(&path, &job_id).unwrap();
        assert_eq!(map.values().filter(|b| **b).count(), 3);
        let guard = Guard {
            job: Some(&path),
            project: None,
        };
        check_with(&files, None, guard).unwrap();
        assert!(check_with(&files, None, Guard::default()).is_err());
        // Another job's file is not used: the guard counts every empty reading.
        assert!(loc_blank_crops(&path, &"0".repeat(64)).is_none());
        assert!(loc_blank_crops(&tmp.path().join("missing"), &job_id).is_none());
    }

    #[test]
    fn keeps_results_and_logs_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(
            tmp.path(),
            "a",
            &[
                ("c01", "one two three four"),
                ("c02", "five six seven eight"),
            ],
        );
        let b = write(
            tmp.path(),
            "b",
            &[
                ("c01", "one two three five"),
                ("c02", "five six seven eight"),
            ],
        );
        let keep = tmp.path().join("seen");
        check(&a, Some(&keep)).unwrap();
        assert!(keep.join("usnm_qwen_x_a_r/readings.json").exists());
        let log = tmp.path().join("pairs.jsonl");
        compare(&a, &b, 0.7, Some(&log), None).unwrap();
        assert!(compare(&a, &b, 0.9, Some(&log), None).is_err());
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["crops"][0]["dice"], 0.75);
        assert_eq!(lines[1]["agree"], false);
        // The strict rule's log line keeps the shape every reader of it knows.
        assert!(lines[0].get("rule").is_none() && lines[0].get("crop_states").is_none());
        std::fs::write(&a[0], "tampered").unwrap();
        assert!(check(&a, None).is_err());
    }

    const LONG_A: &str = "the evening paper printed the news of the harbour and the market today";
    const LONG_B: &str =
        "five ships came into port on tuesday with grain and timber from the north";

    #[test]
    fn a_benchmark_bundle_pair_is_judged_but_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let texts = |doc_a: &str, doc_b: &str| {
            [
                (format!("{doc_a}:c01"), LONG_A.to_string()),
                (format!("{doc_b}:c02"), LONG_B.to_string()),
            ]
        };
        let write_owned = |tag: &str, t: &[(String, String)]| {
            let refs: Vec<(&str, &str)> = t.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let files = write(tmp.path(), tag, &refs);
            // A bundle result lists exactly the crops it read.
            let mut r: ResultManifest =
                serde_json::from_slice(&std::fs::read(&files[1]).unwrap()).unwrap();
            r.crops = r.read;
            std::fs::write(&files[1], serde_json::to_vec(&r).unwrap()).unwrap();
            files
        };
        let sample = texts(
            "sample-synthetic-public_public-001",
            "sample-synthetic-public_public-002",
        );
        let legacy = texts(
            &format!("benchmark-{}", "a".repeat(64)),
            &format!("benchmark-{}", "b".repeat(64)),
        );
        let loc = texts(
            "sn85055101_1917-04-03_ed-1_seq-3",
            "sn85055101_1917-04-03_ed-1_seq-4",
        );
        for (tag, t, benchmark) in [
            ("s", &sample, true),
            ("l", &legacy, true),
            ("p", &loc, false),
        ] {
            let a = write_owned(&format!("{tag}a"), t);
            let b = write_owned(&format!("{tag}b"), t);
            assert_eq!(is_benchmark(&load(&a).unwrap()), benchmark, "{tag}");
            let dir = tmp.path().join(format!("qwen-{tag}"));
            let (seen, log, pages, states) = (
                dir.join("seen"),
                dir.join("pairs.jsonl"),
                dir.join("pages"),
                dir.join("crop-states"),
            );
            check(&a, Some(&seen)).unwrap();
            check(&b, Some(&seen)).unwrap();
            for rule in [Rule::StrictV1, Rule::CropConsensusV2] {
                compare_with(
                    &a,
                    &b,
                    Compare {
                        min_dice: 0.7,
                        rule,
                        bundle_verdict: BundleVerdict::PageV1,
                        log: Some(&log),
                        pages: Some(&pages),
                        crop_states: Some(&states),
                    },
                )
                .unwrap();
            }
            for kept in [&seen, &log, &pages, &states] {
                assert_eq!(kept.exists(), !benchmark, "{tag}: {}", kept.display());
            }
        }
        // Under page-v1 a benchmark bundle that agrees on most pages settles, in memory only.
        let mixed: Vec<(String, String)> = (0..4)
            .map(|i| {
                (
                    format!("sample-synthetic-public_public-00{i}:c01"),
                    if i % 2 == 0 { LONG_A } else { LONG_B }.to_string(),
                )
            })
            .collect();
        let mut off = mixed.clone();
        off[3].1 = "nothing alike here at all".into();
        let (a, b) = (write_owned("ma", &mixed), write_owned("mb", &off));
        let dir = tmp.path().join("qwen-mixed");
        for (verdict, settles) in [(BundleVerdict::Unit, false), (BundleVerdict::PageV1, true)] {
            for rule in [Rule::StrictV1, Rule::CropConsensusV2] {
                let got = compare_with(
                    &a,
                    &b,
                    Compare {
                        min_dice: 0.7,
                        rule,
                        bundle_verdict: verdict,
                        log: Some(&dir.join("pairs.jsonl")),
                        pages: Some(&dir.join("pages")),
                        crop_states: Some(&dir.join("crop-states")),
                    },
                );
                assert_eq!(got.is_ok(), settles, "{verdict:?} {rule:?}");
            }
        }
        assert!(!dir.exists());
        // A pair still has to agree: a benchmark bundle is judged like any other.
        let other = [
            (sample[0].0.clone(), "nothing alike here".to_string()),
            (sample[1].0.clone(), LONG_B.to_string()),
        ];
        let a = write_owned("da", &sample);
        let b = write_owned("db", &other);
        assert!(compare(&a, &b, 0.7, None, None).is_err());
    }

    /// Rewrites `files` as a Xiaomi-OCR-0 reading at request version 5 (#1776).
    fn as_xiaomi(files: &[PathBuf]) {
        let mut readings: Readings =
            serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        readings.model = Some("xiaomi-ocr-0-q8".into());
        readings.prompt_version = usnm_ocr_contract::qwen::PROMPT_VERSION_XIAOMI;
        let rb = serde_json::to_vec(&readings).unwrap();
        let mut r: ResultManifest =
            serde_json::from_slice(&std::fs::read(&files[1]).unwrap()).unwrap();
        r.model = readings.model.clone();
        r.crops = r.read;
        r.readings_bytes = rb.len() as u64;
        r.readings_sha256 = sha256(&rb);
        std::fs::write(&files[0], rb).unwrap();
        std::fs::write(&files[1], serde_json::to_vec(&r).unwrap()).unwrap();
    }

    #[test]
    fn crop_consensus_v2_judges_two_xiaomi_v5_readings_crop_by_crop() {
        let tmp = tempfile::tempdir().unwrap();
        let doc = "sn85055101_1917-04-03_ed-1_seq-3";
        let ids = [
            format!("{doc}:c01"),
            format!("{doc}:c02"),
            format!("{doc}:c03"),
        ];
        let a = write(
            tmp.path(),
            "a",
            &[
                (&ids[0], "le journal de paris publie ce matin les dernieres nouvelles du front et de la ville"),
                (&ids[1], "nouvelles du jour et de la nuit dans toute la france et dans les colonies lointaines"),
                (&ids[2], "rien"),
            ],
        );
        let b = write(
            tmp.path(),
            "b",
            &[
                (&ids[0], "le journal de paris publie ce matin les dernieres nouvelles du front et de la ville"),
                (&ids[1], "nouvelles du jour et de la nuit dans toute la france et dans les colonies lointaines"),
                // v5 has no loop guard; a looping reading is still caught by its tail.
                (&ids[2], &"la la la la la la la la ".repeat(40)),
            ],
        );
        as_xiaomi(&a);
        as_xiaomi(&b);
        let states = tmp.path().join("crop-states");
        compare_with(
            &a,
            &b,
            Compare {
                min_dice: 0.7,
                rule: Rule::CropConsensusV2,
                bundle_verdict: BundleVerdict::Unit,
                log: None,
                pages: None,
                crop_states: Some(&states),
            },
        )
        .unwrap();
        let job = load(&a).unwrap().result.job_id;
        let paths = crate::qwen::consensus::record_paths(&states, &job).unwrap();
        assert_eq!(paths.len(), 1);
        let record: crate::qwen::consensus::Record =
            serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
        assert_eq!(record.basis, crate::qwen::consensus::Basis::CropConsensusV2);
        assert!(record.agree);
        let state = |id: &str| record.crops.iter().find(|c| c.id == id).unwrap().state;
        assert_eq!(state(&ids[0]), crate::qwen::consensus::CropState::Agree);
        assert_eq!(state(&ids[1]), crate::qwen::consensus::CropState::Agree);
        assert_eq!(state(&ids[2]), crate::qwen::consensus::CropState::Looped);
        // Only the agreeing crops are vouched for in the canonical result.
        let vouch = crate::qwen::consensus::vouched(
            &states,
            &job,
            &load(&a).unwrap().result.readings_sha256,
        )
        .unwrap();
        assert!(vouch.vouches(&ids[0]) && !vouch.vouches(&ids[2]));
    }

    #[test]
    fn a_xiaomi_result_never_validates_a_qwen_one() {
        let tmp = tempfile::tempdir().unwrap();
        let texts = [
            ("c01", "one two three four"),
            ("c02", "five six seven eight"),
        ];
        let a = write(tmp.path(), "a", &texts);
        let b = write(tmp.path(), "b", &texts);
        // b as Xiaomi-OCR-0 read it: the same text, another reader.
        let mut readings: Readings =
            serde_json::from_slice(&std::fs::read(&b[0]).unwrap()).unwrap();
        readings.model = Some("xiaomi-ocr-0-q8".into());
        readings.prompt_version = usnm_ocr_contract::qwen::PROMPT_VERSION_XIAOMI;
        let rb = serde_json::to_vec(&readings).unwrap();
        let mut r: ResultManifest = serde_json::from_slice(&std::fs::read(&b[1]).unwrap()).unwrap();
        r.model = readings.model.clone();
        r.readings_bytes = rb.len() as u64;
        r.readings_sha256 = sha256(&rb);
        std::fs::write(&b[0], rb).unwrap();
        std::fs::write(&b[1], serde_json::to_vec(&r).unwrap()).unwrap();
        check(&b, None).unwrap();
        compare(&a, &a, 0.5, None, None).unwrap();
        let log = tmp.path().join("pairs.jsonl");
        let err = compare(&a, &b, 0.5, Some(&log), None).unwrap_err();
        assert!(
            format!("{err}").contains("read by different models"),
            "{err}"
        );
        let states = tmp.path().join("states");
        let err = compare_with(
            &a,
            &b,
            Compare {
                min_dice: 0.5,
                rule: Rule::CropConsensusV2,
                bundle_verdict: BundleVerdict::Unit,
                log: Some(&log),
                pages: None,
                crop_states: Some(&states),
            },
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("read by different models"),
            "{err}"
        );
        let last: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .last()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(last["agree"], false);
        assert_eq!(last["basis"], "strict-v1");
        assert_eq!(last["b"]["model"], "xiaomi-ocr-0-q8");
    }

    #[test]
    fn retains_both_replicas_exact_bytes_metrics_and_replays() {
        let tmp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let keep = tmp.path().join("seen");
        let a = write(
            tmp.path(),
            "canonical",
            &[("c01", "one two"), ("c02", "three four")],
        );
        let b = write(tmp.path(), "alternative", &[("c01", "[?]"), ("c02", "[?]")]);
        let original: ResultManifest =
            serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
        let raw = serde_json::to_vec_pretty(&original).unwrap();
        std::fs::write(&a[1], &raw).unwrap();
        for files in [&a, &b] {
            check(files, Some(&keep)).unwrap();
            let dir = keep.join(crate::quality::validate::result_name(&files[1]).unwrap());
            for (file, source) in [READINGS_FILE, "result.raw.json", DIAG_FILE]
                .iter()
                .zip(files)
            {
                assert_eq!(
                    std::fs::read(dir.join(file)).unwrap(),
                    std::fs::read(source).unwrap()
                );
            }
            let receipt = std::fs::read(dir.join("capture-v1.json")).unwrap();
            check(files, Some(&keep)).unwrap();
            assert_eq!(std::fs::read(dir.join("capture-v1.json")).unwrap(), receipt);
            for file in files {
                std::fs::remove_file(file).unwrap();
            }
            load(&[READINGS_FILE, RESULT_FILE, DIAG_FILE].map(|f| dir.join(f))).unwrap();
        }
        let alternative = keep.join(crate::quality::validate::result_name(&b[1]).unwrap());
        let metrics: serde_json::Value = serde_json::from_slice(
            &std::fs::read(alternative.join("readings-metrics-v1.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(metrics["readings"][0]["uncertainty_only"], true);
        assert_eq!(metrics["damage_rate"], serde_json::Value::Null);
        assert_eq!(metrics["readings"][0]["accuracy_established"], false);
    }

    #[test]
    fn completes_interrupted_legacy_capture_without_overwriting_evidence() {
        let tmp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let files = write(
            tmp.path(),
            "legacy",
            &[("c01", "one two"), ("c02", "three four")],
        );
        let keep = tmp.path().join("seen");
        let dir = keep.join(crate::quality::validate::result_name(&files[1]).unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(&files[0], dir.join(READINGS_FILE)).unwrap();
        let manifest = serde_json::to_vec_pretty(&load(&files).unwrap().result).unwrap();
        std::fs::write(dir.join(RESULT_FILE), &manifest).unwrap();
        check(&files, Some(&keep)).unwrap();
        assert_eq!(std::fs::read(dir.join(RESULT_FILE)).unwrap(), manifest);
        assert!(dir.join("capture-v1.json").is_file());
        let retained = std::fs::read(dir.join(READINGS_FILE)).unwrap();
        std::fs::write(dir.join(DIAG_FILE), b"corrupt").unwrap();
        let error = check(&files, Some(&keep)).unwrap_err();
        assert!(error
            .downcast_ref::<super::super::retain::CaptureError>()
            .is_some());
        assert_eq!(std::fs::read(dir.join(READINGS_FILE)).unwrap(), retained);
        assert_eq!(std::fs::read(dir.join(DIAG_FILE)).unwrap(), b"corrupt");
    }

    #[test]
    fn capture_failure_is_transient_but_invalid_science_is_not() {
        let tmp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let files = write(
            tmp.path(),
            "failure",
            &[("c01", "one two"), ("c02", "three four")],
        );
        let blocked = tmp.path().join("not-a-directory");
        std::fs::write(&blocked, b"blocked").unwrap();
        let error = check(&files, Some(&blocked)).unwrap_err();
        assert!(error
            .downcast_ref::<super::super::retain::CaptureError>()
            .is_some());
        assert!(files.iter().all(|f| f.is_file()));
        std::fs::write(&files[0], b"invalid").unwrap();
        let error = check(&files, Some(&blocked)).unwrap_err();
        assert!(error
            .downcast_ref::<super::super::retain::CaptureError>()
            .is_none());
    }

    // #1463 characterisation tests: they pin what the strict rule does today
    // with crops that hold no word. They are not a rule anyone has chosen.
    const WORDS: &str = "the ship sailed for england";

    /// A pair whose `c01` both read alike and whose `c02` is `c02`.
    fn wordless_pair(dir: &Path, c02: (&str, &str)) -> (Vec<PathBuf>, Vec<PathBuf>) {
        (
            write(dir, "a", &[("c01", WORDS), ("c02", c02.0)]),
            write(dir, "b", &[("c01", WORDS), ("c02", c02.1)]),
        )
    }

    /// Re-seals a written result as a loop-guarded run with these
    /// `repetition_loop_crop_bits`.
    fn loop_bits(files: &[PathBuf], bits: &str) {
        let mut r: Readings = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        r.prompt_version = usnm_ocr_contract::qwen::PROMPT_VERSION_LOOP_GUARD;
        r.diag
            .insert("repetition_loop_crop_bits".into(), bits.into());
        let rb = serde_json::to_vec(&r).unwrap();
        let mut m: ResultManifest =
            serde_json::from_slice(&std::fs::read(&files[1]).unwrap()).unwrap();
        m.readings_bytes = rb.len() as u64;
        m.readings_sha256 = sha256(&rb);
        std::fs::write(&files[0], rb).unwrap();
        std::fs::write(&files[1], serde_json::to_vec(&m).unwrap()).unwrap();
    }

    fn last_line(log: &Path) -> serde_json::Value {
        let text = std::fs::read_to_string(log).unwrap();
        serde_json::from_str(text.lines().last().unwrap()).unwrap()
    }

    #[test]
    fn characterise_1463_differing_punctuation_only_readings_pass_strict() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = wordless_pair(tmp.path(), (". , ;", "* * * —"));
        let log = tmp.path().join("pairs.jsonl");
        compare(&a, &b, 0.5, Some(&log), None).unwrap();
        // Two empty word bags score 1.0, so no floor fails them.
        compare(&a, &b, 1.0, None, None).unwrap();
        let line = last_line(&log);
        assert_eq!(line["crops"][1]["read"], serde_json::json!([true, true]));
        assert_eq!(line["crops"][1]["dice"], 1.0);
        // A pair with no word on any crop passes as well.
        let (a, b) = (
            write(tmp.path(), "c", &[("c01", "[?]"), ("c02", ". ,")]),
            write(tmp.path(), "d", &[("c01", "* *"), ("c02", "--")]),
        );
        compare(&a, &b, 0.5, None, None).unwrap();
    }

    #[test]
    fn characterise_1463_whitespace_only_pair_passes_strict() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = wordless_pair(tmp.path(), (" \n ", "\t"));
        let log = tmp.path().join("pairs.jsonl");
        compare(&a, &b, 0.5, Some(&log), None).unwrap();
        let line = last_line(&log);
        // Neither host read the crop, so it is not compared.
        assert_eq!(line["crops"][1]["read"], serde_json::json!([false, false]));
        assert!(line["crops"][1]["dice"].is_null());
    }

    #[test]
    fn characterise_1463_whitespace_only_against_nonempty_fails_strict() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = wordless_pair(tmp.path(), ("  ", "* * *"));
        let err = compare(&a, &b, 0.5, None, None).unwrap_err().to_string();
        assert_eq!(err, "c02: read by one host only");
    }

    #[test]
    fn characterise_1463_loop_flagged_wordless_crop_passes_strict() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = wordless_pair(tmp.path(), ("[?] [?]", "[?]"));
        loop_bits(&a, "02");
        loop_bits(&b, "00");
        let looped =
            |f: &[PathBuf]| super::super::consensus::sides(&load(f).unwrap())["c02"].looped;
        assert!(looped(&a) && !looped(&b));
        // The strict rule does not read the loop guard's flags.
        compare(&a, &b, 0.5, None, None).unwrap();
    }

    #[test]
    fn pair_log_marks_wordless_crops_and_leaves_every_verdict_as_it_was() {
        let crop = |identical, looped: [bool; 2]| {
            vec![serde_json::json!({"id": "c02", "identical": identical, "looped": looped})]
        };
        let cases = [
            ((". , ;", "* * * —"), None, crop(false, [false, false])),
            (
                ("[?] [?]", "[?]\n [?]"),
                Some("02"),
                crop(true, [true, false]),
            ),
            (("[?] [?]", "[?]"), Some("02"), crop(false, [true, false])),
            ((" \n ", "\t"), None, vec![]),
            (("  ", "* * *"), None, vec![]),
            (("one two", "* * *"), None, vec![]),
        ];
        for (c02, bits, wordless) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let (a, b) = wordless_pair(tmp.path(), c02);
            if let Some(bits) = bits {
                loop_bits(&a, bits);
                loop_bits(&b, "00");
            }
            let (la, lb) = (load(&a).unwrap(), load(&b).unwrap());
            let strict = qwen::agree(&la.result, &la.readings, &lb.result, &lb.readings, 0.5).1;
            let log = tmp.path().join("pairs.jsonl");
            let logged = compare(&a, &b, 0.5, Some(&log), None).map_err(|e| e.to_string());
            let unlogged = compare(&a, &b, 0.5, None, None).map_err(|e| e.to_string());
            assert_eq!(logged, strict, "{c02:?}");
            assert_eq!(unlogged, strict, "{c02:?}");
            let line = last_line(&log);
            assert_eq!(line["agree"], strict.is_ok(), "{c02:?}");
            assert_eq!(line["wordless"], serde_json::json!(wordless), "{c02:?}");
        }
    }

    #[test]
    fn strict_v2_does_not_count_wordless_crops_as_agreement_1463() {
        let opts = |rule| Compare {
            min_dice: 0.5,
            rule,
            bundle_verdict: BundleVerdict::Unit,
            log: None,
            pages: None,
            crop_states: None,
        };
        let run = |tmp: &Path, a: &[(&str, &str)], b: &[(&str, &str)], rule| {
            let (fa, fb) = (write(tmp, "a", a), write(tmp, "b", b));
            compare_with(&fa, &fb, opts(rule)).map_err(|e| e.to_string())
        };
        let tmp = tempfile::tempdir().unwrap();
        // A worded crop plus a wordless one that differs: the wordless crop is neutral.
        let (a, b) = (
            [("c01", WORDS), ("c02", ". , ;")],
            [("c01", WORDS), ("c02", "* * * —")],
        );
        assert!(run(tmp.path(), &a, &b, Rule::StrictV1).is_ok());
        assert!(run(tmp.path(), &a, &b, Rule::StrictV2).is_ok());
        // No word on any crop: strict-v1 accepts, strict-v2 does not.
        let (a, b) = (
            [("c01", "[?]"), ("c02", ". ,")],
            [("c01", "* *"), ("c02", "--")],
        );
        assert!(run(tmp.path(), &a, &b, Rule::StrictV1).is_ok());
        assert_eq!(
            run(tmp.path(), &a, &b, Rule::StrictV2).unwrap_err(),
            "no crop with a word on either side"
        );
        // Identical [?] on every crop is no better.
        let (a, b) = (
            [("c01", "[?]"), ("c02", "[?]")],
            [("c01", "[?]"), ("c02", "[?]")],
        );
        assert!(run(tmp.path(), &a, &b, Rule::StrictV2).is_err());
        // Whitespace-only on both sides (nothing read) is not agreement either.
        let (a, b) = ([("c01", " "), ("c02", "\n")], [("c01", "\t"), ("c02", " ")]);
        assert!(run(tmp.path(), &a, &b, Rule::StrictV1).is_ok());
        assert!(run(tmp.path(), &a, &b, Rule::StrictV2).is_err());
        // A worded crop that disagrees still fails, as before.
        let (a, b) = (
            [("c01", WORDS), ("c02", "[?]")],
            [("c01", "other words entirely"), ("c02", "[?]")],
        );
        assert!(run(tmp.path(), &a, &b, Rule::StrictV2).is_err());
        // One side worded, the other wordless: fails under both rules.
        let (a, b) = (
            [("c01", WORDS), ("c02", "one two")],
            [("c01", WORDS), ("c02", "* * *")],
        );
        assert!(run(tmp.path(), &a, &b, Rule::StrictV1).is_err());
        assert!(run(tmp.path(), &a, &b, Rule::StrictV2).is_err());
    }

    #[test]
    fn strict_v2_keeps_a_wordless_loop_flagged_crop_neutral_and_logs_its_rule_1463() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = wordless_pair(tmp.path(), ("[?] [?]", "[?]"));
        loop_bits(&a, "02");
        loop_bits(&b, "00");
        let log = tmp.path().join("pairs.jsonl");
        let opts = Compare {
            min_dice: 0.5,
            rule: Rule::StrictV2,
            bundle_verdict: BundleVerdict::Unit,
            log: Some(&log),
            pages: None,
            crop_states: None,
        };
        // c01 has words, so the pair agrees; loop flags are #1463's other question.
        compare_with(&a, &b, opts).unwrap();
        let line = last_line(&log);
        assert_eq!(line["rule"], "strict-v2");
        assert_eq!(line["agree"], true);
        assert_eq!(line["wordless"][0]["id"], "c02");
    }

    #[test]
    fn strict_v2_demotes_a_bundle_page_that_agreed_only_on_wordless_crops_1463() {
        let page = |doc: &str, ids: &[&str]| qwen::PageAgreement {
            doc_id: doc.into(),
            agree: true,
            crops: ids
                .iter()
                .map(|id| qwen::CropAgreement {
                    dice: Some(1.0),
                    id: format!("{doc}:{id}"),
                    read: (true, true),
                })
                .collect(),
            reason: None,
        };
        let mut pages = vec![page("p1", &["c01", "c02"]), page("p2", &["c01", "c02"])];
        let worded: std::collections::HashSet<&str> = ["p1:c01"].into();
        demote_wordless_pages(&mut pages, &worded);
        assert!(pages[0].agree && pages[0].reason.is_none());
        assert!(!pages[1].agree);
        assert_eq!(
            pages[1].reason.as_deref(),
            Some("p2: no crop with a word on either side")
        );
        // With one agreeing page of two, page-v1 would still settle the unit
        // on p1; with none it cannot.
        assert!(pages_settle(1, 2) && !pages_settle(0, 2));
    }
}
