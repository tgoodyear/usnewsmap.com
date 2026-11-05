//! How Newspaper Navigator's boxes meet the curated pages (#1102, 04 §4.11):
//! `usnm-ingest pictures-join`, a report that changes no store.
//!
//! A row is keyed by its scan (batch, LCCN, reel, issue, frame), a curated
//! page by `page_key` (LCCN, date, edition, seq). The frame is the JP2's
//! number on its reel, not the page's `seq` ([`super::url`]), so a row is put
//! on a page only through a placement: a line that names a page's `doc_id`
//! and the IIIF base of the scan it was fetched from, as a scan fetcher's
//! prefetch index (`index-{tag}.jsonl`) and sidecars record them. Nothing
//! here makes a `seq` out of a frame.
//!
//! Each row gets the first class that fits, from the most certain to the
//! least, by the placements, each curated batch's `counts.json` and the page
//! keys of the curated batches that the placements' scans are in:
//!
//! - `page`: placed on a `doc_id` that is a page of the curated version of
//!   the batch it was detected in (that version's parts hold the page key).
//! - `same_batch_day`: not placed on such a page, but that batch version is
//!   curated with the title-day; the page is probably there, at a `seq` not
//!   known here.
//! - `other_version_day`: another version of the batch is curated with the
//!   title-day (the batch was reissued after 2020).
//! - `other_batch_day`: only another batch is curated with the title-day.
//! - `not_curated`: no curated batch has pages of the title on that day.
//! - `unplaced`: the row has no full scan identity, or its batch has no
//!   `_verNN` version.
//!
//! `counts.json` is by day, not by edition, so the day classes don't tell a
//! day's editions apart.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use bytes::Bytes;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usnm_core::ids::PageKey;
use usnm_core::time::day_number;
use usnm_store::ObjectStore;

use super::url::scan_of_iiif_base;
use super::{attempt_dir, finished, read_scans, PartRef, ScanFields, PREFIX, SCHEMA_VERSION};
use crate::dedup::load_counts;
use crate::source::hex;
use crate::state::{RunBatch, State};

/// The `batches` key of rows without a batch.
pub const NO_BATCH: &str = "(none)";

/// A scan as a row and a placement both name it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ScanKey {
    /// The batch directory (`ohi_ingstad_ver01`).
    batch: String,
    /// In lower case.
    lccn: String,
    reel: String,
    date: NaiveDate,
    edition: u32,
    frame: String,
}

impl ScanKey {
    /// The scan a row names, if it names all of one.
    fn of(f: &ScanFields<'_>) -> Option<Self> {
        let (Some(batch), Some(lccn), Some(reel), Some(date), Some(edition), Some(frame)) =
            (f.batch, f.lccn, f.reel, f.date, f.edition, f.frame)
        else {
            return None;
        };
        Some(Self {
            batch: batch.to_owned(),
            lccn: lccn.to_ascii_lowercase(),
            reel: reel.to_owned(),
            date,
            edition,
            frame: frame.to_owned(),
        })
    }
}

/// What the placement files held.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementSummary {
    pub files: u64,
    /// Lines that aren't blank.
    pub lines: u64,
    /// Scans put on a page, each on one.
    pub placed: u64,
    /// Lines that aren't JSON with a `doc_id` and an `iiif_base` that names
    /// a scan.
    pub unreadable: u64,
    /// Lines whose `doc_id` is of another title, date or edition than the
    /// scan's issue directory.
    pub inconsistent: u64,
    /// Distinct scan and page pairs (not lines: a pair on many lines counts
    /// once) left out because the scan was given more than one page or the
    /// page more than one scan.
    pub conflicting: u64,
}

/// Scans put on pages.
#[derive(Debug, Default)]
pub struct Placements {
    /// Scan → index into `doc_ids`.
    by_scan: HashMap<ScanKey, u32>,
    doc_ids: Vec<String>,
    pub summary: PlacementSummary,
}

/// The fields of a placement line that are read; any others are ignored.
#[derive(Deserialize)]
struct Line {
    doc_id: String,
    iiif_base: String,
}

enum Placed {
    Unreadable,
    Inconsistent,
    Scan(ScanKey, String),
}

fn placement(line: &str) -> Placed {
    let Ok(Line { doc_id, iiif_base }) = serde_json::from_str::<Line>(line) else {
        return Placed::Unreadable;
    };
    let (Ok(page), Some((batch, scan))) =
        (PageKey::from_doc_id(&doc_id), scan_of_iiif_base(&iiif_base))
    else {
        return Placed::Unreadable;
    };
    if !page.lccn.eq_ignore_ascii_case(&scan.lccn)
        || page.date != scan.date
        || u32::from(page.edition) != scan.edition
    {
        return Placed::Inconsistent;
    }
    let lccn = scan.lccn_key();
    Placed::Scan(
        ScanKey {
            batch,
            lccn,
            reel: scan.reel,
            date: scan.date,
            edition: scan.edition,
            frame: scan.frame,
        },
        page.doc_id(),
    )
}

impl Placements {
    /// Read JSON Lines files whose lines have a `doc_id` and the `iiif_base`
    /// of the scan it was fetched from, as a scan fetcher's
    /// `index-{tag}.jsonl` and sidecars do. The same pair on many lines (one
    /// scan fetched at two resolutions) counts once.
    ///
    /// One pass holds each scan and page once, with the first page or scan
    /// it was given; only a scan's later pages are kept apart, and those are
    /// all conflicts.
    pub fn read(files: &[PathBuf]) -> anyhow::Result<Self> {
        let mut scans: HashMap<ScanKey, u32> = HashMap::new();
        let mut docs: HashMap<String, u32> = HashMap::new();
        // By scan: its first page, and whether it was given another.
        let mut first_doc: Vec<u32> = Vec::new();
        let mut scan_twice: Vec<bool> = Vec::new();
        // By page: whether it was given a second scan.
        let mut doc_twice: Vec<bool> = Vec::new();
        // (scan, page) pairs past the scan's first page.
        let mut later: HashSet<(u32, u32)> = HashSet::new();
        let mut summary = PlacementSummary::default();
        for path in files {
            let file = std::fs::File::open(path)
                .with_context(|| format!("opening `{}`", path.display()))?;
            summary.files += 1;
            for line in std::io::BufReader::new(file).lines() {
                let line = line.with_context(|| format!("reading `{}`", path.display()))?;
                if line.trim().is_empty() {
                    continue;
                }
                summary.lines += 1;
                match placement(&line) {
                    Placed::Unreadable => summary.unreadable += 1,
                    Placed::Inconsistent => summary.inconsistent += 1,
                    Placed::Scan(scan, doc_id) => {
                        let next = u32::try_from(scans.len()).context("too many placements")?;
                        let s = *scans.entry(scan).or_insert(next);
                        let new_scan = s == next;
                        let next = u32::try_from(docs.len()).context("too many placements")?;
                        let d = *docs.entry(doc_id).or_insert(next);
                        let new_doc = d == next;
                        if new_scan {
                            first_doc.push(d);
                            scan_twice.push(false);
                        }
                        if new_doc {
                            doc_twice.push(false);
                        }
                        let fresh = new_scan
                            || (first_doc[s as usize] != d && {
                                scan_twice[s as usize] = true;
                                later.insert((s, d))
                            });
                        // A page seen before, in a pair not seen before, had
                        // another scan.
                        if fresh && !new_doc {
                            doc_twice[d as usize] = true;
                        }
                    }
                }
            }
        }
        let mut doc_ids: Vec<Option<String>> = vec![None; docs.len()];
        for (doc_id, d) in docs {
            doc_ids[d as usize] = Some(doc_id);
        }
        summary.conflicting = later.len() as u64;
        let mut out = Self::default();
        scans.retain(|_, s| {
            let d = first_doc[*s as usize] as usize;
            // A page with one scan is taken by that scan alone.
            let doc_id = (!scan_twice[*s as usize] && !doc_twice[d])
                .then(|| doc_ids[d].take())
                .flatten();
            let Some(doc_id) = doc_id else {
                summary.conflicting += 1;
                return false;
            };
            // Fewer than `docs`, whose indices fit.
            *s = out.doc_ids.len() as u32;
            out.doc_ids.push(doc_id);
            true
        });
        out.by_scan = scans;
        summary.placed = out.doc_ids.len() as u64;
        out.summary = summary;
        Ok(out)
    }
}

/// Each curated batch's title-days.
#[derive(Debug, Default)]
struct CuratedDays {
    /// Batch name (without `_verNN`) → (index, curated version).
    batches: HashMap<String, (u32, u16)>,
    /// LCCN in lower case → title number.
    titles: HashMap<String, u32>,
    /// (title, day) → the first batch with pages on it; `more` has the
    /// others, for the few title-days that more than one batch has. A full
    /// corpus has a few million title-days.
    first: HashMap<(u32, u32), u32>,
    more: HashMap<(u32, u32), BTreeSet<u32>>,
}

impl CuratedDays {
    async fn load(curated: &dyn ObjectStore, batches: &[RunBatch]) -> anyhow::Result<Self> {
        let mut out = Self::default();
        for (i, b) in batches.iter().enumerate() {
            let i = u32::try_from(i).context("too many batches")?;
            out.batches.insert(b.batch.clone(), (i, b.curated.version));
            for (lccn, days) in load_counts(curated, b).await? {
                let lccn = lccn.to_ascii_lowercase();
                let title = match out.titles.get(&lccn) {
                    Some(&title) => title,
                    None => {
                        let title = u32::try_from(out.titles.len()).context("too many titles")?;
                        out.titles.insert(lccn, title);
                        title
                    }
                };
                for (day, pages) in days {
                    if pages == 0 {
                        continue;
                    }
                    match out.first.entry((title, day)) {
                        Entry::Vacant(e) => {
                            e.insert(i);
                        }
                        Entry::Occupied(e) if *e.get() != i => {
                            out.more.entry((title, day)).or_default().insert(i);
                        }
                        Entry::Occupied(_) => {}
                    }
                }
            }
        }
        Ok(out)
    }

    /// Whether `batch` has pages of `title` on `day`.
    fn on(&self, title_day: (u32, u32), batch: u32) -> bool {
        self.first.get(&title_day) == Some(&batch)
            || self
                .more
                .get(&title_day)
                .is_some_and(|more| more.contains(&batch))
    }

    /// `on_page`: the scan is placed on a page its batch version holds.
    fn class(&self, scan: &ScanKey, on_page: bool) -> Class {
        let Some((name, version)) = split_batch(&scan.batch) else {
            return Class::Unplaced;
        };
        let title_day = match self.titles.get(&scan.lccn) {
            Some(&title) => (title, day_number(scan.date)),
            None => return Class::NotCurated,
        };
        match self.batches.get(name) {
            Some(&(i, curated)) if self.on(title_day, i) => {
                if curated != version {
                    Class::OtherVersionDay
                } else if on_page {
                    Class::Page
                } else {
                    Class::SameBatchDay
                }
            }
            _ if self.first.contains_key(&title_day) => Class::OtherBatchDay,
            _ => Class::NotCurated,
        }
    }
}

/// `ohi_ingstad_ver01` → (`ohi_ingstad`, 1).
fn split_batch(dir: &str) -> Option<(&str, u16)> {
    let (name, version) = dir.rsplit_once("_ver")?;
    if name.is_empty() || version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((name, version.parse().ok()?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Page,
    SameBatchDay,
    OtherVersionDay,
    OtherBatchDay,
    NotCurated,
    Unplaced,
}

/// Rows by class.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classes {
    pub rows: u64,
    pub page: u64,
    pub same_batch_day: u64,
    pub other_version_day: u64,
    pub other_batch_day: u64,
    pub not_curated: u64,
    pub unplaced: u64,
}

impl Classes {
    fn add(&mut self, class: Class) {
        self.rows += 1;
        *match class {
            Class::Page => &mut self.page,
            Class::SameBatchDay => &mut self.same_batch_day,
            Class::OtherVersionDay => &mut self.other_version_day,
            Class::OtherBatchDay => &mut self.other_batch_day,
            Class::NotCurated => &mut self.not_curated,
            Class::Unplaced => &mut self.unplaced,
        } += 1;
    }
}

/// One kind's rows by class, and the pages its `page` rows are on.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindJoin {
    #[serde(flatten)]
    pub classes: Classes,
    pub pages: u64,
}

/// What `pictures-join` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinReport {
    pub revision: String,
    pub attempt: u32,
    pub curated_batches: u64,
    pub curated_title_days: u64,
    pub placements: PlacementSummary,
    /// Placements whose `doc_id` is a page of the curated version of the
    /// scan's batch; only these make `page` rows.
    pub placements_on_curated_pages: u64,
    pub total: Classes,
    /// Distinct pages with a `page` row.
    pub pages: u64,
    pub kinds: BTreeMap<String, KindJoin>,
    /// By the row's batch directory ([`NO_BATCH`] for none).
    pub batches: BTreeMap<String, Classes>,
}

/// Give `on_row` each row's scan fields from a part, once the part's size and
/// SHA-256 are what the provenance records. Reads only those columns. A part
/// whose rows or kind are not what the provenance records stops the report.
async fn verified_part(
    store: &dyn ObjectStore,
    dir: &str,
    part: &PartRef,
    kind: &str,
    mut on_row: impl FnMut(ScanFields<'_>),
) -> anyhow::Result<()> {
    let path = &part.path;
    let inside = path
        .strip_prefix(dir)
        .and_then(|p| p.strip_prefix('/'))
        .is_some_and(|rest| {
            rest.split('/')
                .all(|c| !c.is_empty() && c != "." && c != "..")
        });
    if !inside {
        bail!("`{path}` is outside the attempt `{dir}`");
    }
    let body = store
        .get(path)
        .await?
        .with_context(|| format!("`{path}` is missing"))?;
    let sha256 = hex(&Sha256::digest(&body));
    if body.len() as u64 != part.bytes || sha256 != part.sha256 {
        bail!(
            "`{path}` is not the part provenance.json records: {} bytes with SHA-256 {sha256}, not {} bytes with {}",
            body.len(),
            part.bytes,
            part.sha256
        );
    }
    let mut other_kind = false;
    let rows = read_scans(Bytes::from(body), |f| {
        if f.kind == kind {
            on_row(f);
        } else {
            other_kind = true;
        }
        Ok(())
    })
    .with_context(|| format!("reading `{path}`"))?;
    if rows != part.rows {
        bail!(
            "`{path}` has {rows} rows; provenance.json records {}",
            part.rows
        );
    }
    if other_kind {
        bail!("`{path}` has rows of another kind than `{kind}`");
    }
    Ok(())
}

/// The placements (indices into `doc_ids`) whose `doc_id` is a page of the
/// curated version of the scan's batch. Reads the curated parts (without
/// their text) of only the batches the placements' scans are in, when the
/// scan is of the batch's curated version, and stops reading a batch's parts
/// once all its pages are found.
async fn on_curated_pages(
    curated: &dyn ObjectStore,
    batches: &[RunBatch],
    days: &CuratedDays,
    placements: &Placements,
) -> anyhow::Result<HashSet<u32>> {
    let mut wanted: BTreeMap<u32, HashMap<PageKey, u32>> = BTreeMap::new();
    for (scan, &i) in &placements.by_scan {
        let Some((name, version)) = split_batch(&scan.batch) else {
            continue;
        };
        match days.batches.get(name) {
            Some(&(b, curated_version)) if curated_version == version => {
                let page = PageKey::from_doc_id(&placements.doc_ids[i as usize])
                    .context("a placement's doc_id")?;
                wanted.entry(b).or_default().insert(page, i);
            }
            _ => {}
        }
    }
    let mut found = HashSet::new();
    for (b, mut pages) in wanted {
        for path in &batches[b as usize].curated.parts {
            if pages.is_empty() {
                break;
            }
            let bytes = curated
                .get(path)
                .await?
                .with_context(|| format!("curated part `{path}` is missing"))?;
            crate::curated::read_part(bytes.into(), false, |row| {
                if let Some(i) = pages.remove(&row.key) {
                    found.insert(i);
                }
                Ok(())
            })
            .with_context(|| format!("reading curated part `{path}`"))?;
        }
    }
    Ok(found)
}

/// Join the newest finished attempt at `revision` in `pictures` to the
/// curated `batches` and `placements`. Every part is checked against
/// `provenance.json` first; one that differs stops the report.
pub async fn join(
    pictures: &dyn ObjectStore,
    revision: &str,
    curated: &dyn ObjectStore,
    batches: &[RunBatch],
    placements: &Placements,
) -> anyhow::Result<JoinReport> {
    let Some(p) = finished(pictures, revision).await? else {
        bail!("`{PREFIX}/{revision}` has no finished attempt");
    };
    if p.schema != SCHEMA_VERSION || p.revision != revision {
        bail!(
            "attempt {} at `{PREFIX}/{revision}` is schema {} of revision {}; this reads schema {SCHEMA_VERSION}",
            p.attempt,
            p.schema,
            p.revision
        );
    }
    let days = CuratedDays::load(curated, batches).await?;
    let on_pages = on_curated_pages(curated, batches, &days, placements).await?;
    let dir = attempt_dir(revision, p.attempt);
    let mut report = JoinReport {
        revision: p.revision.clone(),
        attempt: p.attempt,
        curated_batches: batches.len() as u64,
        curated_title_days: days.first.len() as u64,
        placements: placements.summary.clone(),
        placements_on_curated_pages: on_pages.len() as u64,
        total: Classes::default(),
        pages: 0,
        kinds: BTreeMap::new(),
        batches: BTreeMap::new(),
    };
    let mut all_pages: HashSet<u32> = HashSet::new();
    for (kind, summary) in &p.kinds {
        let mut join = KindJoin::default();
        let mut pages: HashSet<u32> = HashSet::new();
        for part in &summary.parts {
            verified_part(pictures, &dir, part, kind, |row| {
                let scan = ScanKey::of(&row);
                let placed = scan
                    .as_ref()
                    .and_then(|s| placements.by_scan.get(s).copied())
                    .filter(|i| on_pages.contains(i));
                let class = scan
                    .as_ref()
                    .map_or(Class::Unplaced, |s| days.class(s, placed.is_some()));
                if let (Class::Page, Some(i)) = (class, placed) {
                    pages.insert(i);
                    all_pages.insert(i);
                }
                join.classes.add(class);
                report.total.add(class);
                let batch = row.batch.unwrap_or(NO_BATCH);
                match report.batches.get_mut(batch) {
                    Some(c) => c.add(class),
                    None => {
                        let mut c = Classes::default();
                        c.add(class);
                        report.batches.insert(batch.to_owned(), c);
                    }
                }
            })
            .await?;
        }
        if join.classes.rows != summary.rows {
            bail!(
                "the `{kind}` parts hold {} rows; provenance.json records {}",
                join.classes.rows,
                summary.rows
            );
        }
        join.pages = pages.len() as u64;
        report.kinds.insert(kind.clone(), join);
    }
    if report.total.rows != p.rows {
        bail!(
            "the parts hold {} rows; provenance.json records {}",
            report.total.rows,
            p.rows
        );
    }
    report.pages = all_pages.len() as u64;
    tracing::info!(
        revision,
        attempt = p.attempt,
        rows = report.total.rows,
        page = report.total.page,
        pages = report.pages,
        "pictures joined"
    );
    Ok(report)
}

/// Create the report file at `path`, which must not exist yet. The report
/// goes to a file because the logs are JSON on stdout; it is created before
/// the join runs, so an existing file stops the command at once.
pub fn create_report(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating `{}`", path.display()))
}

/// Write `report` as JSON to `file`, which [`create_report`] made at `path`.
pub fn write_report(file: &mut File, path: &Path, report: &JoinReport) -> anyhow::Result<()> {
    let mut write = || -> anyhow::Result<()> {
        serde_json::to_writer_pretty(&mut *file, report)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    };
    write().with_context(|| format!("writing `{}`", path.display()))
}

/// [`join`] against every curated batch in `state`.
pub async fn report(
    state: &State,
    curated: &dyn ObjectStore,
    pictures: &dyn ObjectStore,
    revision: &str,
    placements: &Placements,
) -> anyhow::Result<JoinReport> {
    let batches: Vec<RunBatch> = state
        .batches(&[])
        .await?
        .into_iter()
        .filter_map(|(b, _)| {
            b.curated.map(|c| RunBatch {
                batch: b.batch,
                curated: c,
            })
        })
        .collect();
    join(pictures, revision, curated, &batches, placements).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pictures::{write_part, KindSummary, Picture, Provenance, Quality, PROVENANCE};
    use crate::state::Curated;
    use crate::worker::Counts;
    use usnm_store::LocalStore;

    const REVISION: &str = "e2e585824cdd28d52755c481dca69ed0bfa992a1";
    const TILE: &str = "https://tile.loc.gov/image-services/iiif/service:ndnp";

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn row(kind: &str, batch: &str, lccn: &str, day: NaiveDate, frame: &str) -> Picture {
        Picture {
            kind: kind.into(),
            lccn: Some(lccn.into()),
            date: Some(day),
            edition: Some(1),
            batch: Some(batch.into()),
            reel: Some("00000000001".into()),
            frame: Some(frame.into()),
            score: Some(0.9),
            bbox: Some([0.1, 0.2, 0.3, 0.4]),
            caption_words: vec![],
            predicted_type: None,
            page_url: None,
            crop_url: None,
        }
    }

    /// A placement line for a scan of `row`'s shape, with the extra fields a
    /// prefetch index has.
    fn line(batch: &str, lccn: &str, day: NaiveDate, frame: &str, doc_id: &str) -> String {
        let awardee = batch.split('_').next().unwrap();
        serde_json::json!({
            "doc_id": doc_id,
            "batch": batch,
            "iiif_base": format!(
                "{TILE}:{awardee}:batch_{batch}:data:{lccn}:00000000001:{}01:{frame}",
                day.format("%Y%m%d")
            ),
            "resolution": "full",
        })
        .to_string()
    }

    fn batch(name: &str, version: u16) -> RunBatch {
        RunBatch {
            batch: name.into(),
            curated: Curated {
                version,
                parts: vec![],
                counts: format!("curated/{name}/counts.json"),
                pages: 0,
                ok_pages: 0,
                lccns: vec![],
                source_sha256: String::new(),
                first: String::new(),
                last: String::new(),
            },
        }
    }

    async fn put_counts(store: &LocalStore, b: &RunBatch, days: &[(&str, NaiveDate)]) {
        let mut counts = Counts::new();
        for (lccn, d) in days {
            counts
                .entry((*lccn).to_owned())
                .or_default()
                .insert(day_number(*d), 4);
        }
        store
            .put(
                &b.curated.counts,
                serde_json::to_vec(&counts).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
    }

    /// A curated part of `b` holding `pages`, listed in `b`'s parts.
    async fn put_pages(store: &LocalStore, b: &mut RunBatch, pages: &[PageKey]) {
        let mut w = crate::curated::PartWriter::new();
        for key in pages {
            let at = chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap();
            let row = crate::curated::CuratedRow::from_ocr(
                key.clone(),
                "words on a page",
                &b.batch,
                b.curated.version,
                "loc",
                at,
            );
            assert!(w.push(row).unwrap().is_none());
        }
        let path = format!("curated/{}/part-{}.parquet", b.batch, b.curated.parts.len());
        store
            .put(&path, w.finish_part().unwrap().unwrap(), "x")
            .await
            .unwrap();
        b.curated.parts.push(path);
    }

    /// An attempt holding `rows`, one part per kind, with its provenance.
    async fn put_attempt(store: &LocalStore, rows: &[Picture]) -> Provenance {
        let dir = attempt_dir(REVISION, 1);
        let mut kinds = BTreeMap::new();
        for (_, kind) in crate::pictures::CONFIGS {
            let of_kind: Vec<Picture> = rows.iter().filter(|r| r.kind == kind).cloned().collect();
            if of_kind.is_empty() {
                continue;
            }
            let bytes = write_part(&of_kind).unwrap();
            let path = format!("{dir}/{kind}-0000.parquet");
            let part = PartRef {
                path: path.clone(),
                sha256: hex(&Sha256::digest(&bytes)),
                bytes: bytes.len() as u64,
                rows: of_kind.len() as u64,
            };
            store.put(&path, bytes, "x").await.unwrap();
            kinds.insert(
                kind.to_owned(),
                KindSummary {
                    config: kind.to_owned(),
                    rows: of_kind.len() as u64,
                    parts: vec![part],
                    quality: Quality::default(),
                },
            );
        }
        let p = Provenance {
            schema: SCHEMA_VERSION,
            source: "biglam/newspaper-navigator".into(),
            license: "cc0-1.0".into(),
            revision: REVISION.into(),
            attempt: 1,
            written_at: chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap(),
            ingest_version: "test".into(),
            commit: None,
            inputs: vec![],
            rows: rows.len() as u64,
            kinds,
            quality: Quality::default(),
        };
        put_provenance(store, &p).await;
        p
    }

    async fn put_provenance(store: &LocalStore, p: &Provenance) {
        store
            .put(
                &format!("{}/{PROVENANCE}", attempt_dir(REVISION, p.attempt)),
                serde_json::to_vec(p).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
    }

    fn placements(dir: &std::path::Path, files: &[&[String]]) -> Placements {
        let paths: Vec<PathBuf> = files
            .iter()
            .enumerate()
            .map(|(i, lines)| {
                let path = dir.join(format!("index-{i}.jsonl"));
                std::fs::write(&path, lines.join("\n")).unwrap();
                path
            })
            .collect();
        Placements::read(&paths).unwrap()
    }

    #[tokio::test]
    async fn classes_each_row_by_how_it_meets_the_curated_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let (a, b) = ("sn11111111", "sn22222222");
        let (d1, d2, d3) = (date(1880, 1, 2), date(1880, 1, 9), date(1880, 1, 16));
        let doc =
            |lccn: &str, d: NaiveDate, seq: u16| PageKey::new(lccn, d, 1, seq).unwrap().doc_id();
        let key =
            |lccn: &str, d: NaiveDate, ed: u16, seq: u16| PageKey::new(lccn, d, ed, seq).unwrap();

        // `ohi_alpha` is curated at version 1, `ohi_beta` at 2 (the dataset
        // has its version 1), `ohi_gamma` has `b` on d2. `alpha`'s parts
        // hold d1's pages 3 and 4 and d2's edition 2.
        let mut alpha = batch("ohi_alpha", 1);
        let beta = batch("ohi_beta", 2);
        let gamma = batch("ohi_gamma", 1);
        put_counts(&store, &alpha, &[(a, d1), (a, d2)]).await;
        put_counts(&store, &beta, &[(a, d3)]).await;
        put_counts(&store, &gamma, &[(b, d2)]).await;
        put_pages(&store, &mut alpha, &[key(a, d1, 1, 3), key(a, d1, 1, 4)]).await;
        put_pages(&store, &mut alpha, &[key(a, d2, 2, 1)]).await;
        let batches = [alpha, beta, gamma];

        let rows = vec![
            // Placed on a page of `alpha`, twice on one scan (once with the
            // LCCN in upper case) and once on another.
            row("photo", "ohi_alpha_ver01", a, d1, "0005"),
            row("photo", "ohi_alpha_ver01", "SN11111111", d1, "0005"),
            row("map", "ohi_alpha_ver01", a, d1, "0006"),
            // Same batch and day, not placed.
            row("photo", "ohi_alpha_ver01", a, d2, "0010"),
            // Placed on edition 1 of d2, which the batch's parts don't hold
            // (they hold edition 2): only on its batch's day.
            row("map", "ohi_alpha_ver01", a, d2, "0012"),
            // Its batch is curated at another version.
            row("photo", "ohi_beta_ver01", a, d3, "0001"),
            // Only another batch has the title-day: `alpha` is curated, but
            // has nothing of `b`.
            row("map", "ohi_alpha_ver01", b, d2, "0002"),
            // No curated batch has the title-day, or the title.
            row("map", "ohi_gamma_ver01", b, d3, "0003"),
            row("map", "ohi_delta_ver01", "sn33333333", d1, "0004"),
            // A placement whose batch is not curated at the row's version
            // does not make a page.
            row("photo", "ohi_beta_ver01", a, d3, "0002"),
            // No batch, and a batch with no version.
            Picture {
                batch: None,
                ..row("photo", "x", a, d1, "0007")
            },
            row("photo", "ohi_alpha", a, d1, "0008"),
        ];
        let p = put_attempt(&store, &rows).await;

        let places = placements(
            dir.path(),
            &[
                &[
                    line("ohi_alpha_ver01", a, d1, "0005", &doc(a, d1, 3)),
                    line("ohi_alpha_ver01", a, d1, "0006", &doc(a, d1, 4)),
                    // The same pair again, as a second resolution's index has it.
                    line("ohi_alpha_ver01", a, d1, "0005", &doc(a, d1, 3)),
                    line("ohi_beta_ver01", a, d3, "0002", &doc(a, d3, 1)),
                    line("ohi_alpha_ver01", a, d2, "0012", &doc(a, d2, 1)),
                    String::new(),
                ],
                &[
                    "not json".to_owned(),
                    r#"{"doc_id":"x","iiif_base":"y"}"#.to_owned(),
                    // The doc_id is of another day than the scan.
                    line("ohi_alpha_ver01", a, d2, "0011", &doc(a, d1, 9)),
                ],
            ],
        );
        assert_eq!(
            places.summary,
            PlacementSummary {
                files: 2,
                lines: 8,
                placed: 4,
                unreadable: 2,
                inconsistent: 1,
                conflicting: 0,
            }
        );

        let r = join(&store, REVISION, &store, &batches, &places)
            .await
            .unwrap();
        assert_eq!((r.revision.as_str(), r.attempt), (REVISION, p.attempt));
        assert_eq!((r.curated_batches, r.curated_title_days), (3, 4));
        assert_eq!(r.placements_on_curated_pages, 2);
        assert_eq!(
            r.total,
            Classes {
                rows: 12,
                page: 3,
                same_batch_day: 2,
                other_version_day: 2,
                other_batch_day: 1,
                not_curated: 2,
                unplaced: 2,
            }
        );
        assert_eq!(r.pages, 2);
        let photo = &r.kinds["photo"];
        assert_eq!(
            (photo.classes.rows, photo.classes.page, photo.pages),
            (7, 2, 1)
        );
        let map = &r.kinds["map"];
        assert_eq!((map.classes.rows, map.classes.page, map.pages), (5, 1, 1));
        assert_eq!(
            r.batches.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                NO_BATCH,
                "ohi_alpha",
                "ohi_alpha_ver01",
                "ohi_beta_ver01",
                "ohi_delta_ver01",
                "ohi_gamma_ver01"
            ]
        );
        assert_eq!(r.batches["ohi_beta_ver01"].other_version_day, 2);
        assert_eq!(r.batches["ohi_alpha_ver01"].same_batch_day, 2);
        assert_eq!(r.batches["ohi_alpha_ver01"].other_batch_day, 1);
        assert_eq!(r.batches["ohi_alpha"].unplaced, 1);

        // The report is written once, to a new file.
        let out = dir.path().join("join.json");
        write_report(&mut create_report(&out).unwrap(), &out, &r).unwrap();
        let read: JoinReport = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(read, r);
        assert!(create_report(&out).is_err());

        // Without placements, the placed rows are only on their batch's day.
        let none = Placements::read(&[]).unwrap();
        let r = join(&store, REVISION, &store, &batches, &none)
            .await
            .unwrap();
        assert_eq!(r.placements_on_curated_pages, 0);
        assert_eq!((r.total.page, r.total.same_batch_day, r.pages), (0, 5, 0));
    }

    #[test]
    fn leaves_out_a_scan_or_page_placed_twice() {
        let dir = tempfile::tempdir().unwrap();
        let (a, d) = ("sn11111111", date(1880, 1, 2));
        let doc = |seq: u16| PageKey::new(a, d, 1, seq).unwrap().doc_id();
        let places = placements(
            dir.path(),
            &[&[
                // One scan, two pages.
                line("ohi_alpha_ver01", a, d, "0001", &doc(1)),
                line("ohi_alpha_ver01", a, d, "0001", &doc(2)),
                // One page, two scans.
                line("ohi_alpha_ver01", a, d, "0003", &doc(3)),
                line("ohi_alpha_ver01", a, d, "0004", &doc(3)),
                // The same page from another batch's scan.
                line("ohi_alpha_ver01", a, d, "0005", &doc(5)),
                line("ohi_omega_ver01", a, d, "0005", &doc(5)),
                // Upper case on the scan and a different spelling of the
                // same page: one pair.
                line("ohi_alpha_ver01", "SN11111111", d, "0006", &doc(6)),
                line(
                    "ohi_alpha_ver01",
                    a,
                    d,
                    "0006",
                    &doc(6).replace("seq-6", "seq-06"),
                ),
                // Conflicting pairs again count once.
                line("ohi_alpha_ver01", a, d, "0001", &doc(1)),
                line("ohi_alpha_ver01", a, d, "0001", &doc(2)),
                line("ohi_alpha_ver01", a, d, "0004", &doc(3)),
            ]],
        );
        assert_eq!((places.summary.placed, places.summary.conflicting), (1, 6));
        assert_eq!(places.doc_ids, [doc(6)]);
    }

    #[tokio::test]
    async fn stops_when_a_part_is_not_what_the_provenance_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let none = Placements::read(&[]).unwrap();
        let d = date(1880, 1, 2);

        // No attempt.
        let err = join(&store, REVISION, &store, &[], &none)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no finished attempt"), "{err}");

        let mut p = put_attempt(
            &store,
            &[row("photo", "ohi_alpha_ver01", "sn11111111", d, "0001")],
        )
        .await;
        assert!(join(&store, REVISION, &store, &[], &none).await.is_ok());

        // Another schema.
        p.schema = SCHEMA_VERSION + 1;
        put_provenance(&store, &p).await;
        assert!(join(&store, REVISION, &store, &[], &none).await.is_err());
        p.schema = SCHEMA_VERSION;

        // A part changed after it was recorded.
        let part = p.kinds["photo"].parts[0].clone();
        let other =
            write_part(&[row("photo", "ohi_alpha_ver01", "sn11111111", d, "0002")]).unwrap();
        store.put(&part.path, other, "x").await.unwrap();
        put_provenance(&store, &p).await;
        let err = join(&store, REVISION, &store, &[], &none)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("is not the part"), "{err}");
        assert!(err.to_string().contains(&part.sha256), "{err}");

        // Rows of another kind, with the hash recorded.
        let bytes = write_part(&[row("map", "ohi_alpha_ver01", "sn11111111", d, "0001")]).unwrap();
        let kind = p.kinds.get_mut("photo").unwrap();
        kind.parts[0].sha256 = hex(&Sha256::digest(&bytes));
        kind.parts[0].bytes = bytes.len() as u64;
        store.put(&part.path, bytes, "x").await.unwrap();
        put_provenance(&store, &p).await;
        let err = join(&store, REVISION, &store, &[], &none)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("another kind"), "{err}");

        // A kind whose total is not its parts'.
        let bytes =
            write_part(&[row("photo", "ohi_alpha_ver01", "sn11111111", d, "0001")]).unwrap();
        let kind = p.kinds.get_mut("photo").unwrap();
        kind.parts[0].sha256 = hex(&Sha256::digest(&bytes));
        kind.parts[0].bytes = bytes.len() as u64;
        kind.rows = 2;
        store.put(&part.path, bytes, "x").await.unwrap();
        put_provenance(&store, &p).await;
        let err = join(&store, REVISION, &store, &[], &none)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("provenance.json records 2"),
            "{err}"
        );

        // A part outside the attempt, or only under it by its spelling.
        let kind = p.kinds.get_mut("photo").unwrap();
        kind.rows = 1;
        let dir = attempt_dir(REVISION, p.attempt);
        for outside in [
            "elsewhere/photo-0000.parquet".to_owned(),
            format!("{dir}/../2/photo-0000.parquet"),
            format!("{dir}/./photo-0000.parquet"),
            format!("{dir}//photo-0000.parquet"),
        ] {
            p.kinds.get_mut("photo").unwrap().parts[0].path = outside;
            put_provenance(&store, &p).await;
            let err = join(&store, REVISION, &store, &[], &none)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("outside the attempt"), "{err}");
        }
    }

    #[test]
    fn a_batch_directory_has_a_version() {
        assert_eq!(split_batch("ohi_ingstad_ver01"), Some(("ohi_ingstad", 1)));
        assert_eq!(split_batch("mdu_bode_ver03"), Some(("mdu_bode", 3)));
        assert_eq!(split_batch("a_ver_b_ver12"), Some(("a_ver_b", 12)));
        for bad in [
            "ohi_ingstad",
            "ohi_ingstad_ver",
            "_ver01",
            "ohi_ingstad_ver0x",
        ] {
            assert_eq!(split_batch(bad), None, "{bad}");
        }
    }
}
