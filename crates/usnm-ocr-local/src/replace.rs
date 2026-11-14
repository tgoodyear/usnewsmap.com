//! The release's side of our own OCR (12 §12.5 Step 9, `--ocr-local`):
//! which of `ocr-local`'s readings replace LoC's text in the index.
//!
//! - **Loading.** The release lists `ocr-local/pages/` once ([`Listing`]).
//!   As it reaches a batch it loads the parts of that batch's titles
//!   ([`Listing::load`]), so memory holds one batch's titles; a title in
//!   several batches is read once for each. For each page it takes the row
//!   with the lowest stored damage rate (ties: the newest) from the stored
//!   scores alone, then reads the text of the chosen rows only and
//!   normalizes it again for the row's engine and language, so a rule added
//!   since the part was written (#1065) applies to pages already read.
//! - **Revocations.** `ocr-local/revoked/<stamp>.json` files, create-only,
//!   each naming a node, an engine, a time window, an import run or an
//!   explicit list of pages, with a reason and a reference
//!   ([`Revocation`]). Rows they match are left out
//!   before a page's row is chosen.
//! - **Host holds.** A revocation can name BOINC hosts (`hosts`). The pages
//!   a host read come from the provenance files under `ocr-local/provenance/`
//!   ([`Provenance`], built from the BOINC database, #1956).
//! - **Rule.** [`super::rule`] on both texts scored again.
//! - **Records.** [`Tally`] for the version's `ocr_local.json`; the pages
//!   replaced for its `ocr_local_pages.bin` (`usnm_state::ocr_local`).
//!
//! Logs carry page ids, paths and counts, never text.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{bail, Context};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use usnm_store::ObjectStore;

use super::pages::{self, Scores, StoredRow};
use super::report::{engine_key, Tally as Count};
use super::rule::{self, Keep};
use super::ROOT;
use usnm_core::digest::hex;
use usnm_core::text::normalize_ocr_for;
use usnm_state::ocr_local::doc_hash;

/// Where revocations are.
pub fn revoked_prefix() -> String {
    format!("{ROOT}/revoked")
}

/// Where the BOINC host provenance files are.
pub fn provenance_prefix() -> String {
    format!("{ROOT}/provenance")
}

/// Which BOINC host read which published page: the union of the
/// `ocr-local/provenance/<stamp>.jsonl` files, create-only, one JSON object
/// a line with at least `doc_id`, `status` and (for `resolved`) `hostid`;
/// `candidate_host_ids` lists the hosts behind a page whose job ran in two
/// workunits (`ambiguous_duplicate_wu`). They are exports of the quality
/// database's `quality.boinc_page_provenance` table. Of two lines for one
/// page the later file wins, unless it resolved less than the earlier one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// `doc_hash` of a page to the host that read it.
    host: HashMap<u64, u64>,
    /// `doc_hash` of a page to the hosts one of which read it.
    candidates: HashMap<u64, Vec<u64>>,
}

#[derive(Deserialize)]
struct ProvenanceLine {
    doc_id: String,
    status: String,
    #[serde(default)]
    hostid: Option<u64>,
    #[serde(default)]
    candidate_host_ids: Option<Vec<u64>>,
}

impl Provenance {
    /// Reads every provenance file in the store.
    pub async fn load(curated: &dyn ObjectStore) -> anyhow::Result<Self> {
        let mut paths = curated.list(&provenance_prefix()).await?;
        paths.sort();
        let mut out = Self::default();
        for path in paths.into_iter().filter(|p| p.ends_with(".jsonl")) {
            let bytes = curated
                .get(&path)
                .await?
                .with_context(|| format!("`{path}` vanished"))?;
            out.add(&bytes)
                .with_context(|| format!("provenance `{path}`"))?;
        }
        Ok(out)
    }

    /// Adds one file's lines.
    pub fn add(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        for (n, line) in bytes.split(|b| *b == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let l: ProvenanceLine =
                serde_json::from_slice(line).with_context(|| format!("line {}", n + 1))?;
            let key = doc_hash(&l.doc_id);
            match (l.status.as_str(), l.hostid) {
                ("resolved", Some(h)) => {
                    self.host.insert(key, h);
                    self.candidates.remove(&key);
                }
                ("resolved", None) => bail!("line {}: `resolved` without a `hostid`", n + 1),
                _ if self.host.contains_key(&key) => {}
                _ => {
                    if let Some(c) = l.candidate_host_ids.filter(|c| !c.is_empty()) {
                        self.candidates.insert(key, c);
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether no page is known.
    pub fn is_empty(&self) -> bool {
        self.host.is_empty() && self.candidates.is_empty()
    }

    /// The pages `hosts` read: those resolved to one of them, and with
    /// `ambiguous` also those one of whose candidate hosts is one of them.
    pub fn pages_of(&self, hosts: &[u64], ambiguous: bool) -> HashSet<u64> {
        let mut out: HashSet<u64> = self
            .host
            .iter()
            .filter(|(_, h)| hosts.contains(h))
            .map(|(k, _)| *k)
            .collect();
        if ambiguous {
            out.extend(
                self.candidates
                    .iter()
                    .filter(|(_, c)| c.iter().any(|h| hosts.contains(h)))
                    .map(|(k, _)| *k),
            );
        }
        out
    }
}

/// Every merged part in `ocr-local/pages/`, by title.
#[derive(Debug, Default, Clone)]
pub struct Listing {
    by_title: BTreeMap<String, Vec<String>>,
}

impl Listing {
    /// List `ocr-local/pages/` once. Paths that aren't
    /// `ocr-local/pages/<lccn>/<run>.parquet` are left out.
    pub async fn list(curated: &dyn ObjectStore) -> anyhow::Result<Self> {
        let prefix = format!("{ROOT}/pages");
        let mut by_title: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in curated.list(&prefix).await? {
            let Some(rest) = path.strip_prefix(&format!("{prefix}/")) else {
                continue;
            };
            let Some((lccn, file)) = rest.split_once('/') else {
                continue;
            };
            if file.contains('/') || !file.ends_with(".parquet") {
                continue;
            }
            by_title.entry(lccn.to_owned()).or_default().push(path);
        }
        for paths in by_title.values_mut() {
            paths.sort();
        }
        Ok(Self { by_title })
    }

    pub fn parts(&self) -> usize {
        self.by_title.values().map(Vec::len).sum()
    }

    pub fn titles(&self) -> usize {
        self.by_title.len()
    }

    /// Whether any part is for one of `lccns`.
    pub fn has_any<'a>(&self, mut lccns: impl Iterator<Item = &'a str>) -> bool {
        lccns.any(|l| self.by_title.contains_key(l))
    }

    /// Load the pages of `lccns` for the release: per page, the chosen row
    /// with its text, after `revoked`.
    pub async fn load<'a>(
        &self,
        curated: &dyn ObjectStore,
        lccns: impl Iterator<Item = &'a str>,
        revoked: &[Revocation],
    ) -> anyhow::Result<Loaded> {
        let mut out = Loaded::default();
        let lccns: BTreeSet<&str> = lccns.collect();
        for lccn in lccns {
            let Some(paths) = self.by_title.get(lccn) else {
                continue;
            };
            // The stored scores alone choose each page's row: (row, part,
            // the row's place in the part).
            let mut best: HashMap<String, (StoredRow, usize, usize)> = HashMap::new();
            let mut withdrawn: HashSet<String> = HashSet::new();
            let mut bodies = Vec::with_capacity(paths.len());
            for (i, path) in paths.iter().enumerate() {
                let bytes = Bytes::from(
                    curated
                        .get(path)
                        .await?
                        .with_context(|| format!("`{path}` vanished"))?,
                );
                out.parts.push(InputPart {
                    path: path.clone(),
                    bytes: bytes.len() as u64,
                    sha256: hex(&Sha256::digest(&bytes)),
                });
                let run = run_of(path);
                let rows = pages::read_scores(bytes.clone()).with_context(|| path.clone())?;
                for (r, row) in rows.into_iter().enumerate() {
                    anyhow::ensure!(row.lccn == lccn, "`{path}` holds a page of `{}`", row.lccn);
                    if revoked.iter().any(|v| v.matches(&row, run)) {
                        withdrawn.insert(row.doc_id.clone());
                        continue;
                    }
                    let take = match best.get(&row.doc_id) {
                        None => true,
                        Some((b, j, q)) => prefer((&row, i, r), (b, *j, *q)),
                    };
                    if take {
                        best.insert(row.doc_id.clone(), (row, i, r));
                    }
                }
                bodies.push(bytes);
            }
            // Then the chosen rows' text, by their place in their part (a
            // part can hold a page twice), reading only that column.
            let mut wanted: HashMap<usize, HashSet<usize>> = HashMap::new();
            for (_, i, r) in best.values() {
                wanted.entry(*i).or_default().insert(*r);
            }
            let mut texts: HashMap<(usize, usize), (String, String)> = HashMap::new();
            for (i, rows) in wanted {
                for (r, found) in
                    pages::read_texts(bodies[i].clone(), &rows).with_context(|| paths[i].clone())?
                {
                    texts.insert((i, r), found);
                }
            }
            drop(bodies);
            for (id, (row, i, r)) in best {
                let (doc_id, text) = texts
                    .remove(&(i, r))
                    .with_context(|| format!("`{id}` has scores but no text"))?;
                anyhow::ensure!(doc_id == id, "`{}` row {r} changed between reads", paths[i]);
                withdrawn.remove(&id);
                // Stored text was normalized when the page was read, by the
                // rules of that day: rejoin the line-end marks the rules
                // since take, as a worker would now: `⸗`, `¬` and `‐` in any
                // text, and `—` before a lowercase letter in text read with
                // a Fraktur model or in German. A row has no language of its
                // own; the one its stored scores found in our text is the
                // page's (`StoredRow::hyphenation`). Where that is unknown
                // (`und`, no words, another metric version's scores), the
                // engine alone decides. Normalizing is idempotent, so text
                // that has nothing new to join stays as it is. The stored
                // scores chose the row; the release scores this text again.
                let text = normalize_ocr_for(&text, row.hyphenation());
                out.pages.insert(id, Candidate { row, text });
            }
            out.revoked.extend(withdrawn);
        }
        Ok(out)
    }

    /// How many pages of `ocr-local` would replace LoC's text in
    /// `published`, the batches the version keeps from its published
    /// indexes, by rule v1 on the scores `ocr-sync` stored: an estimate,
    /// for timing a full rebuild. Reads no text.
    pub async fn estimate(
        &self,
        curated: &dyn ObjectStore,
        published: &BTreeSet<&str>,
        title_languages: impl Fn(&str) -> Vec<String>,
        revoked: &[Revocation],
    ) -> anyhow::Result<u64> {
        let mut n = 0u64;
        for (lccn, paths) in &self.by_title {
            let langs = title_languages(lccn);
            let langs: Vec<&str> = langs.iter().map(String::as_str).collect();
            let mut best: HashMap<String, (StoredRow, usize, usize)> = HashMap::new();
            for (i, path) in paths.iter().enumerate() {
                let Some(bytes) = curated.get(path).await? else {
                    continue;
                };
                let run = run_of(path);
                let rows = pages::read_scores(Bytes::from(bytes)).with_context(|| path.clone())?;
                for (r, row) in rows.into_iter().enumerate() {
                    if !published.contains(row.batch.as_str())
                        || revoked.iter().any(|r| r.matches(&row, run))
                    {
                        continue;
                    }
                    let take = best
                        .get(&row.doc_id)
                        .is_none_or(|(b, j, q)| prefer((&row, i, r), (b, *j, *q)));
                    if take {
                        best.insert(row.doc_id.clone(), (row, i, r));
                    }
                }
            }
            n += best
                .values()
                .filter(|(r, _, _)| {
                    rule::line_merge::is_line_job(&r.ocr_engine)
                        || rule::verdict(&r.ours, &r.loc, Some(&langs)).is_ok()
                })
                .count() as u64;
        }
        Ok(n)
    }
}

/// The import run a merged part came from: its file name's stem.
fn run_of(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".parquet"))
        .unwrap_or_default()
}

/// Whether row `a` (the `r`th row of the `i`th part in path order) is
/// chosen over `b`: the lower stored damage rate, then the newer, then the
/// later part, then the later row.
fn prefer(a: (&StoredRow, usize, usize), b: (&StoredRow, usize, usize)) -> bool {
    let rate = |r: &StoredRow| r.ours.damage_rate.unwrap_or(f64::INFINITY);
    let (ra, rb) = (rate(a.0), rate(b.0));
    if ra != rb {
        return ra < rb;
    }
    (a.0.ocred_at, a.1, a.2) > (b.0.ocred_at, b.1, b.2)
}

/// One page's chosen reading.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub row: StoredRow,
    pub text: String,
}

impl Candidate {
    pub fn engine(&self) -> usnm_state::ocr_local::Engine {
        usnm_state::ocr_local::Engine {
            source: self.row.ocr_source.clone(),
            engine: engine_key(&self.row.ocr_engine),
        }
    }
}

/// One immutable input part, hashed from the bytes used by the release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputPart {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// One batch's titles' pages.
#[derive(Debug, Default)]
pub struct Loaded {
    pub pages: HashMap<String, Candidate>,
    /// Pages whose every row a revocation matched.
    pub revoked: HashSet<String>,
    /// The exact bytes read, hashed before decoding.
    pub parts: Vec<InputPart>,
}

/// A create-only `ocr-local/revoked/<stamp>.json`: the rows it names are
/// left out of every release from then on. Each selector given must match
/// (at least one is required); a page whose rows are all withdrawn keeps
/// LoC's text. Published indexes keep our text until the next full rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub reason: String,
    /// An issue, an incident, a report.
    pub reference: String,
    /// The node that read the rows (the merged parts' `node`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The engine: its full tag, or its short key (`<family>/<version> <profile>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// The `ocr-sync` import run (`ocr-local/pages/<lccn>/<run>.parquet`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Read at or after this time (`ocred_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    /// Read before this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    /// An explicit page list: doc ids (`<lccn>_<date>_ed-<n>_seq-<n>`). A
    /// row matches when its page is listed here or in `doc_hashes`. Needs
    /// `at` and `source`. Combine with `engine` to withdraw only that
    /// engine's readings of the listed pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_ids: Option<Vec<String>>,
    /// BOINC host ids (`quality.boinc_page_provenance.hostid`): only rows of
    /// pages those hosts read. Needs the provenance files: with none the
    /// release stops. Pages the provenance doesn't resolve to a host
    /// (workunit already purged, or ambiguous) don't match unless
    /// `ambiguous_hosts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<u64>>,
    /// With `hosts`: also the pages whose job ran in two workunits on
    /// different hosts, one of which is listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambiguous_hosts: Option<bool>,
    /// The same by `usnm_state::ocr_local::doc_hash`, 16 lowercase hex
    /// digits each, the keys `ocr_local_pages.bin` holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_hashes: Option<Vec<String>>,
    /// When the decision was made (a page list's provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    /// Where the page list came from: the table, query or comparison that
    /// produced it (a page list's provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip)]
    pub path: String,
    /// `doc_ids` and `doc_hashes` as hashes, built by `check`.
    #[serde(skip)]
    listed: HashSet<u64>,
    /// The pages `hosts` read, bound from the provenance by [`Self::bind`].
    #[serde(skip)]
    host_pages: HashSet<u64>,
}

impl Revocation {
    pub fn matches(&self, row: &StoredRow, run: &str) -> bool {
        let at = super::from_micros(row.ocred_at);
        self.node.as_ref().is_none_or(|n| *n == row.node)
            && self
                .engine
                .as_ref()
                .is_none_or(|e| *e == row.ocr_engine || *e == engine_key(&row.ocr_engine))
            && self.run.as_ref().is_none_or(|r| r == run)
            && self.from.is_none_or(|f| at >= f)
            && self.to.is_none_or(|t| at < t)
            && (!self.has_pages() || self.listed.contains(&doc_hash(&row.doc_id)))
            && (self.hosts.is_none() || self.host_pages.contains(&doc_hash(&row.doc_id)))
    }

    /// Resolves `hosts` to their pages. A revocation naming hosts matches
    /// nothing until this is called.
    pub fn bind(&mut self, provenance: &Provenance) {
        if let Some(h) = &self.hosts {
            self.host_pages = provenance.pages_of(h, self.ambiguous_hosts == Some(true));
        }
    }

    /// Whether it names pages.
    pub fn has_pages(&self) -> bool {
        self.doc_ids.is_some() || self.doc_hashes.is_some()
    }

    /// How many distinct pages it names (with `hosts`, the pages they read,
    /// narrowed to the page list when it has one).
    pub fn page_count(&self) -> usize {
        match (&self.hosts, self.has_pages()) {
            (Some(_), false) => self.host_pages.len(),
            (Some(_), true) => self.listed.intersection(&self.host_pages).count(),
            _ => self.listed.len(),
        }
    }

    fn check(&mut self) -> anyhow::Result<()> {
        if self.node.is_none()
            && self.engine.is_none()
            && self.run.is_none()
            && self.from.is_none()
            && self.to.is_none()
            && !self.has_pages()
            && self.hosts.is_none()
        {
            bail!("names no node, engine, run, time, host or page");
        }
        if self.hosts.as_ref().is_some_and(|h| h.is_empty()) {
            bail!("has an empty host list, which would match every row instead of none");
        }
        if self.ambiguous_hosts.is_some() && self.hosts.is_none() {
            bail!("gives `ambiguous_hosts` without `hosts`");
        }
        self.listed.clear();
        for id in self.doc_ids.iter().flatten() {
            if id.trim().is_empty() || id.trim() != id {
                bail!("lists an empty doc id or one with spaces around it: `{id}`");
            }
            self.listed.insert(doc_hash(id));
        }
        for h in self.doc_hashes.iter().flatten() {
            if h.len() != 16 || !h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                bail!("has a doc hash that isn't 16 lowercase hex digits: `{h}`");
            }
            self.listed
                .insert(u64::from_str_radix(h, 16).context("doc hash")?);
        }
        if self.has_pages() && self.listed.is_empty() {
            bail!("has an empty page list, which would match every row instead of none");
        }
        if self.has_pages()
            && (self.at.is_none() || self.source.as_ref().is_none_or(|s| s.trim().is_empty()))
        {
            bail!("lists pages without `at` (when) and `source` (where the list came from)");
        }
        if self.reason.trim().is_empty() || self.reference.trim().is_empty() {
            bail!("has no reason or reference");
        }
        for (name, v) in [
            ("node", &self.node),
            ("engine", &self.engine),
            ("run", &self.run),
            ("source", &self.source),
        ] {
            if v.as_ref().is_some_and(|v| v.trim().is_empty()) {
                bail!("has an empty `{name}`, which matches nothing");
            }
        }
        if let (Some(from), Some(to)) = (self.from, self.to) {
            if from >= to {
                bail!("has `from` at or after `to`, which matches nothing");
            }
        }
        Ok(())
    }
}

/// Every revocation. One that can't be read stops the release: a
/// withdrawn node's text mustn't go live because its file has a typo.
pub async fn revocations(curated: &dyn ObjectStore) -> anyhow::Result<Vec<Revocation>> {
    let prefix = revoked_prefix();
    let mut out = Vec::new();
    let mut paths = curated.list(&prefix).await?;
    paths.sort();
    for path in paths.into_iter().filter(|p| p.ends_with(".json")) {
        let bytes = curated
            .get(&path)
            .await?
            .with_context(|| format!("`{path}` vanished"))?;
        let mut r: Revocation =
            serde_json::from_slice(&bytes).with_context(|| format!("revocation `{path}`"))?;
        r.check().with_context(|| format!("revocation `{path}`"))?;
        r.path = path;
        out.push(r);
    }
    if out.iter().any(|r| r.hosts.is_some()) {
        let provenance = Provenance::load(curated).await?;
        if provenance.is_empty() {
            bail!(
                "a revocation names hosts but `{}` has no provenance",
                provenance_prefix()
            );
        }
        for r in out.iter_mut() {
            r.bind(&provenance);
        }
    }
    Ok(out)
}

/// A page the completeness gate kept LoC's text on (`short` or `cover`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gated {
    pub doc_id: String,
    pub reason: &'static str,
    pub ours_chars: usize,
    pub loc_chars: usize,
    /// Rounded to four places.
    pub len_ratio: f64,
    pub cover: f64,
}

/// What the release did with `ocr-local`, for `ocr_local.json`.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Tally {
    /// Pages of the batches this release indexes, on the copy the version
    /// keeps, with a reading of ours.
    pub considered: u64,
    pub replaced: u64,
    /// Pages whose LoC text stays, by why: the rule's reasons, `revoked`
    /// and `japanese_ocr` (the Japanese overlay has the page).
    pub kept: BTreeMap<String, u64>,
    /// The pages the rule weighed, by our text's language and by engine.
    pub by_language: BTreeMap<String, Count>,
    pub by_engine: BTreeMap<String, Count>,
    /// Each page the completeness gate ([`rule::Completeness`]) kept LoC's text on, with what it
    /// measured: the per-page reason, in release order.
    pub gated: Vec<Gated>,
    /// The pages weighed by each rule: `v1`, or `line-merge-v1` for line jobs' pages (#1621).
    pub by_rule: BTreeMap<String, Count>,
    /// The parts read, with each title's read once per batch holding it.
    #[serde(skip)]
    parts: BTreeMap<String, InputPart>,
}

impl Tally {
    pub fn keep(&mut self, why: &str) {
        self.considered += 1;
        *self.kept.entry(why.to_owned()).or_default() += 1;
    }

    /// A page the rule weighed: rule v1, or the row's own rule ([`rule::rule_for`]).
    pub fn weighed(&mut self, c: &Candidate, ours: &Scores, verdict: Result<(), Keep>) {
        self.weighed_by(c, ours, verdict, rule::rule_for(&c.row.ocr_engine));
    }

    /// Record why the completeness gate kept LoC's text on `c`'s page, beside the `kept` count
    /// [`Tally::weighed_by`] made for it.
    pub fn gated(&mut self, c: &Candidate, why: Keep, m: &rule::Measure) {
        let r4 = |x: f64| (x * 1e4).round() / 1e4;
        self.gated.push(Gated {
            doc_id: c.row.doc_id.clone(),
            reason: why.as_str(),
            ours_chars: m.ours_chars,
            loc_chars: m.loc_chars,
            len_ratio: r4(m.len_ratio),
            cover: r4(m.cover),
        });
    }

    /// A page `rule` weighed.
    pub fn weighed_by(
        &mut self,
        c: &Candidate,
        ours: &Scores,
        verdict: Result<(), Keep>,
        rule: &str,
    ) {
        self.considered += 1;
        let ok = verdict.is_ok();
        self.replaced += u64::from(ok);
        if let Err(k) = verdict {
            *self.kept.entry(k.as_str().to_owned()).or_default() += 1;
        }
        let lang = ours.lang.clone().unwrap_or_else(|| "none".into());
        for t in [
            self.by_language.entry(lang).or_default(),
            self.by_engine
                .entry(engine_key(&c.row.ocr_engine))
                .or_default(),
            self.by_rule.entry(rule.to_owned()).or_default(),
        ] {
            t.considered += 1;
            t.replaced += u64::from(ok);
        }
    }

    /// Another tally's counts added to this one's (the release's parts,
    /// built in parallel, merged in order).
    pub fn merge(&mut self, other: Tally) -> anyhow::Result<()> {
        for part in other.parts.values() {
            self.add_part(part)?;
        }
        self.considered += other.considered;
        self.replaced += other.replaced;
        self.gated.extend(other.gated);
        for (k, n) in other.kept {
            *self.kept.entry(k).or_default() += n;
        }
        for (mine, theirs) in [
            (&mut self.by_language, other.by_language),
            (&mut self.by_engine, other.by_engine),
            (&mut self.by_rule, other.by_rule),
        ] {
            for (k, c) in theirs {
                let t = mine.entry(k).or_default();
                t.considered += c.considered;
                t.replaced += c.replaced;
            }
        }
        Ok(())
    }

    fn add_part(&mut self, part: &InputPart) -> anyhow::Result<()> {
        if let Some(previous) = self.parts.get(&part.path) {
            anyhow::ensure!(
                previous == part,
                "OCR input part `{}` changed between batch reads",
                part.path
            );
        } else {
            self.parts.insert(part.path.clone(), part.clone());
        }
        Ok(())
    }

    pub fn add_parts(&mut self, parts: &[InputPart]) -> anyhow::Result<()> {
        for part in parts {
            self.add_part(part)?;
        }
        Ok(())
    }

    /// Sorted, distinct inputs consumed by this run, not carried-forward pages.
    pub fn inputs(&self) -> Vec<&InputPart> {
        self.parts.values().collect()
    }

    /// The parts summary: count, bytes and the sha256 of the sorted path
    /// list (one path a line), for the manifest and `ocr_local.json`.
    pub fn parts_summary(&self) -> Value {
        let mut h = Sha256::new();
        for p in self.parts.keys() {
            h.update(p.as_bytes());
            h.update(b"\n");
        }
        json!({
            "count": self.parts.len(),
            "bytes": self.parts.values().map(|p| p.bytes).sum::<u64>(),
            "paths_sha256": hex(&h.finalize()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_core::text::Hyphenation;

    #[test]
    fn input_evidence_deduplicates_and_rejects_changed_bytes_across_batches() {
        let part = InputPart {
            path: "ocr-local/pages/sn1/run.parquet".into(),
            bytes: 42,
            sha256: hex(&Sha256::digest(b"original")),
        };
        let mut tally = Tally::default();
        tally.add_parts(std::slice::from_ref(&part)).unwrap();
        let mut other = Tally::default();
        other.add_parts(std::slice::from_ref(&part)).unwrap();
        tally.merge(other).unwrap();
        assert_eq!(tally.inputs(), vec![&part]);
        assert_eq!(tally.parts_summary()["count"], 1);
        assert_eq!(tally.parts_summary()["bytes"], 42);
        assert_eq!(
            tally.parts_summary()["paths_sha256"],
            hex(&Sha256::digest(format!("{}\n", part.path).as_bytes()))
        );
        for changed in [
            InputPart {
                sha256: hex(&Sha256::digest(b"modified")),
                ..part.clone()
            },
            InputPart {
                bytes: 43,
                ..part.clone()
            },
        ] {
            assert!(tally.add_parts(std::slice::from_ref(&changed)).is_err());
            let mut other = Tally::default();
            other.add_parts(&[changed]).unwrap();
            assert!(tally.merge(other).is_err());
            assert_eq!(tally.inputs(), vec![&part]);
        }
    }

    #[test]
    fn a_page_the_gate_kept_is_recorded_with_its_measure_and_survives_a_merge() {
        let c = Candidate {
            row: row(Some(0.1), 1),
            text: String::new(),
        };
        let m = rule::Measure {
            ours_chars: 25_488,
            loc_chars: 69_836,
            len_ratio: 25_488.0 / 69_836.0,
            cover: 0.2046,
        };
        let mut a = Tally::default();
        a.weighed_by(&c, &Scores::default(), Err(Keep::Short), "v1");
        a.gated(&c, Keep::Short, &m);
        let mut b = Tally::default();
        b.weighed_by(&c, &Scores::default(), Err(Keep::Cover), "v1");
        b.gated(&c, Keep::Cover, &m);
        a.merge(b).unwrap();
        assert_eq!(a.kept["short"], 1);
        assert_eq!(a.kept["cover"], 1);
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["gated"][0]["reason"], "short");
        assert_eq!(json["gated"][1]["reason"], "cover");
        assert_eq!(json["gated"][0]["len_ratio"], 0.365);
        assert_eq!(json["gated"][0]["loc_chars"], 69_836);
        assert_eq!(json["gated"][0]["doc_id"], c.row.doc_id);
    }

    fn row(rate: Option<f64>, at: i64) -> StoredRow {
        StoredRow {
            doc_id: "sn1_1900-01-01_ed-1_seq-1".into(),
            lccn: "sn1".into(),
            batch: "batch_a".into(),
            ocr_source: "usnm-tesseract".into(),
            ocr_engine: "tesseract/5.5.1 profile=eng".into(),
            ocr_reason: "bulk".into(),
            node: "node1".into(),
            metric_version: usnm_core::damage::VERSION,
            ocred_at: at,
            ours: Scores {
                damage_rate: rate,
                ..Scores::default()
            },
            loc: Scores::default(),
        }
    }

    #[test]
    fn the_lowest_stored_rate_wins_then_the_newest() {
        assert!(prefer(
            (&row(Some(0.1), 1), 0, 0),
            (&row(Some(0.2), 9), 1, 0)
        ));
        assert!(!prefer(
            (&row(Some(0.2), 9), 1, 0),
            (&row(Some(0.1), 1), 0, 0)
        ));
        // Ties: the newest, then the later part.
        assert!(prefer(
            (&row(Some(0.1), 2), 0, 0),
            (&row(Some(0.1), 1), 1, 0)
        ));
        assert!(prefer(
            (&row(Some(0.1), 1), 1, 0),
            (&row(Some(0.1), 1), 0, 0)
        ));
        // An unscored row loses to any scored one.
        assert!(prefer((&row(Some(0.9), 0), 0, 0), (&row(None, 9), 1, 0)));
    }

    #[test]
    fn a_revocation_matches_every_selector_it_gives() {
        let r = |v: Value| -> Revocation { serde_json::from_value(v).unwrap() };
        let base = json!({"reason": "bad node", "reference": "#296"});
        let with = |k: &str, v: Value| {
            let mut b = base.clone();
            b[k] = v;
            r(b)
        };
        let x = row(Some(0.1), 1_000_000);
        assert!(with("node", "node1".into()).matches(&x, "run1"));
        assert!(!with("node", "node2".into()).matches(&x, "run1"));
        assert!(with("engine", "tesseract/5.5.1 eng".into()).matches(&x, "run1"));
        assert!(with("engine", "tesseract/5.5.1 profile=eng".into()).matches(&x, "run1"));
        assert!(!with("engine", "kraken/5 eng".into()).matches(&x, "run1"));
        assert!(with("run", "run1".into()).matches(&x, "run1"));
        assert!(!with("run", "run1".into()).matches(&x, "run2"));
        assert!(with("from", "1970-01-01T00:00:01Z".into()).matches(&x, "r"));
        assert!(!with("to", "1970-01-01T00:00:01Z".into()).matches(&x, "r"));
        let mut both = with("node", "node1".into());
        both.run = Some("other".into());
        assert!(!both.matches(&x, "run1"), "every selector must match");
        assert!(r(base.clone()).check().is_err(), "a selector is required");
        assert!(with("node", "node1".into()).check().is_ok());
        assert!(
            with("node", " ".into()).check().is_err(),
            "an empty selector"
        );
        let mut window = with("from", "2026-10-02T00:00:00Z".into());
        window.to = Some("2026-10-01T00:00:00Z".parse().unwrap());
        assert!(window.check().is_err(), "a reversed window");
        window.to = window.from;
        assert!(window.check().is_err(), "an empty window");
        assert!(serde_json::from_value::<Revocation>(json!({
            "reason": "x", "reference": "y", "nodes": "z"
        }))
        .is_err());
    }

    #[test]
    fn a_revocation_by_page_list_matches_listed_pages_only() {
        let checked = |v: Value| -> anyhow::Result<Revocation> {
            let mut r: Revocation = serde_json::from_value(v)?;
            r.check()?;
            Ok(r)
        };
        let x = row(Some(0.1), 1_000_000);
        let other = StoredRow {
            doc_id: "sn2_1900-01-02_ed-1_seq-1".into(),
            ..x.clone()
        };
        let by_id = checked(json!({
            "reason": "worse than LoC", "reference": "#835",
            "at": "2026-11-12T15:00:00Z", "source": "wf1415 table, band 1890/worst",
            "doc_ids": [x.doc_id],
        }))
        .unwrap();
        assert!(by_id.matches(&x, "r"));
        assert!(!by_id.matches(&other, "r"));
        assert_eq!(by_id.page_count(), 1);
        let by_hash = checked(json!({
            "reason": "r", "reference": "#835", "at": "2026-11-12T15:00:00Z",
            "source": "s",
            "doc_hashes": [format!("{:016x}", doc_hash(&x.doc_id))],
        }))
        .unwrap();
        assert!(by_hash.matches(&x, "r") && !by_hash.matches(&other, "r"));
        let scoped = checked(json!({
            "reason": "r", "reference": "#835", "at": "2026-11-12T15:00:00Z",
            "source": "s", "doc_ids": [x.doc_id], "engine": "kraken/5 eng",
        }))
        .unwrap();
        assert!(!scoped.matches(&x, "r"), "the engine must match too");
        // An empty list must not widen the revocation to the whole engine.
        for key in ["doc_ids", "doc_hashes"] {
            assert!(checked(json!({
                "reason": "r", "reference": "#835", "at": "2026-11-12T15:00:00Z",
                "source": "s", "engine": "tesseract/5.5.1 eng", (key): [],
            }))
            .is_err());
        }
        // Provenance is required, and hashes must be well formed.
        assert!(checked(json!({
            "reason": "r", "reference": "#835", "doc_ids": [x.doc_id],
        }))
        .is_err());
        assert!(checked(json!({
            "reason": "r", "reference": "#835", "at": "2026-11-12T15:00:00Z",
            "source": " ", "doc_ids": [x.doc_id],
        }))
        .is_err());
        assert!(checked(json!({
            "reason": "r", "reference": "#835", "at": "2026-11-12T15:00:00Z",
            "source": "s", "doc_hashes": ["ABC"],
        }))
        .is_err());
    }

    /// A part `ocr-sync` scored with another metric version: its stored
    /// scores read as none, so they rank last and estimate nothing.
    #[test]
    fn scores_of_another_metric_version_are_not_compared() {
        use std::sync::Arc;
        let text = "The city council met on Tuesday and voted for the new school on the hill \
            and the mayor said the railroad would bring more trade to the town and its farms.";
        let imported = pages::Imported {
            row: usnm_overlay::OverlayRow {
                key: usnm_core::ids::PageKey::new(
                    "sn1",
                    chrono::NaiveDate::from_ymd_opt(1900, 1, 1).unwrap(),
                    1,
                    1,
                )
                .unwrap(),
                batch: "batch_a".into(),
                ocr_source: "usnm-tesseract".into(),
                ocr_engine: "tesseract/5.5.1 profile=eng".into(),
                loc_text: "bulk".into(),
                text: text.into(),
                image_url: "https://tile.loc.gov/x".into(),
                ocred_at: chrono::DateTime::from_timestamp(1, 0).unwrap(),
            },
            batch_version: 1,
            source_part: "pages/x.parquet".into(),
            node: "node1".into(),
            reason: "bulk".into(),
            ours: Scores::of(text, &["eng"]),
            loc: Scores::of("", &["eng"]),
        };
        let bytes = Bytes::from(pages::encode(&[imported]).unwrap());
        let now = pages::read_scores(bytes.clone()).unwrap();
        assert_eq!(now[0].metric_version, usnm_core::damage::VERSION);
        assert!(now[0].ours.damage_rate.is_some());
        // The same part as an older metric wrote it.
        let builder =
            parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
        let schema = builder.schema().clone();
        let reader = builder.build().unwrap();
        let batch = reader.into_iter().next().unwrap().unwrap();
        let i = batch.schema().index_of("metric_version").unwrap();
        let mut cols = batch.columns().to_vec();
        cols[i] = Arc::new(arrow_array::Int32Array::from(vec![1]));
        let old = arrow_array::RecordBatch::try_new(schema, cols).unwrap();
        let mut w = parquet::arrow::ArrowWriter::try_new(Vec::new(), old.schema(), None).unwrap();
        w.write(&old).unwrap();
        let rows = pages::read_scores(Bytes::from(w.into_inner().unwrap())).unwrap();
        assert_eq!(rows[0].metric_version, 1);
        assert_eq!(rows[0].ours, Scores::default());
        assert!(
            prefer((&now[0], 0, 0), (&rows[0], 1, 0)),
            "the current metric's row wins"
        );
        assert!(rule::verdict(&rows[0].ours, &rows[0].loc, None).is_err());
    }

    /// An `Imported` page of title `sn1` with `text`, read by `ocr_engine`.
    fn imported(seq: u16, ocr_engine: &str, text: &str) -> pages::Imported {
        imported_scored(seq, ocr_engine, text, Scores::of(text, &["deu"]))
    }

    /// The same, with the scores `ocr-sync` stored for our text.
    fn imported_scored(seq: u16, ocr_engine: &str, text: &str, ours: Scores) -> pages::Imported {
        let loc = Scores::of("", &["deu"]);
        pages::Imported {
            row: usnm_overlay::OverlayRow {
                key: usnm_core::ids::PageKey::new(
                    "sn1",
                    chrono::NaiveDate::from_ymd_opt(1900, 1, 1).unwrap(),
                    1,
                    seq,
                )
                .unwrap(),
                batch: "batch_a".into(),
                ocr_source: "usnm-tesseract".into(),
                ocr_engine: ocr_engine.into(),
                loc_text: "bulk".into(),
                text: text.into(),
                image_url: "https://tile.loc.gov/x".into(),
                ocred_at: chrono::DateTime::from_timestamp(1, 0).unwrap(),
            },
            batch_version: 1,
            source_part: "pages/x.parquet".into(),
            node: "node1".into(),
            reason: "bulk".into(),
            ours,
            loc,
        }
    }

    /// Pages stored before the line-end `⸗`, `¬`, `‐` and `—` rules (#1065)
    /// are normalized again as they are loaded, for the engine that read them.
    #[tokio::test]
    async fn loads_text_normalized_again_for_its_engine() {
        const FRAKTUR: &str = "tesseract/5.5.1 profile=frak2021 image=sha256:3f9a0c1d2e4b \
            models=sha256:8c2d71aa09fe route=minimal@r1 contract=0";
        const ENGLISH: &str = "tesseract/5.5.1 profile=eng image=sha256:3f9a0c1d2e4b \
            models=sha256:8c2d71aa09fe route=minimal@r1 contract=0";
        // As a worker stored it before: only `-` was rejoined.
        let stored =
            "durch den engen Haus\u{2014}\ngang schritt, weiter und Draht\u{2E17}\nzaun,\n\
            Haus\u{2014}\nCunow";
        let fraktur =
            "durch den engen Hausgang schritt, weiter und Drahtzaun,\nHaus\u{2014}\nCunow";
        let other = "durch den engen Haus\u{2014}\ngang schritt, weiter und Drahtzaun,\nHaus\u{2014}\nCunow";
        let dir = tempfile::tempdir().unwrap();
        let curated = usnm_store::open(dir.path().to_str().unwrap()).unwrap();
        let rows = [
            imported(1, FRAKTUR, stored),
            imported(2, ENGLISH, stored),
            // A legacy tag that names a Fraktur model.
            imported(3, "tesseract-5.5.1/deu_latf+deu", stored),
            imported(4, "tesseract-5.5.1/eng", stored),
        ];
        let part = pages::encode(&rows).unwrap();
        let expected_input = InputPart {
            path: format!("{ROOT}/pages/sn1/20261011T000000Z-ab.parquet"),
            bytes: part.len() as u64,
            sha256: hex(&Sha256::digest(&part)),
        };
        curated
            .put_new(
                &format!("{ROOT}/pages/sn1/20261011T000000Z-ab.parquet"),
                part,
                "x",
            )
            .await
            .unwrap();
        let listing = Listing::list(curated.as_ref()).await.unwrap();
        let loaded = listing
            .load(curated.as_ref(), ["sn1"].into_iter(), &[])
            .await
            .unwrap();
        assert_eq!(loaded.parts, vec![expected_input]);
        let text = |seq: u16| {
            loaded.pages[&format!("sn1_1900-01-01_ed-1_seq-{seq}")]
                .text
                .as_str()
        };
        assert_eq!(text(1), fraktur);
        assert_eq!(text(2), other);
        assert_eq!(text(3), fraktur);
        assert_eq!(text(4), other);
        // The stored scores are the ones that chose the row.
        let row = &loaded.pages["sn1_1900-01-01_ed-1_seq-1"].row;
        assert_eq!(row.ours, Scores::of(stored, &["deu"]));
    }

    fn provenance_lines(lines: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for l in lines {
            out.extend(serde_json::to_vec(l).unwrap());
            out.push(b'\n');
        }
        out
    }

    #[test]
    fn provenance_resolves_pages_to_hosts_and_a_resolved_row_is_never_degraded() {
        let mut p = Provenance::default();
        p.add(&provenance_lines(&[
            json!({"doc_id": "a", "status": "resolved", "hostid": 2, "family": "amd64"}),
            json!({"doc_id": "b", "status": "resolved", "hostid": 9}),
            json!({"doc_id": "c", "status": "ambiguous_duplicate_wu", "candidate_host_ids": [2, 4]}),
            json!({"doc_id": "d", "status": "no_wu"}),
        ]))
        .unwrap();
        // A later file with less on a page doesn't undo what an earlier one resolved.
        p.add(&provenance_lines(&[
            json!({"doc_id": "a", "status": "no_wu"}),
            json!({"doc_id": "c", "status": "resolved", "hostid": 4}),
        ]))
        .unwrap();
        let h = |ids: &[&str]| -> HashSet<u64> { ids.iter().map(|i| doc_hash(i)).collect() };
        assert_eq!(p.pages_of(&[2], false), h(&["a"]));
        assert_eq!(p.pages_of(&[2, 9], false), h(&["a", "b"]));
        // `c` resolved later, so it is no longer ambiguous.
        assert_eq!(p.pages_of(&[4], true), h(&["c"]));
        assert!(p.pages_of(&[77], true).is_empty());
        let mut q = Provenance::default();
        q.add(&provenance_lines(&[
            json!({"doc_id": "c", "status": "ambiguous_duplicate_wu", "candidate_host_ids": [2, 4]}),
        ]))
        .unwrap();
        assert!(q.pages_of(&[2], false).is_empty());
        assert_eq!(q.pages_of(&[2], true), h(&["c"]));
        assert!(Provenance::default().is_empty() && !q.is_empty());
        // A resolved line without a host, or a line that isn't JSON, is refused.
        assert!(Provenance::default()
            .add(br#"{"doc_id": "a", "status": "resolved"}"#)
            .is_err());
        assert!(Provenance::default().add(b"not json\n").is_err());
    }

    #[test]
    fn a_revocation_by_host_matches_only_pages_that_host_read() {
        let checked = |v: Value| -> anyhow::Result<Revocation> {
            let mut r: Revocation = serde_json::from_value(v)?;
            r.check()?;
            Ok(r)
        };
        let base = json!({"reason": "host 2 held (#1956)", "reference": "#1855"});
        let with = |k: &str, v: Value| {
            let mut b = base.clone();
            b[k] = v;
            b
        };
        let x = row(Some(0.1), 1_000_000);
        let other = StoredRow {
            doc_id: "sn2_1900-01-02_ed-1_seq-1".into(),
            ..x.clone()
        };
        let mut p = Provenance::default();
        p.add(&provenance_lines(&[
            json!({"doc_id": x.doc_id, "status": "resolved", "hostid": 2}),
            json!({"doc_id": other.doc_id, "status": "resolved", "hostid": 9}),
        ]))
        .unwrap();
        let mut by_host = checked(with("hosts", json!([2]))).unwrap();
        assert!(!by_host.matches(&x, "r"), "unbound, it matches nothing");
        by_host.bind(&p);
        assert!(by_host.matches(&x, "r") && !by_host.matches(&other, "r"));
        assert_eq!(by_host.page_count(), 1);
        // Every selector must match: the engine, and a page list.
        let mut scoped = checked(with("hosts", json!([2, 9]))).unwrap();
        scoped.engine = Some("kraken/5 eng".into());
        scoped.bind(&p);
        assert!(!scoped.matches(&x, "r"));
        let mut listed = checked(json!({
            "reason": "r", "reference": "#1855", "hosts": [2, 9],
            "at": "2026-11-14T00:00:00Z", "source": "s", "doc_ids": [other.doc_id],
        }))
        .unwrap();
        listed.bind(&p);
        assert!(!listed.matches(&x, "r") && listed.matches(&other, "r"));
        assert_eq!(listed.page_count(), 1);
        // An empty list must not widen the revocation, and `ambiguous_hosts` needs hosts.
        assert!(checked(with("hosts", json!([]))).is_err());
        assert!(checked(with("ambiguous_hosts", json!(true))).is_err());
        assert!(checked(base.clone()).is_err());
    }

    /// A page of a held host falls back to LoC at release; another host's page of the same
    /// engine keeps ours.
    #[tokio::test]
    async fn a_host_hold_withdraws_that_hosts_pages_in_the_release_load() {
        const ENGLISH: &str = "tesseract/5.5.1 profile=eng image=sha256:3f9a0c1d2e4b \
            models=sha256:8c2d71aa09fe route=boinc@r1 contract=0";
        let text = "durch den engen Hausgang schritt, weiter und Drahtzaun";
        let dir = tempfile::tempdir().unwrap();
        let curated = usnm_store::open(dir.path().to_str().unwrap()).unwrap();
        let rows = [imported(1, ENGLISH, text), imported(2, ENGLISH, text)];
        curated
            .put_new(
                &format!("{ROOT}/pages/sn1/20261011T000000Z-ab.parquet"),
                pages::encode(&rows).unwrap(),
                "x",
            )
            .await
            .unwrap();
        let held = "sn1_1900-01-01_ed-1_seq-1";
        let kept = "sn1_1900-01-01_ed-1_seq-2";
        let revoke = json!({"reason": "host 2 held", "reference": "#1855", "hosts": [2]});
        curated
            .put_new(
                &format!("{ROOT}/revoked/host2.json"),
                serde_json::to_vec(&revoke).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        // Without provenance the release stops rather than revoking nothing.
        assert!(revocations(curated.as_ref()).await.is_err());
        curated
            .put_new(
                &format!("{ROOT}/provenance/20261114T000000Z.jsonl"),
                provenance_lines(&[
                    json!({"doc_id": held, "status": "resolved", "hostid": 2}),
                    json!({"doc_id": kept, "status": "resolved", "hostid": 9}),
                ]),
                "application/x-ndjson",
            )
            .await
            .unwrap();
        let revoked = revocations(curated.as_ref()).await.unwrap();
        let listing = Listing::list(curated.as_ref()).await.unwrap();
        let loaded = listing
            .load(curated.as_ref(), ["sn1"].into_iter(), &revoked)
            .await
            .unwrap();
        assert!(
            !loaded.pages.contains_key(held),
            "the held host's page falls back to LoC"
        );
        assert!(loaded.revoked.contains(held));
        assert!(loaded.pages.contains_key(kept));
    }

    /// An engine tag for `profile`, as a worker writes it.
    fn tag(profile: &str) -> String {
        format!(
            "tesseract/5.5.1 profile={profile} image=sha256:3f9a0c1d2e4b \
            models=sha256:8c2d71aa09fe route=minimal@r1 contract=0"
        )
    }

    /// A row as `ocr-sync` read it, with our text's detected language.
    fn row_in(engine: &str, lang: Option<&str>) -> StoredRow {
        StoredRow {
            ocr_engine: engine.into(),
            ours: Scores {
                lang: lang.map(str::to_owned),
                ..Scores::default()
            },
            ..row(None, 0)
        }
    }

    #[test]
    fn a_stored_row_gives_its_page_the_language_it_was_scored_in() {
        let lang = |l: Option<&str>| row_in(&tag("eng"), l).language();
        assert_eq!(lang(Some("ger")), Some("deu"));
        assert_eq!(lang(Some("eng")), Some("eng"));
        assert_eq!(lang(Some("cze")), Some("ces"));
        // No language: not detected, no words, another metric version's
        // scores (read as none), or not a catalog code.
        assert_eq!(lang(Some("und")), None);
        assert_eq!(lang(None), None);
        assert_eq!(lang(Some("deu")), None);
        assert_eq!(lang(Some("")), None);
        assert_eq!(row(None, 0).language(), None);

        let rule = |engine: &str, l: Option<&str>| row_in(&tag(engine), l).hyphenation();
        // A German page takes the Fraktur rule with any model.
        assert_eq!(rule("deu", Some("ger")), Hyphenation::Fraktur);
        assert_eq!(rule("eng", Some("ger")), Hyphenation::Fraktur);
        // A Fraktur model does with any language, or none.
        for l in [Some("ger"), Some("eng"), Some("und"), None] {
            assert_eq!(rule("frak2021", l), Hyphenation::Fraktur, "{l:?}");
            assert_eq!(rule("deu_latf+deu", l), Hyphenation::Fraktur, "{l:?}");
        }
        // Otherwise the engine alone decides, as before a row had a language.
        for l in [Some("eng"), Some("fre"), Some("und"), None] {
            assert_eq!(rule("deu", l), Hyphenation::Hyphens, "{l:?}");
            assert_eq!(rule("eng", l), Hyphenation::Hyphens, "{l:?}");
        }
        assert_eq!(
            row_in("tesseract-5.5.1/deu", Some("ger")).hyphenation(),
            Hyphenation::Fraktur
        );
        assert_eq!(
            row_in("tesseract-5.5.1/deu", None).hyphenation(),
            Hyphenation::Hyphens
        );
    }

    /// A German page read with a model that isn't a Fraktur one gets the
    /// em-dash rule at load when the row's scores say the page is German,
    /// as a worker or an import gives it now; with no language, as before.
    #[tokio::test]
    async fn loads_a_german_page_with_the_language_it_was_scored_in() {
        const PAGE: &str = "Der Stadtrath hat am Dienstag beschlossen, dass die neue Schule auf dem \
            Grund{dash}\nstück gebaut werden soll, welches die Behörde im vorigen Jahre gekauft hat. \
            Es ist noch nicht bekannt, wann die Arbeiten beginnen werden, aber man hofft, dass es bald \
            sein wird und dass die Kinder im nächsten Jahre in das neue Schul{dash}\nhaus ziehen können. \
            Der Rath der Stadt{dash}\nCunow sprach dann über die Steuern. ";
        const ENGLISH: &str =
            "The city council met on Tuesday and voted for the new school on the \
            hill and the mayor said the railroad{dash}\nwould bring more trade to the town and its \
            farms, and the people of the county were glad of it. ";
        let dash = |s: &str| {
            s.replace("{dash}", "\u{2014}")
                .repeat(3)
                .trim_end()
                .to_owned()
        };
        let (stored, english) = (dash(PAGE), dash(ENGLISH));
        // What a worker stores now: the dash after a letter, before a
        // lowercase letter, joined; `Cunow` starts a new word.
        let joined = stored
            .replace("Grund\u{2014}\nstück", "Grundstück")
            .replace("Schul\u{2014}\nhaus", "Schulhaus");
        assert_ne!(joined, stored);
        assert!(joined.contains("Stadt\u{2014}\nCunow"));
        let (deu, frak, en) = (tag("deu"), tag("frak2021"), tag("eng"));
        assert_eq!(
            joined,
            normalize_ocr_for(&stored, Hyphenation::for_page(&deu, "deu")),
            "what a worker stores for a German page"
        );

        let german = Scores::of(&stored, &["ger"]);
        assert_eq!(german.lang.as_deref(), Some("ger"));
        let eng = Scores::of(&english, &["eng"]);
        assert_eq!(eng.lang.as_deref(), Some("eng"));
        let undecided = Scores {
            lang: Some("und".into()),
            ..german.clone()
        };
        let rows = [
            // German, read with a model that is not a Fraktur one.
            imported_scored(1, &deu, &stored, german.clone()),
            imported_scored(2, &en, &stored, german.clone()),
            imported_scored(3, "tesseract-5.5.1/deu", &stored, german.clone()),
            // The same text and engine with no language known: unchanged.
            imported_scored(4, &deu, &stored, Scores::default()),
            imported_scored(5, &deu, &stored, undecided.clone()),
            // A page scored as English keeps its dash.
            imported_scored(6, &deu, &english, eng.clone()),
            imported_scored(7, &en, &english, eng),
            // A Fraktur model, whatever the language.
            imported_scored(8, &frak, &stored, german.clone()),
            imported_scored(9, &frak, &stored, Scores::default()),
            imported_scored(10, &frak, &stored, undecided),
            // Text a worker stored with the language: nothing left to join.
            imported_scored(11, &deu, &joined, Scores::of(&joined, &["ger"])),
        ];
        let dir = tempfile::tempdir().unwrap();
        let curated = usnm_store::open(dir.path().to_str().unwrap()).unwrap();
        curated
            .put_new(
                &format!("{ROOT}/pages/sn1/20261011T000000Z-ab.parquet"),
                pages::encode(&rows).unwrap(),
                "x",
            )
            .await
            .unwrap();
        let listing = Listing::list(curated.as_ref()).await.unwrap();
        let loaded = listing
            .load(curated.as_ref(), ["sn1"].into_iter(), &[])
            .await
            .unwrap();
        let text = |seq: u16| {
            loaded.pages[&format!("sn1_1900-01-01_ed-1_seq-{seq}")]
                .text
                .as_str()
        };
        for seq in [1, 2, 3, 8, 9, 10] {
            assert_eq!(text(seq), joined, "seq {seq}");
        }
        for seq in [4, 5] {
            assert_eq!(text(seq), stored, "seq {seq}");
        }
        for seq in [6, 7] {
            assert_eq!(text(seq), english, "seq {seq}");
        }
        assert_eq!(text(11), joined);
        // Normalizing again, as a worker would have, changes nothing.
        for seq in 1..=11 {
            let row = &loaded.pages[&format!("sn1_1900-01-01_ed-1_seq-{seq}")].row;
            let once = text(seq);
            assert_eq!(
                normalize_ocr_for(once, row.hyphenation()),
                once,
                "seq {seq}"
            );
            assert_eq!(
                normalize_ocr_for(once, Hyphenation::Hyphens),
                once,
                "seq {seq}"
            );
        }
    }

    #[test]
    fn names_the_import_run_of_a_part() {
        assert_eq!(
            run_of("ocr-local/pages/sn1/20261011T000000Z-ab.parquet"),
            "20261011T000000Z-ab"
        );
    }
}
