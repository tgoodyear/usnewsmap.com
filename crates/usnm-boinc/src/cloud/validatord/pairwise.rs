//! The pairwise apps of `cloud validatord`: `usnm_ndlocr`, `usnm_quality` and `usnm_ocr`. Dev only.
//!
//! These apps have an init script and a compare script under stock `script_validator`, so the
//! reply uses the server's `matches` form (cloud-boinc `cloud/docs/architecture.md`, "Webhook
//! call"; D39 and D41 in `cloud/docs/compatibility.md`): a verdict per result, no fingerprints, and
//! the list of result pairs that match.
//!
//! * Init, per result: `ndlocr::load` (`usnm-boinc ndlocr check`) and `quality::validate::check`
//!   (`usnm-boinc quality check`, with the result-name rule its `--keep` applies). A result that
//!   does not have its three files, or fails the check, is `invalid`; a fetch that fails is
//!   `retry`, a case the stock validator (local files) never has.
//! * Compare, per pair: `ndlocr::compare` and `quality::validate::compare`, driven as stock
//!   `check_set` and `check_pair` drive them (`sched/validate_util2.cpp`). `check_set`: candidates
//!   in request order, each compared (candidate first) with every other usable result; the loop
//!   stops at the first candidate with at least `min_quorum / 2` matches. `check_pair`: each late
//!   result compared (late first) with the canonical result, and no other pair.
//! * `usnm_ocr` has no stock script: on-prem it runs at quorum 1 with the trivial validator and
//!   checks hosts by trust spot checks. Here its init is the integrity part of `checked_result`
//!   (see `ocr_init`); its compare is the spot checks' own text measure,
//!   `host_trust::container_spot::outside`, at the limits of [`Kind::OCR_LIMITS`], behind cheap
//!   bounds (see `ocr_match`). A failing init is `invalid`, never a retry.
//! * Side effects (quality only; ndlocr has none): `check --keep` copies, under `quality/seen/`
//!   in the state store, and each `compare --log` line as its own object,
//!   `quality/pairs/<key>.json`, where the key is a digest of the call id and the two results.
//!   They are written only once the call is past every transient failure. A line is written
//!   create-only, so a repeated or retried call adds nothing twice and keeps the first line's
//!   `at` time; a seen copy is overwritten with the same bytes. See docs/boinc/server/bridge-steps.md, "Validate gold results for the
//!   cloud server (dev only)".
use super::{Config, Request, Res};
use crate::cloud::pull::{self, OutFile};
use crate::host_trust::container_spot::{self, Limits};
use crate::quality::validate as quality_validate;
use anyhow::{bail, ensure, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio::sync::Semaphore;
use usnm_ocr_contract::{boinc as ocr_boinc, quality as ocr_quality};
use usnm_ocr_local::checks;

/// The apps with an init script and a compare script (the `matches` reply).
pub const PAIRWISE_APPS: [&str; 3] = ["usnm_ndlocr", "usnm_quality", "usnm_ocr"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ndlocr,
    Quality,
    Ocr,
}

impl Kind {
    /// The limits two `usnm_ocr` results may differ by and still match: `container_spot::CROSS`,
    /// the most lenient of the spot checks' families (`STRICT` for two hosts of one family,
    /// `CROSS` for an arm64 host against an amd64 one). A pairwise call sees the results, not
    /// the hosts' families, so any two hosts may be a cross-family pair; judging every pair by
    /// `STRICT` would reject a legitimate arm64/amd64 pair that the spot checks accept. Text
    /// within these limits is enough: `layout.xml` is not compared.
    const OCR_LIMITS: Limits = container_spot::CROSS;

    fn of(app: &str) -> Option<Kind> {
        match app {
            "usnm_ndlocr" => Some(Kind::Ndlocr),
            "usnm_quality" => Some(Kind::Quality),
            "usnm_ocr" => Some(Kind::Ocr),
            _ => None,
        }
    }

    /// The read limit of each output position, in template order: `read` fails on a larger file.
    fn caps(self) -> [usize; 3] {
        match self {
            Kind::Ndlocr => [
                ocr_boinc::MAX_TEXT,
                ocr_boinc::MAX_ALTO,
                ocr_boinc::MAX_JSON,
            ],
            // The usnm_ocr_out template: text.txt, layout.xml, result.json.
            Kind::Ocr => [
                ocr_boinc::MAX_TEXT,
                ocr_boinc::MAX_ALTO,
                ocr_boinc::MAX_JSON,
            ],
            Kind::Quality => [
                ocr_quality::MAX_RESULT_JSON,
                ocr_quality::MAX_CONFUSIONS,
                ocr_quality::MAX_DIAG,
            ],
        }
    }

    /// The init script: exit 0 or not. `workunit` is the workunit's name, which `usnm_ocr` ties
    /// its result to.
    fn init(self, files: &[PathBuf], workunit: &str) -> anyhow::Result<()> {
        match self {
            Kind::Ndlocr => crate::ndlocr::load(files).map(|_| ()),
            Kind::Ocr => ocr_init(files, workunit),
            Kind::Quality => {
                quality_validate::check(files, None)?;
                // `--keep` names the copy after the uploaded result.json and refuses an odd name.
                quality_validate::result_name(&files[0]).map(|_| ())
            }
        }
    }

    /// The compare script. Quality's `--log` line goes to `line` and is returned: the library
    /// writes it, so it is the stock line. No line with an error means the compare could not
    /// run (not "the results differ").
    fn compare(
        self,
        a: &[PathBuf],
        b: &[PathBuf],
        line: &Path,
    ) -> anyhow::Result<(bool, Option<Vec<u8>>)> {
        match self {
            Kind::Ndlocr => Ok((crate::ndlocr::compare(a, b).is_ok(), None)),
            Kind::Ocr => {
                let (a, b) = (text_of(&a[0])?, text_of(&b[0])?);
                Ok((ocr_match(&a, &b), None))
            }
            Kind::Quality => {
                let verdict = quality_validate::compare(a, b, Some(line));
                match std::fs::read(line) {
                    Ok(bytes) if !bytes.is_empty() => Ok((verdict.is_ok(), Some(bytes))),
                    _ => bail!(
                        "the quality compare wrote no log line: {}",
                        verdict.err().map(|e| format!("{e:#}")).unwrap_or_default()
                    ),
                }
            }
        }
    }
}

fn read_capped(path: &Path, cap: usize) -> anyhow::Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        bytes.len() <= cap,
        "{} is over its size limit",
        path.display()
    );
    Ok(bytes)
}

/// The longest text, in characters, that the `usnm_ocr` compare measures. `outside` aligns the two
/// texts, which costs about the product of their lengths, and a call has a time limit; real
/// newspaper pages are far smaller. A longer pair is no match, not an error: the server then asks
/// for another result.
pub(super) const OCR_COMPARE_MAX_CHARS: usize = 200_000;

/// The init script of `usnm_ocr`: what `checked_result` (`usnm-boinc import`) checks of a result
/// that does not need the job. `result.json` parses as the contract's `ResultManifest` with an
/// RFC 3339 `completed_at`; `text.txt` is UTF-8 and `layout.xml` valid ALTO (`checks::check_alto`,
/// as `checked_result` calls it), each within its read limit; and `text_sha256` and `alto_sha256`
/// are the digests of those two files.
///
/// Provenance: the webhook names the workunit but carries no input files, so the job
/// (`<id>_request_json`) is not at hand and `ResultManifest::matches` cannot run. Instead
/// `job_id` must be the id that `cloud submit` put in the workunit's name (`Job::workunit`:
/// `usnm_<id>`). The scan, engine and version checks of `matches` are done on import, by
/// `checked_result`, which holds the job's record.
fn ocr_init(files: &[PathBuf], workunit: &str) -> anyhow::Result<()> {
    let [text, alto, result] = files else {
        bail!("expected text, layout and result files");
    };
    let manifest: ocr_boinc::ResultManifest =
        serde_json::from_slice(&read_capped(result, ocr_boinc::MAX_JSON)?)
            .context("result.json is not a usnm_ocr result")?;
    chrono::DateTime::parse_from_rfc3339(&manifest.completed_at)
        .context("completed_at is not RFC 3339")?;
    ensure!(
        workunit == format!("usnm_{}", manifest.job_id),
        "result.json is for another job than this workunit"
    );
    let text = read_capped(text, ocr_boinc::MAX_TEXT)?;
    let alto = read_capped(alto, ocr_boinc::MAX_ALTO)?;
    ensure!(
        manifest.text_sha256 == ocr_boinc::sha256(&text)
            && manifest.alto_sha256 == ocr_boinc::sha256(&alto),
        "output checksum mismatch"
    );
    std::str::from_utf8(&text).context("text.txt is not UTF-8")?;
    checks::check_alto(&alto).map_err(|r| anyhow::anyhow!("{}", r.code()))?;
    Ok(())
}

/// Whether two `usnm_ocr` texts match: equal, or within [`Kind::OCR_LIMITS`] by `outside`.
/// The cheap tests come first and bound the cost of `outside`: equal bytes match; character
/// counts outside the ratio limits, or a longer text over [`OCR_COMPARE_MAX_CHARS`], do not.
///
/// The measure is symmetric here. `outside` takes the replay's length over the original's, so
/// the shorter text is always passed as the original: the ratio is then at least 1 and only
/// `max_ratio` binds, whichever result was the candidate. Both orders of a pair give one answer.
fn ocr_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (ca, cb) = (a.chars().count(), b.chars().count());
    let (short, long, cs, cl) = if ca <= cb {
        (a, b, ca, cb)
    } else {
        (b, a, cb, ca)
    };
    let limits = &Kind::OCR_LIMITS;
    if cl > OCR_COMPARE_MAX_CHARS
        || (cs as f64 / cl as f64) < limits.min_ratio
        || (cl as f64 / cs as f64) > limits.max_ratio
    {
        return false;
    }
    container_spot::outside(limits, short, long).is_none()
}

/// The text of a `usnm_ocr` result: `text.txt`, which must be UTF-8.
fn text_of(path: &Path) -> anyhow::Result<String> {
    String::from_utf8(read_capped(path, ocr_boinc::MAX_TEXT)?).context("text.txt is not UTF-8")
}

enum Fetched {
    Invalid,
    Retry,
    Files(Vec<PathBuf>),
}

enum Usable {
    Invalid,
    Retry,
    Files(Vec<PathBuf>),
}

/// The files of one result, fetched into `work`. The init script wants exactly three; a file
/// over its read limit is an invalid result before any fetch.
async fn fetch(cfg: &Config, kind: Kind, r: &Res, work: &Path) -> Fetched {
    if r.files.len() != 3 {
        return Fetched::Invalid;
    }
    let mut names = BTreeSet::new();
    for (f, cap) in r.files.iter().zip(kind.caps()) {
        let unsafe_name =
            f.name.is_empty() || f.name.contains(['/', '\\']) || f.name.contains("..");
        if f.nbytes > cap as u64 || unsafe_name || !names.insert(f.name.as_str()) {
            return Fetched::Invalid;
        }
    }
    let dir = work.join(r.id.to_string());
    if std::fs::create_dir_all(&dir).is_err() {
        return Fetched::Retry;
    }
    let mut paths = Vec::new();
    for f in &r.files {
        let path = dir.join(&f.name);
        let listed = OutFile::listed(&f.name, f.nbytes, &f.md5);
        if pull::download(&cfg.fetch, &listed, &path).await.is_err() {
            return Fetched::Retry;
        }
        paths.push(path);
    }
    Fetched::Files(paths)
}

pub(super) async fn evaluate(cfg: &Config, req: &Request) -> anyhow::Result<(Value, bool)> {
    let kind = Kind::of(&req.workunit.app).context("not a pairwise app")?;
    let work = Arc::new(tempfile::tempdir_in(&cfg.work_dir)?);
    let mut items = Vec::new();
    for r in &req.results {
        let fetched = fetch(cfg, kind, r, work.path()).await;
        let usable = match fetched {
            Fetched::Invalid => Usable::Invalid,
            Fetched::Retry => Usable::Retry,
            Fetched::Files(p) => Usable::Files(p),
        };
        items.push((r.id, usable));
    }
    let call = Call {
        id: req.call_id.clone(),
        check_pair: req.mode == "check_pair",
        min_quorum: req.workunit.min_quorum,
        canonical: req.canonical_result_id,
        workunit: req.workunit.name.clone(),
        work: work.path().to_owned(),
    };
    // The blocking part owns the work directory, so a call cut off by the timeout cannot have
    // its files removed from under a side effect that is still being written.
    // The permit is moved in too: a timed-out call's blocking work goes on, and holds its place
    // among at most one per CPU until it ends, so such work cannot pile up.
    let permit = blocking_limiter()
        .clone()
        .acquire_owned()
        .await
        .context("the validation limiter")?;
    let keep = work.clone();
    let judged = tokio::task::spawn_blocking(move || {
        let _keep = keep;
        let _permit = permit;
        judge(kind, &call, items)
    })
    .await
    .context("the validation task")??;
    // The side effects, into the state store. A failure is an error (500), and the server calls
    // again: the seen copies are rewritten with the same bytes and the lines already written stay.
    if let Some(seen) = &judged.seen {
        store_tree(&*cfg.state, seen, "quality/seen").await?;
    }
    for (key, line) in &judged.lines {
        cfg.state
            .put_new(
                &format!("quality/pairs/{key}.json"),
                line.clone(),
                "application/json",
            )
            .await
            .with_context(|| format!("storing pair line {key}"))?;
    }
    drop(work);
    let out: Vec<Value> = judged
        .verdicts
        .iter()
        .map(|(id, v)| match v {
            Verdict::Readable => json!({ "id": id }),
            Verdict::Invalid => json!({ "id": id, "invalid": true }),
            Verdict::Retry => json!({ "id": id, "retry": true }),
        })
        .collect();
    let all_final = !judged.verdicts.iter().any(|(_, v)| *v == Verdict::Retry);
    let matches: Vec<Value> = judged.pairs.iter().map(|(a, b)| json!([a, b])).collect();
    Ok((json!({ "results": out, "matches": matches }), all_final))
}

/// At most one blocking judge per CPU runs at once, across calls.
fn blocking_limiter() -> &'static Arc<Semaphore> {
    static LIMIT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    LIMIT.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(1, |n| n.get());
        Arc::new(Semaphore::new(n))
    })
}

struct Call {
    id: String,
    check_pair: bool,
    min_quorum: i64,
    canonical: Option<i64>,
    /// The workunit's name.
    workunit: String,
    /// This call's scratch directory: the fetched files, the compare's log line, and the seen
    /// copies before they are stored.
    work: PathBuf,
}

#[derive(PartialEq, Eq)]
enum Verdict {
    Readable,
    Invalid,
    Retry,
}

struct Judged {
    verdicts: Vec<(i64, Verdict)>,
    /// Unordered pairs, as `(lower id, higher id)`.
    pairs: BTreeSet<(i64, i64)>,
    /// The local directory `check --keep` filled, when the call records side effects.
    seen: Option<PathBuf>,
    /// The log lines to record, with their keys; empty when the call records nothing.
    lines: Vec<(String, Vec<u8>)>,
}

/// Everything after the fetches: the init checks, the compares and the side effects.
fn judge(kind: Kind, call: &Call, items: Vec<(i64, Usable)>) -> anyhow::Result<Judged> {
    let mut results: Vec<(i64, Option<Vec<PathBuf>>, Verdict)> = Vec::new();
    for (id, u) in items {
        results.push(match u {
            Usable::Retry => (id, None, Verdict::Retry),
            Usable::Invalid => (id, None, Verdict::Invalid),
            Usable::Files(p) => match kind.init(&p, &call.workunit) {
                Ok(()) => (id, Some(p), Verdict::Readable),
                Err(_) => (id, None, Verdict::Invalid),
            },
        });
    }
    let any_retry = results.iter().any(|r| r.2 == Verdict::Retry);
    let usable: Vec<usize> = (0..results.len())
        .filter(|i| results[*i].1.is_some())
        .collect();
    let mut cmp = Comparer {
        kind,
        call,
        results: &results,
        lines: Vec::new(),
        pairs: BTreeSet::new(),
        n: 0,
    };
    // Whether the call got past every transient failure, which is when stock's init and compare
    // would have run and left their files.
    let mut record = false;
    if call.check_pair {
        let canon = call
            .canonical
            .and_then(|c| usable.iter().copied().find(|i| results[*i].0 == c));
        if let Some(c) = canon {
            record = true;
            for &late in usable.iter().filter(|i| **i != c) {
                cmp.compare(late, c)?;
            }
        }
    } else if !any_retry {
        record = true;
        if usable.len() as i64 >= call.min_quorum {
            let need = (call.min_quorum / 2).max(0) as usize;
            for &i in &usable {
                let mut neq = 0;
                for &j in usable.iter().filter(|j| **j != i) {
                    if cmp.compare(i, j)? {
                        neq += 1;
                    }
                }
                if neq >= need {
                    break;
                }
            }
        }
    }
    let (lines, pairs) = (cmp.lines, cmp.pairs);
    let mut out = Judged {
        verdicts: Vec::new(),
        pairs,
        seen: None,
        lines: Vec::new(),
    };
    if record && kind == Kind::Quality {
        let seen = call.work.join("seen");
        for i in &usable {
            let files = results[*i].1.as_ref().expect("usable");
            quality_validate::check(files, Some(&seen))?;
        }
        out.seen = Some(seen);
        out.lines = lines;
    }
    out.verdicts = results.into_iter().map(|(id, _, v)| (id, v)).collect();
    Ok(out)
}

struct Comparer<'a> {
    kind: Kind,
    call: &'a Call,
    results: &'a [(i64, Option<Vec<PathBuf>>, Verdict)],
    /// The log lines of the compares run, with their journal keys.
    lines: Vec<(String, Vec<u8>)>,
    pairs: BTreeSet<(i64, i64)>,
    n: usize,
}

impl Comparer<'_> {
    /// One stock compare call, `first` as the candidate (stock's first argument).
    fn compare(&mut self, first: usize, second: usize) -> anyhow::Result<bool> {
        let (a, b) = (&self.results[first], &self.results[second]);
        let (ida, idb) = (a.0, b.0);
        let line = self.call.work.join(format!("line-{}.tmp", self.n));
        self.n += 1;
        let out = self.kind.compare(
            a.1.as_ref().expect("usable"),
            b.1.as_ref().expect("usable"),
            &line,
        );
        let _ = std::fs::remove_file(&line);
        let (matched, log) = out?;
        if let Some(log) = log {
            let key: String = Sha256::digest(format!("{}\0{ida}\0{idb}", self.call.id).as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            self.lines.push((key, log));
        }
        if matched {
            self.pairs.insert((ida.min(idb), ida.max(idb)));
        }
        Ok(matched)
    }
}

/// The store key segment for one path segment of a seen copy. A result name the quality check
/// accepts (up to 200 characters, `_` and `-` anywhere) is not always a store segment (at most
/// 128, starting alphanumeric); such a name is stored as `x-<sha256 of the name>`, with the
/// name itself in that directory's `result-name.txt`.
pub(super) fn seen_segment(seg: &str) -> (String, bool) {
    if usnm_store::is_safe_segment(seg) && !seg.starts_with("x-") {
        (seg.to_owned(), false)
    } else {
        let hex: String = Sha256::digest(seg.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (format!("x-{hex}"), true)
    }
}

/// Stores every file under `dir` at `<prefix>/<path relative to dir>`, overwriting, with each
/// segment made a store segment by [`seen_segment`].
async fn store_tree(
    store: &dyn usnm_store::ObjectStore,
    dir: &Path,
    prefix: &str,
) -> anyhow::Result<()> {
    let mut todo = vec![dir.to_owned()];
    while let Some(d) = todo.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {}", d.display())),
        };
        for e in rd {
            let p = e?.path();
            if p.is_dir() {
                todo.push(p);
                continue;
            }
            let rel = p.strip_prefix(dir).context("seen path")?;
            let rel = rel.to_str().context("seen path is not UTF-8")?;
            let mut path = prefix.to_owned();
            for seg in rel.split('/') {
                let (key, hashed) = seen_segment(seg);
                path.push('/');
                path.push_str(&key);
                if hashed {
                    let name = format!("{path}/result-name.txt");
                    store
                        .put(&name, seg.as_bytes().to_vec(), "text/plain")
                        .await
                        .with_context(|| format!("storing {name}"))?;
                }
            }
            let bytes = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
            store
                .put(&path, bytes, "application/octet-stream")
                .await
                .with_context(|| format!("storing {path}"))?;
        }
    }
    Ok(())
}

/// For `Request::check`: what the pairwise apps need beyond the gold apps.
pub(super) fn check_request(req: &Request) -> anyhow::Result<()> {
    ensure!(
        req.mode == "check_set" || req.mode == "check_pair",
        "mode must be check_set or check_pair"
    );
    ensure!(
        req.workunit.min_quorum >= 1,
        "workunit.min_quorum is missing"
    );
    if req.mode == "check_pair" {
        let c = req
            .canonical_result_id
            .context("check_pair needs canonical_result_id")?;
        ensure!(
            req.results.iter().any(|r| r.id == c),
            "canonical_result_id {c} is not among the results"
        );
    }
    Ok(())
}
