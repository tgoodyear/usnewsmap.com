//! Penn State's Pennsylvania Newspaper Archive (#1592, 04 §4.12): batches
//! its Open ONI site serves that LoC's bulk OCR doesn't have, made into
//! retained archives that `curate` reads as it reads LoC's.
//!
//! [`fetch_batch`] copies a batch's BagIt tag files and requested XML payload
//! (ALTO and METS), each checked against the bag's MD5. With a scan root, JP2s
//! go directly to `keep/{batch-version}/` and a per-batch JSONL catalog records
//! their provenance; this avoids collisions between corrected batch versions
//! and duplicating images in the batch store. The bag
//! names page files by reel frame (`1024.jp2`), so each issue's METS gives the
//! pages' sequence numbers. Each page's `ocr.txt` comes from the site, and the
//! texts go into a `tar.zst` in the page-key layout `curate` reads
//! ([`crate::archive`]), recorded as the batch's retained archive
//! ([`crate::raw`]). With `USNM_RAW_URL` at the same store, `curate` reads
//! that archive and downloads nothing.
//!
//! For the site's `batch_pst_bonaDea_ver03` ([`local_name`]:
//! `psu_pst_bonadea_ver03`) the store holds:
//! - `psu_pst_bonadea_ver03/bag/{path in the bag}`: the tag and XML files;
//! - `psu_pst_bonadea_ver03/ocr/{page key}/ocr.txt`: the site's page texts;
//! - `psu_pst_bonadea_ver03/psu_pst_bonadea_ver03-{sha256 prefix}.tar.zst`
//!   and `psu_pst_bonadea_ver03/manifest.json` ([`raw::Manifest`]).
//!
//! With `--scans-root /tank/ocr/scans`, page images instead live under
//! `keep/psu_pst_bonadea_ver03/` and are listed in
//! `catalog/psu_pst_bonadea_ver03.jsonl`, separate from the LoC scan index.
//!
//! Stored files are kept, so a run that stops resumes where it stopped, and
//! a batch with a manifest is done.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use chrono::{NaiveDate, Utc};
use md5::Md5;
use quick_xml::events::{BytesStart, Event};
use quick_xml::XmlVersion;
use serde::Serialize;
use sha2::{Digest, Sha256};
use usnm_congress::fetch::Fetch;
use usnm_core::ids::PageKey;
pub use usnm_core::ids::{PSU_BATCH_PREFIX, PSU_SITE};
use usnm_store::{ObjectStore, StoreError};

use crate::raw;
use crate::source::{hex, split_version, ListedBatch};

/// Sent with every request to the site.
pub const USER_AGENT: &str = concat!(
    "usnewsmap-ingest/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/tgoodyear/usnewsmap.com/issues/1592)"
);

/// What [`fetch_batch`] fetches, and from where.
#[derive(Debug, Clone)]
pub struct Options {
    /// The site, without a trailing `/` ([`PSU_SITE`]).
    pub site: String,
    /// The payload files to copy, by extension (`jp2`, `xml`). Each issue's
    /// METS and `BATCH.xml` are copied whatever this says.
    pub exts: Vec<String>,
    /// Issues from this day on are left out, their files too.
    pub issued_before: NaiveDate,
    /// Optional pve-big scan archive root. JP2s go under `keep/psu_*` and
    /// receive a source-specific catalog; they are not duplicated in the
    /// batch store.
    pub scans_root: Option<PathBuf>,
}

/// An issue `BATCH.xml` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub lccn: String,
    pub date: NaiveDate,
    pub edition: u16,
    /// Its METS, as a path in the bag (`data/sn84035968/000002001/1859071801/1859071801.xml`).
    pub mets: String,
}

/// What [`fetch_batch`] did.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Fetched {
    /// Issues fetched, and issues left out by date.
    pub issues: usize,
    pub issues_later: usize,
    /// Pages in the archive.
    pub pages: usize,
    /// Pages whose METS lists no OCR file.
    pub pages_without_ocr: usize,
    /// Pages with OCR whose `ocr.txt` the site doesn't have.
    pub texts_missing: usize,
    /// Files fetched (bag files and texts) and their bytes.
    pub fetched: usize,
    pub fetched_bytes: u64,
    /// Files a previous run stored.
    pub stored_before: usize,
}

/// Our name for the site's batch: `psu_` and the site's name lowercased,
/// without `batch_` (`batch_pst_bonaDea_ver03` is `psu_pst_bonadea_ver03`).
pub fn local_name(site_batch: &str) -> anyhow::Result<String> {
    let name = format!(
        "{PSU_BATCH_PREFIX}{}",
        strip_batch(site_batch).to_ascii_lowercase()
    );
    split_version(&name)?;
    Ok(name)
}

/// A batch name without `batch_`, its version or case (`pst_bonadea`), to
/// compare the site's names with LoC's.
pub fn base_name(name: &str) -> String {
    let name = strip_batch(name);
    name.rsplit_once("_ver")
        .map_or(name, |(base, _)| base)
        .to_ascii_lowercase()
}

fn strip_batch(name: &str) -> &str {
    match name.get(..6) {
        Some(p) if p.eq_ignore_ascii_case("batch_") => &name[6..],
        _ => name,
    }
}

/// A BagIt manifest's `{md5}  {path}` lines, as (path, md5).
pub fn parse_manifest(text: &str) -> anyhow::Result<Vec<(String, String)>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (md5, path) = line
                .split_once(char::is_whitespace)
                .with_context(|| format!("manifest line `{line}`"))?;
            ensure!(
                md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit()),
                "manifest line `{line}`: not an MD5"
            );
            Ok((path.trim().to_owned(), md5.to_ascii_lowercase()))
        })
        .collect()
}

/// The issues `BATCH.xml` lists.
pub fn batch_issues(xml: &str) -> anyhow::Result<Vec<Issue>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut issues = Vec::new();
    let mut open: Option<(String, NaiveDate, u16)> = None;
    let mut text = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.local_name().as_ref() == "issue" => {
                let lccn = attr(&e, "lccn")?.context("an issue without its lccn")?;
                let date = attr(&e, "issueDate")?.context("an issue without its date")?;
                let date = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                    .with_context(|| format!("issue date `{date}`"))?;
                let edition = attr(&e, "editionOrder")?
                    .context("an issue without its edition")?
                    .trim()
                    .parse()
                    .context("edition")?;
                open = Some((lccn, date, edition));
                text.clear();
            }
            Event::Text(t) if open.is_some() => {
                text.push_str(&t.xml_content(XmlVersion::Implicit1_0));
            }
            Event::End(e) if e.local_name().as_ref() == "issue" => {
                let (lccn, date, edition) = open.take().context("unbalanced issue")?;
                let mets = format!("data/{}", text.trim().trim_start_matches("./"));
                issues.push(Issue {
                    lccn,
                    date,
                    edition,
                    mets,
                });
            }
            Event::Eof => return Ok(issues),
            _ => {}
        }
    }
}

/// A page from its METS: sequence number, OCR presence and the service JP2
/// and ALTO file names within the issue directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuePage {
    pub seq: u16,
    pub image_file: Option<String>,
    pub alto_file: Option<String>,
    pub has_ocr: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct PsuScanRecord {
    pub doc_id: String,
    pub batch: String,
    pub batch_version: u16,
    pub page_key: String,
    pub image_path: String,
    pub source_url: String,
    pub viewer_url: String,
    pub alto_store_path: Option<String>,
    pub ocr_store_path: Option<String>,
    pub bytes: u64,
    pub md5: String,
    pub sha256: String,
    pub fetched_at: chrono::DateTime<Utc>,
}

const PSU_SCAN_CATALOG_DIR: &str = "catalog";

fn scan_rel_path(batch_version: &str, key: &PageKey) -> String {
    format!(
        "keep/{batch_version}/{}/{}/{}/{}.jp2",
        key.lccn,
        key.date.format("%Y-%m-%d"),
        key.edition,
        key.seq
    )
}

pub fn issue_pages(xml: &str) -> anyhow::Result<Vec<IssuePage>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut seqs: HashMap<String, u16> = HashMap::new();
    let mut uses: HashMap<String, String> = HashMap::new();
    let mut locations: HashMap<String, String> = HashMap::new();
    let mut divs: Vec<(String, Vec<String>)> = Vec::new();
    let (mut dmd, mut page) = (None::<String>, None::<(String, Vec<String>)>);
    let mut file_id = None::<String>;
    let (mut in_extent, mut in_start) = (false, false);
    loop {
        let event = reader.read_event()?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "dmdSec" => dmd = attr(&e, "ID")?,
                "extent" => in_extent = attr(&e, "unit")?.as_deref() == Some("pages") && !empty,
                "start" => in_start = in_extent && !empty,
                "file" => {
                    if let (Some(id), Some(u)) = (attr(&e, "ID")?, attr(&e, "USE")?) {
                        uses.insert(id, u);
                    }
                    file_id = attr(&e, "ID")?;
                }
                "FLocat" => {
                    if let (Some(id), Some(href)) = (file_id.as_ref(), attr(&e, "href")?) {
                        locations.insert(
                            id.clone(),
                            href.strip_prefix("./").unwrap_or(&href).to_owned(),
                        );
                    }
                }
                "div" if attr(&e, "TYPE")?.as_deref() == Some("np:page") => {
                    let id = attr(&e, "DMDID")?.context("a page without its DMDID")?;
                    if empty {
                        divs.push((id, Vec::new()));
                    } else {
                        page = Some((id, Vec::new()));
                    }
                }
                "fptr" => {
                    if let (Some((_, files)), Some(f)) = (page.as_mut(), attr(&e, "FILEID")?) {
                        files.push(f);
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_start => {
                let id = dmd.clone().context("a page number outside a dmdSec")?;
                let seq = t.xml_content(XmlVersion::Implicit1_0).trim().parse()?;
                seqs.insert(id, seq);
            }
            Event::End(e) => match e.local_name().as_ref() {
                "dmdSec" => dmd = None,
                "file" => file_id = None,
                "extent" => in_extent = false,
                "start" => in_start = false,
                "div" => divs.extend(page.take()),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    let mut pages = divs
        .into_iter()
        .map(|(id, files)| {
            let seq = *seqs
                .get(&id)
                .with_context(|| format!("page `{id}` has no sequence number"))?;
            let image_file = files.iter().find_map(|f| {
                (uses.get(f).is_some_and(|u| u == "service"))
                    .then(|| locations.get(f).cloned())
                    .flatten()
            });
            let alto_file = files.iter().find_map(|f| {
                (uses.get(f).is_some_and(|u| u == "ocr"))
                    .then(|| locations.get(f).cloned())
                    .flatten()
            });
            Ok(IssuePage {
                seq,
                image_file,
                alto_file,
                has_ocr: files
                    .iter()
                    .any(|f| uses.get(f).is_some_and(|u| u == "ocr")),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    pages.sort_by_key(|page| page.seq);
    Ok(pages)
}

fn issue_file_path(mets: &str, href: &str) -> anyhow::Result<String> {
    let file = href.trim_start_matches("./");
    ensure!(
        !file.is_empty()
            && !file.contains('/')
            && file != "."
            && file != ".."
            && file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'),
        "unsafe METS file reference {href:?}"
    );
    Ok(format!("{}/{file}", dir_of(mets)))
}

fn scan_sidecar_path(scan: &Path) -> PathBuf {
    PathBuf::from(format!("{}.psu.json", scan.display()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
    let mut tmp =
        tempfile::NamedTempFile::new_in(parent).with_context(|| parent.display().to_string())?;
    tmp.write_all(bytes)?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    tmp.persist(path)
        .with_context(|| path.display().to_string())?;
    Ok(())
}

fn write_scan_catalog(root: &Path, batch: &str) -> anyhow::Result<()> {
    let (_, version) = split_version(batch)?;
    let dir = root.join("keep").join(batch);
    if !dir.exists() {
        return Ok(());
    }
    let mut stack = vec![dir];
    let mut records = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| dir.display().to_string())? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".jp2.psu.json"))
            {
                let bytes = std::fs::read(&path).with_context(|| path.display().to_string())?;
                let record =
                    serde_json::from_slice::<PsuScanRecord>(&bytes).with_context(|| {
                        format!("invalid Penn State scan catalog sidecar {}", path.display())
                    })?;
                if record.batch_version == version {
                    records.push(record);
                }
            }
        }
    }
    records.sort_by(|a, b| a.page_key.cmp(&b.page_key));
    let catalog_dir = root.join(PSU_SCAN_CATALOG_DIR);
    std::fs::create_dir_all(&catalog_dir).with_context(|| catalog_dir.display().to_string())?;
    let catalog = catalog_dir.join(format!("{batch}.jsonl"));
    let mut body = Vec::new();
    for record in records {
        serde_json::to_writer(&mut body, &record)?;
        body.push(b'\n');
    }
    write_atomic(&catalog, &body)
}

/// The value of `e`'s attribute named `name` (its local name: `href` for
/// `xlink:href`).
fn attr(e: &BytesStart<'_>, name: &str) -> anyhow::Result<Option<String>> {
    for a in e.attributes().with_checks(true) {
        let a = a?;
        if a.key.local_name().as_ref() == name {
            return Ok(Some(
                a.normalized_value(XmlVersion::Implicit1_0)?.into_owned(),
            ));
        }
    }
    Ok(None)
}

/// Fetch the site's batch `site_batch` into `store` (the module doc says
/// what goes where), or find it there done: its `enqueue --list` entry, with
/// the archive's location in the store at `to` (a directory or a Blob URL),
/// and what was done. The archive is built in `tmp`.
pub async fn fetch_batch(
    fetch: &dyn Fetch,
    store: &dyn ObjectStore,
    to: &str,
    opts: &Options,
    site_batch: &str,
    tmp: &Path,
) -> anyhow::Result<(ListedBatch, Fetched)> {
    let name = local_name(site_batch)?;
    let mut run = Run {
        fetch,
        store,
        site: &opts.site,
        bag: format!("{}/data/batches/{site_batch}", opts.site),
        name: name.clone(),
        scans_root: opts.scans_root.as_deref(),
        done: Fetched::default(),
    };
    if let Some(m) = raw::find(store, &name, None).await? {
        let xml = utf8(run.read("data/BATCH.xml", None).await?)?;
        let issues = batch_issues(&xml)?;
        let lccns = issues
            .iter()
            .filter(|i| i.date < opts.issued_before)
            .map(|i| i.lccn.clone())
            .collect();
        if run.scans_root.is_some() {
            run.materialize_stored_scans(&issues, opts.issued_before)
                .await?;
            write_scan_catalog(run.scans_root.unwrap(), &name)?;
        }
        tracing::info!(batch = %name, archive = %m.path, "already fetched");
        return Ok((listed(name, to, &m.path, m.sha256, lccns)?, run.done));
    }
    let tags = parse_manifest(&utf8(run.read("tagmanifest-md5.txt", None).await?)?)?;
    let mut manifest = None;
    for (path, md5) in &tags {
        if path == "manifest-md5.txt" {
            manifest = Some(run.read(path, Some(md5)).await?);
        } else {
            run.copy(path, md5).await?;
        }
    }
    let manifest = manifest.context("the tag manifest doesn't list manifest-md5.txt")?;
    let files = parse_manifest(&utf8(manifest)?)?;
    let md5s: HashMap<&str, &str> = files
        .iter()
        .map(|(p, m)| (p.as_str(), m.as_str()))
        .collect();
    let md5 = |path: &str| {
        md5s.get(path)
            .copied()
            .with_context(|| format!("the bag's manifest doesn't list {path}"))
    };
    let batch_xml = "data/BATCH.xml";
    let issues = batch_issues(&utf8(run.read(batch_xml, Some(md5(batch_xml)?)).await?)?)?;
    ensure!(
        issues.iter().any(|i| i.date < opts.issued_before),
        "{site_batch}: no issue is dated before {}",
        opts.issued_before
    );
    // Payload files by issue folder; the rest (reel METS, copies of
    // BATCH.xml) now.
    let issue_dirs: HashSet<&str> = issues.iter().map(|i| dir_of(&i.mets)).collect();
    let mut by_dir: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for (path, md5) in &files {
        let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
        if path == batch_xml || !opts.exts.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
            continue;
        }
        if issue_dirs.contains(dir_of(path)) {
            by_dir.entry(dir_of(path)).or_default().push((path, md5));
        } else {
            run.copy(path, md5).await?;
        }
    }
    let mut archive = Archive::new(tmp)?;
    let (mut lccns, mut seen) = (BTreeSet::new(), HashSet::new());
    for (i, issue) in issues.iter().enumerate() {
        if issue.date >= opts.issued_before {
            run.done.issues_later += 1;
            continue;
        }
        let mets = utf8(run.read(&issue.mets, Some(md5(&issue.mets)?)).await?)?;
        let pages = issue_pages(&mets).with_context(|| issue.mets.clone())?;
        let image_files: HashSet<String> = pages
            .iter()
            .filter_map(|page| page.image_file.as_ref())
            .map(|file| issue_file_path(&issue.mets, file))
            .collect::<anyhow::Result<_>>()?;
        for (path, md5) in by_dir.remove(dir_of(&issue.mets)).unwrap_or_default() {
            if path != issue.mets && !(run.scans_root.is_some() && image_files.contains(path)) {
                run.copy(path, md5).await?;
            }
        }
        for page in pages {
            let key = PageKey::new(&issue.lccn, issue.date, issue.edition, page.seq)
                .with_context(|| issue.mets.clone())?;
            if !seen.insert(key.clone()) {
                continue;
            }
            let text = if page.has_ocr {
                run.text(&key).await?
            } else {
                None
            };
            if run.scans_root.is_some() {
                let file = page.image_file.as_deref().with_context(|| {
                    format!("{}: page {} has no service JP2", issue.mets, page.seq)
                })?;
                let bag_path = issue_file_path(&issue.mets, file)?;
                let image_md5 = md5(bag_path.as_str())?;
                run.copy_scan(
                    issue,
                    &key,
                    &bag_path,
                    image_md5,
                    page.alto_file.as_deref(),
                    text.is_some(),
                )
                .await?;
            }
            if !page.has_ocr {
                run.done.pages_without_ocr += 1;
                continue;
            }
            match text {
                Some(text) => {
                    archive.add(&key, &text)?;
                    lccns.insert(key.lccn);
                    run.done.pages += 1;
                }
                None => {
                    tracing::warn!(batch = %name, page = %key, "the site has no ocr.txt for this page");
                    run.done.texts_missing += 1;
                }
            }
        }
        run.done.issues += 1;
        tracing::info!(batch = %name, issue = i + 1, of = issues.len(), pages = run.done.pages, "issue fetched");
    }
    ensure!(run.done.pages > 0, "{site_batch}: no page has text");
    let (file, sha256, bytes) = archive.finish()?;
    // Named by its content: a rerun that stopped before the manifest
    // finds its own archive there.
    let path = format!("{name}/{name}-{}.tar.zst", &sha256[..16]);
    upload(store, &path, file).await?;
    raw::record_once(
        store,
        &raw::Manifest {
            batch: name.clone(),
            path: path.clone(),
            source_url: format!("{}/", run.bag),
            bytes,
            sha256: sha256.clone(),
            fetched_at: Utc::now(),
            headers: Default::default(),
        },
    )
    .await?;
    if let Some(root) = run.scans_root {
        write_scan_catalog(root, &name)?;
    }
    Ok((listed(name, to, &path, sha256, lccns)?, run.done))
}

/// One batch being fetched.
struct Run<'a> {
    fetch: &'a dyn Fetch,
    store: &'a dyn ObjectStore,
    site: &'a str,
    /// The bag's URL, without a trailing `/`.
    bag: String,
    name: String,
    scans_root: Option<&'a Path>,
    done: Fetched,
}

impl Run<'_> {
    /// The bag file at `path`, from the store, or fetched, checked against
    /// `md5` and stored.
    async fn read(&mut self, path: &str, md5: Option<&str>) -> anyhow::Result<Vec<u8>> {
        let key = self.key(path)?;
        if let Some(body) = self.store.get(&key).await? {
            self.done.stored_before += 1;
            if let Some(want) = md5 {
                verify_bag_md5(path, &body, want)?;
            }
            return Ok(body);
        }
        let body = self.fetch_checked(path, md5).await?;
        self.store
            .put_new(&key, body.clone(), content_type(path))
            .await?;
        Ok(body)
    }

    /// Copy the bag file at `path` into the store, checked against `md5`,
    /// unless it is there.
    async fn copy(&mut self, path: &str, md5: &str) -> anyhow::Result<()> {
        let key = self.key(path)?;
        if self.store.exists(&key).await? {
            self.done.stored_before += 1;
            return Ok(());
        }
        let body = self.fetch_checked(path, Some(md5)).await?;
        self.store.put_new(&key, body, content_type(path)).await?;
        Ok(())
    }

    fn key(&self, path: &str) -> anyhow::Result<String> {
        usnm_store::validate_path(path).with_context(|| format!("bag file `{path}`"))?;
        Ok(format!("{}/bag/{path}", self.name))
    }

    async fn fetch_checked(&mut self, path: &str, md5: Option<&str>) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/{path}", self.bag);
        let body = self
            .fetch
            .get(&url)
            .await?
            .with_context(|| format!("{url}: not found"))?;
        if let Some(want) = md5 {
            verify_bag_md5(&url, &body, want)?;
        }
        self.done.fetched += 1;
        self.done.fetched_bytes += body.len() as u64;
        Ok(body)
    }

    /// A page's text, the site's `ocr.txt`: from the store, or fetched and
    /// stored. `None` when the site has none.
    async fn text(&mut self, key: &PageKey) -> anyhow::Result<Option<Vec<u8>>> {
        let path = format!("{}/ocr/{key}/ocr.txt", self.name);
        if let Some(text) = self.store.get(&path).await? {
            self.done.stored_before += 1;
            return Ok(Some(text));
        }
        let url = format!("{}/lccn/{key}/ocr.txt", self.site);
        let Some(text) = self.fetch.get(&url).await? else {
            return Ok(None);
        };
        self.done.fetched += 1;
        self.done.fetched_bytes += text.len() as u64;
        self.store
            .put_new(&path, text.clone(), "text/plain; charset=utf-8")
            .await?;
        Ok(Some(text))
    }

    async fn materialize_stored_scans(
        &mut self,
        issues: &[Issue],
        issued_before: NaiveDate,
    ) -> anyhow::Result<()> {
        let manifest = utf8(self.read("manifest-md5.txt", None).await?)?;
        let md5s: HashMap<String, String> = parse_manifest(&manifest)?.into_iter().collect();
        for issue in issues.iter().filter(|issue| issue.date < issued_before) {
            let mets_md5 = md5s
                .get(&issue.mets)
                .with_context(|| format!("the bag's manifest doesn't list {}", issue.mets))?;
            let mets = utf8(self.read(&issue.mets, Some(mets_md5)).await?)?;
            for page in issue_pages(&mets)? {
                let key = PageKey::new(&issue.lccn, issue.date, issue.edition, page.seq)?;
                let file = page.image_file.as_deref().with_context(|| {
                    format!("{}: page {} has no service JP2", issue.mets, page.seq)
                })?;
                let bag_path = issue_file_path(&issue.mets, file)?;
                let want = md5s
                    .get(&bag_path)
                    .with_context(|| format!("the bag's manifest doesn't list {bag_path}"))?
                    .clone();
                let ocr_path = format!("{}/ocr/{key}/ocr.txt", self.name);
                let text_exists = page.has_ocr && self.store.exists(&ocr_path).await?;
                self.copy_scan(
                    issue,
                    &key,
                    &bag_path,
                    &want,
                    page.alto_file.as_deref(),
                    text_exists,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn copy_scan(
        &mut self,
        issue: &Issue,
        key: &PageKey,
        bag_path: &str,
        want_md5: &str,
        alto_file: Option<&str>,
        has_text: bool,
    ) -> anyhow::Result<()> {
        let Some(root) = self.scans_root else {
            return Ok(());
        };
        let (batch, batch_version) = split_version(&self.name)?;
        let rel = scan_rel_path(&self.name, key);
        let path = root.join(&rel);
        let sidecar = scan_sidecar_path(&path);
        let store_key = self.key(bag_path)?;
        let bytes = if path.exists() {
            std::fs::read(&path).with_context(|| path.display().to_string())?
        } else if let Some(bytes) = self.store.get(&store_key).await? {
            self.done.stored_before += 1;
            bytes
        } else {
            self.fetch_checked(bag_path, Some(want_md5)).await?
        };
        let got_md5 = hex(&Md5::digest(&bytes));
        ensure!(
            got_md5 == want_md5,
            "{}: MD5 {got_md5}, but the bag lists {want_md5}",
            path.display()
        );
        let sha256 = hex(&Sha256::digest(&bytes));
        if !path.exists() {
            write_atomic(&path, &bytes)?;
        } else {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .with_context(|| path.display().to_string())?;
        }
        let image_path = rel;
        let source_url = format!("{}/{bag_path}", self.bag);
        let alto_store_path = alto_file
            .map(|file| issue_file_path(&issue.mets, file))
            .transpose()?
            .map(|file| format!("{}/bag/{file}", self.name));
        let alto_store_path = match alto_store_path {
            Some(path) if self.store.exists(&path).await? => Some(path),
            _ => None,
        };
        let record = PsuScanRecord {
            doc_id: key.doc_id(),
            batch,
            batch_version,
            page_key: key.to_string(),
            image_path,
            source_url,
            viewer_url: format!("{}/lccn/{key}/", self.site),
            alto_store_path,
            ocr_store_path: has_text.then(|| format!("{}/ocr/{key}/ocr.txt", self.name)),
            bytes: bytes.len() as u64,
            md5: got_md5,
            sha256,
            fetched_at: Utc::now(),
        };
        if sidecar.exists() {
            let old = std::fs::read(&sidecar).with_context(|| sidecar.display().to_string())?;
            let old: PsuScanRecord = serde_json::from_slice(&old)
                .with_context(|| format!("invalid sidecar {}", sidecar.display()))?;
            ensure!(
                old.batch == record.batch
                    && old.page_key == record.page_key
                    && old.md5 == record.md5
                    && old.sha256 == record.sha256,
                "{}: existing Penn State sidecar disagrees with the verified scan",
                sidecar.display()
            );
            if old.batch_version != record.batch_version
                || old.image_path != record.image_path
                || old.alto_store_path != record.alto_store_path
                || old.ocr_store_path != record.ocr_store_path
            {
                let mut refreshed = record;
                refreshed.fetched_at = old.fetched_at;
                write_atomic(&sidecar, &serde_json::to_vec(&refreshed)?)?;
            } else {
                std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o644))
                    .with_context(|| sidecar.display().to_string())?;
            }
        } else {
            write_atomic(&sidecar, &serde_json::to_vec(&record)?)?;
        }
        Ok(())
    }
}

fn utf8(bytes: Vec<u8>) -> anyhow::Result<String> {
    String::from_utf8(bytes).context("not UTF-8")
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn content_type(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jp2") => "image/jp2",
        Some("xml") => "application/xml",
        Some("txt") => "text/plain",
        Some("pdf") => "application/pdf",
        Some("tif" | "tiff") => "image/tiff",
        _ => "application/octet-stream",
    }
}

fn verify_bag_md5(path: &str, bytes: &[u8], want: &str) -> anyhow::Result<()> {
    let got = hex(&Md5::digest(bytes));
    ensure!(got == want, "{path}: MD5 {got}, but the bag lists {want}");
    Ok(())
}

/// The list entry for the archive at `path` in the store at `to`: its Blob
/// URL, or its absolute path in a directory.
fn listed(
    name: String,
    to: &str,
    path: &str,
    sha256: String,
    lccns: BTreeSet<String>,
) -> anyhow::Result<ListedBatch> {
    let url = if to.starts_with("https://") || to.starts_with("http://") {
        format!("{}/{path}", to.trim_end_matches('/'))
    } else {
        let root = std::fs::canonicalize(to).with_context(|| to.to_owned())?;
        root.join(path).to_string_lossy().into_owned()
    };
    Ok(ListedBatch {
        name,
        url,
        sha256: Some(sha256),
        ocr_source: None,
        lccns: lccns.into_iter().collect(),
    })
}

/// The page texts as a `tar.zst` in a temp file, the same bytes for the
/// same texts: entries in the order added, no times or owners.
struct Archive(tar::Builder<zstd::stream::write::Encoder<'static, Hashed>>);

/// A file that hashes what is written to it.
struct Hashed {
    file: std::fs::File,
    sha: Sha256,
    bytes: u64,
}

impl Write for Hashed {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.sha.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Archive {
    fn new(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
        let file = tempfile::tempfile_in(dir).with_context(|| dir.display().to_string())?;
        let hashed = Hashed {
            file,
            sha: Sha256::new(),
            bytes: 0,
        };
        Ok(Self(tar::Builder::new(zstd::stream::write::Encoder::new(
            hashed, 0,
        )?)))
    }

    fn add(&mut self, key: &PageKey, text: &[u8]) -> anyhow::Result<()> {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_size(text.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(0);
        self.0.append_data(&mut h, format!("{key}/ocr.txt"), text)?;
        Ok(())
    }

    /// The archive's file, read from its start, with its sha256 and size.
    fn finish(self) -> anyhow::Result<(std::fs::File, String, u64)> {
        let Hashed {
            mut file,
            sha,
            bytes,
        } = self.0.into_inner()?.finish()?;
        file.flush()?;
        file.seek(SeekFrom::Start(0))?;
        Ok((file, hex(&sha.finalize()), bytes))
    }
}

/// Store `file` at `path` in the retained archives' tier. One already
/// there has the same name, so the same content.
async fn upload(store: &dyn ObjectStore, path: &str, file: std::fs::File) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let (commit, committed) = tokio::sync::oneshot::channel();
    let send = async move {
        let mut file = tokio::fs::File::from_std(file);
        loop {
            let mut buf = vec![0; 4 << 20];
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            buf.truncate(n);
            if tx.send(bytes::Bytes::from(buf)).await.is_err() {
                // The upload stopped; it says why.
                break;
            }
        }
        drop(tx);
        let _ = commit.send(true);
        anyhow::Ok(())
    };
    let put = store.put_stream(path, "application/zstd", Some(raw::TIER), rx, committed);
    let (sent, put) = tokio::join!(send, put);
    match put {
        Ok(_) | Err(StoreError::AlreadyExists(_)) => sent,
        Err(e) => Err(e).with_context(|| format!("storing `{path}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const SITE: &str = "https://oni.example";
    const BAG: &str = "https://oni.example/data/batches/batch_pst_test_ver01";

    #[test]
    fn names_batches() {
        assert_eq!(
            local_name("batch_pst_bonaDea_ver03").unwrap(),
            "psu_pst_bonadea_ver03"
        );
        assert!(local_name("batch_pst_bonaDea").is_err());
        assert!(local_name("batch_../x_ver01").is_err());
        assert_eq!(base_name("batch_pst_bonaDea_ver03"), "pst_bonadea");
        assert_eq!(base_name("pst_bonadea_ver01"), "pst_bonadea");
    }

    #[test]
    fn reads_bagit_manifests() {
        let md5 = "0123456789abcdef0123456789abcdef";
        let text = format!(
            "{}  data/BATCH.xml\n\n{md5}  data/a b.xml\n",
            md5.to_uppercase()
        );
        assert_eq!(
            parse_manifest(&text).unwrap(),
            [
                ("data/BATCH.xml".to_owned(), md5.to_owned()),
                ("data/a b.xml".to_owned(), md5.to_owned())
            ]
        );
        assert!(parse_manifest("0123  data/BATCH.xml").is_err());
    }

    #[test]
    fn reads_page_numbers_from_the_mets_not_the_file_names() {
        // Frame 1025 is page 2, listed first and without OCR.
        let pages = issue_pages(&mets()).unwrap();
        assert_eq!(pages[0].seq, 1);
        assert!(pages[0].has_ocr);
        assert_eq!(pages[0].image_file.as_deref(), Some("1024.jp2"));
        assert_eq!(pages[0].alto_file.as_deref(), Some("1024.xml"));
        assert_eq!(pages[1].seq, 2);
        assert!(!pages[1].has_ocr);
        assert_eq!(pages[1].image_file.as_deref(), Some("1025.jp2"));
    }

    /// A site holding one bag: a reel, an issue from 1861 with two pages
    /// (one without OCR) and an issue from 1931.
    struct Site {
        files: HashMap<String, Vec<u8>>,
        asked: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Fetch for Site {
        async fn get(&self, url: &str) -> anyhow::Result<Option<Vec<u8>>> {
            self.asked.lock().unwrap().push(url.to_owned());
            Ok(self.files.get(url).cloned())
        }
    }

    fn mets() -> String {
        r#"<mets xmlns="http://www.loc.gov/METS/" xmlns:MODS="http://www.loc.gov/mods/v3" xmlns:xlink="http://www.w3.org/1999/xlink">
  <dmdSec ID="issueModsBib"><mdWrap><xmlData><MODS:mods><MODS:part><MODS:detail type="edition"><MODS:number>1</MODS:number></MODS:detail></MODS:part></MODS:mods></xmlData></mdWrap></dmdSec>
  <dmdSec ID="pageModsBib1"><mdWrap><xmlData><MODS:mods><MODS:part><MODS:extent unit="pages"><MODS:start>1</MODS:start></MODS:extent></MODS:part>
    <MODS:relatedItem type="original"><MODS:identifier type="reel sequence number">1024</MODS:identifier></MODS:relatedItem></MODS:mods></xmlData></mdWrap></dmdSec>
  <dmdSec ID="pageModsBib2"><mdWrap><xmlData><MODS:mods><MODS:part><MODS:extent unit="pages"><MODS:start>2</MODS:start></MODS:extent></MODS:part>
    <MODS:relatedItem type="original"><MODS:identifier type="reel sequence number">1025</MODS:identifier></MODS:relatedItem></MODS:mods></xmlData></mdWrap></dmdSec>
  <fileSec>
    <fileGrp ID="pageFileGrp1">
      <file ID="serviceFile1" USE="service"><FLocat LOCTYPE="OTHER" xlink:href="./1024.jp2"/></file>
      <file ID="otherDerivativeFile1" USE="derivative"><FLocat LOCTYPE="OTHER" xlink:href="./1024.pdf"/></file>
      <file ID="ocrFile1" USE="ocr"><FLocat LOCTYPE="OTHER" xlink:href="./1024.xml"/></file>
    </fileGrp>
    <fileGrp ID="pageFileGrp2">
      <file ID="serviceFile2" USE="service"><FLocat LOCTYPE="OTHER" xlink:href="./1025.jp2"/></file>
    </fileGrp>
  </fileSec>
  <structMap xmlns:np="urn:library-of-congress:ndnp:mets:newspaper">
    <div TYPE="np:issue" DMDID="issueModsBib">
      <div TYPE="np:page" DMDID="pageModsBib2"><fptr FILEID="serviceFile2"/></div>
      <div TYPE="np:page" DMDID="pageModsBib1"><fptr FILEID="serviceFile1"/><fptr FILEID="otherDerivativeFile1"/><fptr FILEID="ocrFile1"/></div>
    </div>
  </structMap>
</mets>"#
            .to_owned()
    }

    fn site() -> Site {
        let batch = r#"<?xml version="1.0" encoding="UTF-8"?>
<batch xmlns="http://www.loc.gov/ndnp" name="batch_pst_test_ver01" awardee="pst" awardYear="2017">
  <reel reelNumber="000002003">./sn84035968/000002003/000002003.xml</reel>
  <issue lccn="sn84035968" issueDate="1861-10-08" editionOrder="1">./sn84035968/000002003/1861100801/1861100801.xml</issue>
  <issue lccn="sn84035968" issueDate="1931-01-02" editionOrder="1">./sn84035968/000002003/1931010201/1931010201.xml</issue>
</batch>"#;
        let issue = "data/sn84035968/000002003/1861100801";
        let later = "data/sn84035968/000002003/1931010201";
        let payload: Vec<(String, Vec<u8>)> = vec![
            ("data/BATCH.xml".into(), batch.into()),
            ("data/BATCH_1.xml".into(), batch.into()),
            (
                "data/sn84035968/000002003/000002003.xml".into(),
                b"<reel/>".to_vec(),
            ),
            (format!("{issue}/1861100801.xml"), mets().into_bytes()),
            (format!("{issue}/1024.jp2"), b"page one image".to_vec()),
            (format!("{issue}/1024.xml"), b"<alto/>".to_vec()),
            (format!("{issue}/1024.pdf"), b"%PDF".to_vec()),
            (format!("{issue}/1025.jp2"), b"page two image".to_vec()),
            (format!("{later}/1931010201.xml"), mets().into_bytes()),
            (format!("{later}/2001.jp2"), b"a later page".to_vec()),
        ];
        let lines = |files: &[(String, Vec<u8>)]| {
            files
                .iter()
                .map(|(p, b)| format!("{}  {p}\n", hex(&Md5::digest(b))))
                .collect::<String>()
        };
        let manifest = lines(&payload);
        let tags = vec![
            ("bagit.txt".to_owned(), b"BagIt-Version: 0.97\n".to_vec()),
            ("manifest-md5.txt".to_owned(), manifest.into_bytes()),
        ];
        let mut files: HashMap<String, Vec<u8>> = payload
            .iter()
            .chain(&tags)
            .map(|(p, b)| (format!("{BAG}/{p}"), b.clone()))
            .collect();
        files.insert(
            format!("{BAG}/tagmanifest-md5.txt"),
            lines(&tags).into_bytes(),
        );
        files.insert(
            format!("{SITE}/lccn/sn84035968/1861-10-08/ed-1/seq-1/ocr.txt"),
            b"HARRISBURG, TUESDAY MORNING".to_vec(),
        );
        Site {
            files,
            asked: Mutex::default(),
        }
    }

    fn opts() -> Options {
        Options {
            site: SITE.into(),
            exts: vec!["jp2".into(), "xml".into()],
            issued_before: NaiveDate::from_ymd_opt(1931, 1, 1).unwrap(),
            scans_root: None,
        }
    }

    #[tokio::test]
    async fn catalogs_verified_images_in_the_pve_scan_tree() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("store");
        let to = to.to_str().unwrap();
        let store = usnm_store::open(to).unwrap();
        let site = site();
        let tmp = dir.path().join("tmp");
        fetch_batch(
            &site,
            store.as_ref(),
            to,
            &opts(),
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap();
        let asked_before = site.asked.lock().unwrap().len();

        let scans_root = dir.path().join("scans");
        let mut scan_opts = opts();
        scan_opts.scans_root = Some(scans_root.clone());
        fetch_batch(
            &site,
            store.as_ref(),
            to,
            &scan_opts,
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap();

        assert_eq!(site.asked.lock().unwrap().len(), asked_before);
        let image = scans_root.join("keep/psu_pst_test_ver01/sn84035968/1861-10-08/1/1.jp2");
        assert_ne!(
            scan_rel_path(
                "psu_pst_test_ver01",
                &PageKey::new(
                    "sn84035968",
                    NaiveDate::from_ymd_opt(1861, 10, 8).unwrap(),
                    1,
                    1
                )
                .unwrap()
            ),
            scan_rel_path(
                "psu_pst_test_ver02",
                &PageKey::new(
                    "sn84035968",
                    NaiveDate::from_ymd_opt(1861, 10, 8).unwrap(),
                    1,
                    1
                )
                .unwrap()
            )
        );
        assert_eq!(std::fs::read(&image).unwrap(), b"page one image");
        assert_eq!(
            std::fs::metadata(&image).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            std::fs::metadata(format!("{}.psu.json", image.display()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert_eq!(
            std::fs::read(scans_root.join("keep/psu_pst_test_ver01/sn84035968/1861-10-08/1/2.jp2"))
                .unwrap(),
            b"page two image"
        );
        let catalog =
            std::fs::read_to_string(scans_root.join("catalog/psu_pst_test_ver01.jsonl")).unwrap();
        let records = catalog
            .lines()
            .map(|line| serde_json::from_str::<PsuScanRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].page_key, "sn84035968/1861-10-08/ed-1/seq-1");
        assert_eq!(
            records[0].alto_store_path.as_deref(),
            Some("psu_pst_test_ver01/bag/data/sn84035968/000002003/1861100801/1024.xml")
        );
        assert!(records[1].ocr_store_path.is_none());

        let sidecar = scan_sidecar_path(&image);
        let mut stale = records[0].clone();
        stale.image_path = "keep/psu_pst_test/sn84035968/1861-10-08/1/1.jp2".into();
        stale.alto_store_path = None;
        stale.ocr_store_path = None;
        write_atomic(&sidecar, &serde_json::to_vec(&stale).unwrap()).unwrap();
        fetch_batch(
            &site,
            store.as_ref(),
            to,
            &scan_opts,
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap();
        let refreshed: PsuScanRecord =
            serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(refreshed.image_path, records[0].image_path);
        assert_eq!(refreshed.alto_store_path, records[0].alto_store_path);
        assert_eq!(refreshed.ocr_store_path, records[0].ocr_store_path);
        assert_eq!(refreshed.fetched_at, records[0].fetched_at);
        assert_eq!(
            std::fs::metadata(scans_root.join("catalog/psu_pst_test_ver01.jsonl"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );

        let mut v2 = records[0].clone();
        v2.batch_version = 2;
        v2.page_key = "sn84035968/1861-10-08/ed-1/seq-3".into();
        v2.image_path = "keep/psu_pst_test_ver02/sn84035968/1861-10-08/1/3.jp2".into();
        let v2_sidecar = scans_root.join(format!("{}.psu.json", v2.image_path));
        write_atomic(&v2_sidecar, &serde_json::to_vec(&v2).unwrap()).unwrap();
        write_scan_catalog(&scans_root, "psu_pst_test_ver01").unwrap();
        let v1_catalog =
            std::fs::read_to_string(scans_root.join("catalog/psu_pst_test_ver01.jsonl")).unwrap();
        assert_eq!(v1_catalog.lines().count(), 2);
        write_scan_catalog(&scans_root, "psu_pst_test_ver02").unwrap();
        let v2_catalog =
            std::fs::read_to_string(scans_root.join("catalog/psu_pst_test_ver02.jsonl")).unwrap();
        let v2_records = v2_catalog
            .lines()
            .map(|line| serde_json::from_str::<PsuScanRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(v2_records.len(), 1);
        assert_eq!(v2_records[0].batch_version, 2);
    }

    #[tokio::test]
    async fn omits_alto_from_the_catalog_when_it_was_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("store");
        let to = to.to_str().unwrap();
        let store = usnm_store::open(to).unwrap();
        let site = site();
        let tmp = dir.path().join("tmp");
        let scans_root = dir.path().join("scans");
        let mut options = opts();
        options.exts = vec!["jp2".into()];
        options.scans_root = Some(scans_root.clone());
        fetch_batch(
            &site,
            store.as_ref(),
            to,
            &options,
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap();
        let catalog =
            std::fs::read_to_string(scans_root.join("catalog/psu_pst_test_ver01.jsonl")).unwrap();
        let records = catalog
            .lines()
            .map(|line| serde_json::from_str::<PsuScanRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(records
            .iter()
            .all(|record| record.alto_store_path.is_none()));
    }

    #[tokio::test]
    async fn verifies_mets_before_materializing_a_stored_batch() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("store");
        let to = to.to_str().unwrap();
        let store = usnm_store::open(to).unwrap();
        let site = site();
        let tmp = dir.path().join("tmp");
        fetch_batch(
            &site,
            store.as_ref(),
            to,
            &opts(),
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap();

        let mets = dir.path().join(
            "store/psu_pst_test_ver01/bag/data/sn84035968/000002003/1861100801/1861100801.xml",
        );
        std::fs::write(&mets, b"<different mets/>").unwrap();
        let mut options = opts();
        options.scans_root = Some(dir.path().join("scans"));
        let error = fetch_batch(
            &site,
            store.as_ref(),
            to,
            &options,
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("MD5"));
    }

    #[tokio::test]
    async fn fetches_a_bag_into_an_archive_curate_reads_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("store");
        let to = to.to_str().unwrap();
        let store = usnm_store::open(to).unwrap();
        let site = site();
        let tmp = dir.path().join("tmp");
        let (listed, done) = fetch_batch(
            &site,
            store.as_ref(),
            to,
            &opts(),
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap();
        assert_eq!(listed.name, "psu_pst_test_ver01");
        assert_eq!(listed.lccns, ["sn84035968"]);
        assert_eq!(
            (
                done.issues,
                done.issues_later,
                done.pages,
                done.pages_without_ocr
            ),
            (1, 1, 1, 1)
        );
        let bag = "psu_pst_test_ver01/bag";
        for kept in [
            "bagit.txt",
            "tagmanifest-md5.txt",
            "data/BATCH_1.xml",
            "data/sn84035968/000002003/000002003.xml",
            "data/sn84035968/000002003/1861100801/1024.jp2",
            "data/sn84035968/000002003/1861100801/1024.xml",
            "data/sn84035968/000002003/1861100801/1025.jp2",
        ] {
            assert!(
                store.exists(&format!("{bag}/{kept}")).await.unwrap(),
                "{kept}"
            );
        }
        for left in [
            "data/sn84035968/000002003/1861100801/1024.pdf",
            "data/sn84035968/000002003/1931010201/2001.jp2",
            "data/sn84035968/000002003/1931010201/1931010201.xml",
        ] {
            assert!(
                !store.exists(&format!("{bag}/{left}")).await.unwrap(),
                "{left}"
            );
        }
        // The list names the recorded archive, which reads as LoC's do.
        let m = raw::find(
            store.as_ref(),
            "psu_pst_test_ver01",
            listed.sha256.as_deref(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            listed.url,
            std::fs::canonicalize(to)
                .unwrap()
                .join(&m.path)
                .to_string_lossy()
        );
        let mut pages = Vec::new();
        crate::archive::read_pages(Path::new(&listed.url), |p| {
            pages.push((p.key.to_string(), p.text));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            pages,
            [(
                "sn84035968/1861-10-08/ed-1/seq-1".to_owned(),
                "HARRISBURG, TUESDAY MORNING".to_owned()
            )]
        );
        // A second run asks the site for nothing and lists the same.
        site.asked.lock().unwrap().clear();
        let (again, _) = fetch_batch(
            &site,
            store.as_ref(),
            to,
            &opts(),
            "batch_pst_test_ver01",
            &tmp,
        )
        .await
        .unwrap();
        assert!(site.asked.lock().unwrap().is_empty());
        assert_eq!(
            (again.url, again.sha256, again.lccns),
            (listed.url, listed.sha256, listed.lccns)
        );
    }

    #[tokio::test]
    async fn keeps_no_file_that_fails_its_md5() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().to_str().unwrap();
        let store = usnm_store::open(to).unwrap();
        let mut site = site();
        let jp2 = format!("{BAG}/data/sn84035968/000002003/1861100801/1024.jp2");
        site.files.insert(jp2, b"cut sho".to_vec());
        let e = fetch_batch(
            &site,
            store.as_ref(),
            to,
            &opts(),
            "batch_pst_test_ver01",
            &dir.path().join("tmp"),
        )
        .await
        .unwrap_err();
        assert!(format!("{e:#}").contains("MD5"), "{e:#}");
        let path = "psu_pst_test_ver01/bag/data/sn84035968/000002003/1861100801/1024.jp2";
        assert!(!store.exists(path).await.unwrap());
        assert!(raw::find(store.as_ref(), "psu_pst_test_ver01", None)
            .await
            .unwrap()
            .is_none());
    }
}
