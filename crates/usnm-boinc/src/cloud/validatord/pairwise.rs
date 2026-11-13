//! The pairwise apps of `cloud validatord`: `usnm_ndlocr` and `usnm_quality`. Dev only.
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
//! * Side effects (quality only; ndlocr has none): `check --keep` copies and the `compare --log`
//!   line, kept under `<state-dir>/quality/`. They are written only once the call is past every
//!   transient failure, and the pair log is journaled by call id, so a repeated or retried call
//!   adds nothing twice. See docs/boinc/server/bridge-steps.md, "Validate gold results for the
//!   cloud server (dev only)".
use super::{read_entry, Config, Request, Res};
use crate::cloud::pull::{self, OutFile};
use crate::quality::validate as quality_validate;
use anyhow::{bail, ensure, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use usnm_ocr_contract::{boinc as ocr_boinc, quality as ocr_quality};

/// The apps with an init script and a compare script (the `matches` reply).
pub const PAIRWISE_APPS: [&str; 2] = ["usnm_ndlocr", "usnm_quality"];

/// Serializes appends to the pair log across calls (one validatord per state directory).
static PAIRS: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ndlocr,
    Quality,
}

impl Kind {
    fn of(app: &str) -> Option<Kind> {
        match app {
            "usnm_ndlocr" => Some(Kind::Ndlocr),
            "usnm_quality" => Some(Kind::Quality),
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
            Kind::Quality => [
                ocr_quality::MAX_RESULT_JSON,
                ocr_quality::MAX_CONFUSIONS,
                ocr_quality::MAX_DIAG,
            ],
        }
    }

    /// The init script: exit 0 or not.
    fn init(self, files: &[PathBuf]) -> anyhow::Result<()> {
        match self {
            Kind::Ndlocr => crate::ndlocr::load(files).map(|_| ()),
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
    let work = Arc::new(tempfile::tempdir_in(cfg.state_dir.join("work"))?);
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
        side: cfg.state_dir.join("quality"),
        work: work.path().to_owned(),
    };
    // The blocking part owns the work directory, so a call cut off by the timeout cannot have
    // its files removed from under a side effect that is still being written.
    let keep = work.clone();
    let judged = tokio::task::spawn_blocking(move || {
        let _keep = keep;
        judge(kind, &call, items)
    })
    .await
    .context("the validation task")??;
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

struct Call {
    id: String,
    check_pair: bool,
    min_quorum: i64,
    canonical: Option<i64>,
    /// `<state-dir>/quality`: `seen/`, `pairs.jsonl` and the pair journal.
    side: PathBuf,
    /// This call's scratch directory, for the compare's own log line before it is journaled.
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
}

/// Everything after the fetches: the init checks, the compares and the side effects.
fn judge(kind: Kind, call: &Call, items: Vec<(i64, Usable)>) -> anyhow::Result<Judged> {
    let mut results: Vec<(i64, Option<Vec<PathBuf>>, Verdict)> = Vec::new();
    for (id, u) in items {
        results.push(match u {
            Usable::Retry => (id, None, Verdict::Retry),
            Usable::Invalid => (id, None, Verdict::Invalid),
            Usable::Files(p) => match kind.init(&p) {
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
    if record && kind == Kind::Quality {
        let seen = call.side.join("seen");
        for i in &usable {
            let files = results[*i].1.as_ref().expect("usable");
            quality_validate::check(files, Some(&seen))?;
        }
        for (key, line) in &lines {
            record_pair(&call.side, key, line)?;
        }
    }
    Ok(Judged {
        verdicts: results.into_iter().map(|(id, _, v)| (id, v)).collect(),
        pairs,
    })
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

/// Appends one compare's log line to `pairs.jsonl` exactly once for its key (call id and the two
/// results). The line and the file length it goes at are saved first; a repeat finds the line
/// there (or its torn start, which it removes) and only marks the key done.
pub(super) fn record_pair(side: &Path, key: &str, line: &[u8]) -> anyhow::Result<()> {
    let journal = side.join("pairs-journal");
    std::fs::create_dir_all(&journal)?;
    let done = journal.join(format!("{key}.done"));
    let _guard = PAIRS.lock().unwrap_or_else(|e| e.into_inner());
    if done.exists() {
        return Ok(());
    }
    let log = side.join("pairs.jsonl");
    let intent = journal.join(format!("{key}.intent"));
    let (offset, line) = match read_entry(&intent)? {
        Some(bytes) => {
            let v: Value = serde_json::from_slice(&bytes).context("pair journal entry")?;
            (
                v["offset"].as_u64().context("pair journal offset")?,
                v["line"]
                    .as_str()
                    .context("pair journal line")?
                    .as_bytes()
                    .to_vec(),
            )
        }
        None => {
            let offset = match std::fs::metadata(&log) {
                Ok(m) => m.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            };
            let text = String::from_utf8(line.to_vec()).context("pair log line")?;
            crate::atomic(
                &intent,
                &serde_json::to_vec(&json!({ "offset": offset, "line": text }))?,
            )?;
            (offset, line.to_vec())
        }
    };
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&log)?;
    let len = f.metadata()?.len();
    let mut appended = false;
    if len > offset {
        let have = (len - offset).min(line.len() as u64) as usize;
        let mut tail = vec![0u8; have];
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(&mut tail)?;
        if tail == line[..have] {
            if have == line.len() {
                appended = true;
            } else {
                // Only the start of the line is there, at the end of the file: a torn write.
                f.set_len(offset)?;
            }
        }
    }
    if !appended {
        f.seek(SeekFrom::End(0))?;
        f.write_all(&line)?;
        f.sync_all()?;
    }
    crate::atomic(&done, b"")?;
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
