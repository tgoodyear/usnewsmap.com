//! Trusted-friends batch transport. BOINC handles volunteer scheduling;
//! this bridge reserves native work and publishes through ocr-out/ocr-sync.
use anyhow::{bail, ensure, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use usnm_core::ids::PageKey;
use usnm_ingest::ocr_local::{
    self, checks,
    ledger::{self, Queued},
};
use usnm_ocr_contract::{
    boinc::{self, Job, ResultManifest},
    message::{OcrMessage, Reason},
    prefetch::ManifestRow,
};
use usnm_ocr_worker::{
    engine,
    loc::{ImageSize, ScanFormat},
    prefetch,
};
use usnm_state::state::State;
use usnm_store::ObjectStore;

pub mod assess;
pub mod bundle;
pub mod claim;
pub mod cli;
pub mod gold;
pub mod handoff;
pub mod host_trust;
pub mod ndlocr;
pub mod quality;
pub mod qwen;
pub mod scans;
pub mod submit;
pub mod surya;
pub mod trust;
pub mod worker;

/// Bounded regular-file reads, also used for untrusted volunteer output.
pub fn read(path: &Path, max: usize) -> anyhow::Result<Vec<u8>> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "not a regular file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let f = options.open(path)?;
    ensure!(
        f.metadata()?.is_file() && f.metadata()?.len() <= max as u64,
        "file over size limit"
    );
    let mut bytes = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "file over size limit");
    Ok(bytes)
}
pub fn atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    atomic_with_permissions(path, bytes, None)
}

/// Public BOINC inputs must be readable by the web-server identity before
/// their final names become visible. Journals use the private `atomic` path.
pub(crate) fn atomic_download(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("no parent directory")?;
    public_dirs(parent)?;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o644))
    };
    // The BOINC server is Linux-only, so `None` is fine on other platforms.
    #[cfg(not(unix))]
    let permissions = None;
    atomic_with_permissions(path, bytes, permissions)
}

/// Creates missing directories as exactly 0755 whatever the umask, so the
/// web-server identity can traverse new download fan-out directories. Existing
/// directories, including ones made by a concurrent caller, gain any missing
/// 0755 bits; other bits (setgid, group write) stay.
pub(crate) fn public_dirs(dir: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let traversable = |d: &Path| -> anyhow::Result<()> {
            let mode = std::fs::metadata(d)?.permissions().mode() & 0o7777;
            if mode & 0o755 != 0o755 {
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(mode | 0o755))?;
            }
            Ok(())
        };
        let mut missing = Vec::new();
        let mut cur = Some(dir);
        while let Some(d) = cur.filter(|d| !d.as_os_str().is_empty() && !d.is_dir()) {
            missing.push(d);
            cur = d.parent();
        }
        let ancestor = cur.filter(|_| !missing.is_empty());
        for d in missing.into_iter().rev() {
            match std::fs::create_dir(d) {
                Ok(()) => {
                    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755))?;
                }
                // Another submitter made it, perhaps under its own umask.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && d.is_dir() => {
                    traversable(d)?;
                }
                Err(e) => return Err(e).with_context(|| format!("creating {}", d.display())),
            }
        }
        // A peer submitter may have just made the first existing ancestor under its
        // umask. Repair it only if it has our new directory's owner, so a directory
        // that belongs to someone else (the project root, say) is left alone.
        if let Some(a) = ancestor {
            use std::os::unix::fs::MetadataExt;
            if std::fs::metadata(a)?.uid() == std::fs::metadata(dir)?.uid() {
                traversable(a)?;
            }
        }
        traversable(dir)?;
    }
    // Other platforms have no mode bits; plain creation is all there is to do.
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
}

pub(crate) fn atomic_with_permissions(
    path: &Path,
    bytes: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> anyhow::Result<()> {
    let parent = path.parent().context("no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    f.write_all(bytes)?;
    if let Some(permissions) = permissions {
        f.as_file().set_permissions(permissions)?;
    }
    f.as_file().sync_all()?;
    f.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Serializes local journal mutations. Kernel lock disappears after a crash.
pub struct Lock {
    _file: File,
}
impl Lock {
    pub fn take(root: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(root)?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".lock"))?;
        rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .context("another BOINC bridge process owns this state directory")?;
        Ok(Self { _file: f })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        // A concurrent fork can retain this open file description until exec.
        // Closing only our descriptor would leave its lock held in that child.
        let _ = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::Unlock);
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn dropping_owner_unlocks_even_with_an_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Lock::take(dir.path()).unwrap();
        // dup and fork share the same open file description and flock lifetime.
        let inherited = owner._file.try_clone().unwrap();
        assert!(Lock::take(dir.path()).is_err());
        drop(owner);
        let next = Lock::take(dir.path()).unwrap();
        drop(inherited);
        assert!(Lock::take(dir.path()).is_err());
        drop(next);
        assert!(Lock::take(dir.path()).is_ok());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Prepared,
    Reserved,
    Submitting,
    Submitted,
    Published,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub job: Job,
    pub plan: String,
    pub stage: Stage,
    /// Fixed once, so interrupted publishing uses the same native object paths.
    pub stamp: String,
    pub error: Option<String>,
    /// For a job that came off the work queues (`claim`): the ledger row the
    /// queue message was written with, which the result is imported against.
    /// An explicit cohort has none, and `queued` makes its row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<LedgerRow>,
    /// The OCR bundle (#1059) this page was submitted in, on the BOINC server's copy of the
    /// record only: its workunit is the bundle's, `usnm_ocrb_<bundle>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    /// The bundle this page failed in and was taken out of, to be submitted alone. Such a page
    /// never goes into another bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unbundled: Option<String>,
}
/// A native ledger row as a claim carries it (`ledger::Queued` with its
/// times as RFC 3339 text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRow {
    pub batch_version: u16,
    pub reason: String,
    pub language: String,
    pub plan: String,
    pub attempt: u32,
    pub request_day: Option<String>,
    pub queued_at: String,
}
impl LedgerRow {
    pub fn from_queued(q: &Queued) -> Self {
        Self {
            batch_version: q.batch_version,
            reason: q.reason.clone(),
            language: q.language.clone(),
            plan: q.plan.clone(),
            attempt: q.attempt,
            request_day: q.request_day.map(|d| d.to_string()),
            queued_at: q
                .queued_at
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        }
    }
    pub fn to_queued(&self, doc_id: &str, batch: &str) -> anyhow::Result<Queued> {
        Ok(Queued {
            doc_id: doc_id.into(),
            batch: batch.into(),
            batch_version: self.batch_version,
            reason: self.reason.clone(),
            language: self.language.clone(),
            plan: self.plan.clone(),
            attempt: self.attempt,
            request_day: self.request_day.as_deref().map(str::parse).transpose()?,
            queued_at: DateTime::parse_from_rfc3339(&self.queued_at)?.with_timezone(&Utc),
        })
    }
}
impl Record {
    pub fn queued(&self) -> anyhow::Result<Queued> {
        let key = self.job.message.key()?;
        if let Some(row) = &self.ledger {
            return row.to_queued(&key.doc_id(), &self.job.message.batch);
        }
        Ok(Queued {
            doc_id: key.doc_id(),
            batch: self.job.message.batch.clone(),
            batch_version: self.job.batch_version,
            reason: self.job.message.reason.as_str().into(),
            language: self.job.message.language.clone(),
            plan: self.plan.clone(),
            attempt: 1,
            request_day: None,
            queued_at: DateTime::parse_from_rfc3339(&self.job.message.queued_at)?
                .with_timezone(&Utc),
        })
    }
    /// The BOINC workunit this page runs in: its own, or its bundle's.
    pub fn workunit(&self) -> String {
        match &self.bundle {
            Some(bundle) => usnm_ocr_contract::boinc::bundle::workunit(bundle),
            None => self.job.workunit(),
        }
    }
    pub fn paths(&self) -> anyhow::Result<(String, String)> {
        let key = self.job.message.key()?;
        // Short IDs keep native filenames compact. put_same refuses differing
        // bytes at an existing path, so a collision fails rather than overwrites.
        Ok((
            format!(
                "alto/{}.boinc-{}.{}.xml",
                key.doc_id(),
                &self.job.id[..12],
                self.stamp
            ),
            format!(
                "pages/{}_{}_ed-{}.boinc-{}.{}.parquet",
                key.lccn,
                key.date,
                key.edition,
                &self.job.id[..12],
                self.stamp
            ),
        ))
    }
}
fn dir(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    Ok(root.join("jobs").join(id))
}
pub fn load(root: &Path, id: &str) -> anyhow::Result<Record> {
    let record: Record = serde_json::from_slice(&read(
        &dir(root, id)?.join("record.json"),
        boinc::MAX_JSON * 2,
    )?)?;
    record.job.validate()?;
    ensure!(record.job.id == id, "journal identity mismatch");
    Ok(record)
}
pub fn save(root: &Path, record: &Record) -> anyhow::Result<()> {
    atomic(
        &dir(root, &record.job.id)?.join("record.json"),
        &serde_json::to_vec_pretty(record)?,
    )
}
pub fn records(root: &Path) -> anyhow::Result<Vec<Record>> {
    let path = root.join("jobs");
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for e in std::fs::read_dir(path)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            let id = e
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid journal directory"))?;
            out.push(load(root, &id)?);
        }
    }
    out.sort_by(|a, b| a.job.id.cmp(&b.job.id));
    Ok(out)
}

/// Find a full-resolution cached page in either set. Never fetches from LoC.
fn cached(scans: &Path, row: &ManifestRow) -> anyhow::Result<(Vec<u8>, prefetch::Sidecar)> {
    for set in ["keep", "buffer"] {
        for format in [ScanFormat::Jp2, ScanFormat::Jpeg] {
            let path = prefetch::page_path(
                &scans.join(set),
                &row.batch,
                &row.lccn,
                row.date,
                row.edition,
                row.seq,
                ImageSize::Full,
                format,
            );
            if !path.exists() {
                continue;
            }
            let bytes = read(&path, boinc::MAX_SCAN)?;
            let side: prefetch::Sidecar =
                serde_json::from_slice(&read(&prefetch::sidecar_path(&path), 64 * 1024)?)?;
            ensure!(
                side.doc_id == row.doc_id
                    && side.batch == row.batch
                    && side.batch_version == row.batch_version
                    && side.resolution == "full"
                    && side.format == format.name()
                    && prefetch::matches_sidecar(&bytes, &side),
                "cached scan does not match its manifest/sidecar"
            );
            return Ok((bytes, side));
        }
    }
    bail!("selected page has no full-resolution cached scan")
}
/// The rows `export` builds queue-fed jobs from: one per selected page, from its claim file,
/// with the claims by doc id. The scan is then found and checked against its sidecar (doc id,
/// batch, batch version, checksum) as a manifest page's is.
fn claimed_rows(
    dir: &Path,
    batch: &str,
    selected: &BTreeSet<String>,
) -> anyhow::Result<(Vec<ManifestRow>, BTreeMap<String, claim::Claim>)> {
    let mut claims = BTreeMap::new();
    let rows = selected
        .iter()
        .map(|page| {
            let key: PageKey = page.parse()?;
            let claim = claim::read_local(dir, &key.doc_id())?;
            let message = claim.message()?;
            ensure!(
                message.page == *page
                    && message.batch == batch
                    && message.language == claim.ledger.language,
                "the claim for {} is not this page and batch's queue message and ledger row",
                key.doc_id()
            );
            let row = ManifestRow::new(
                &key,
                &message.batch,
                claim.ledger.batch_version,
                Some(message.language.clone()),
                "claim",
                message.score.unwrap_or(0.0),
            );
            claims.insert(key.doc_id(), claim);
            Ok(row)
        })
        .collect::<anyhow::Result<_>>()?;
    Ok((rows, claims))
}
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub manifest: PathBuf,
    pub pages: PathBuf,
    pub batch: String,
    pub scans: PathBuf,
    pub root: PathBuf,
    pub plan: String,
    pub image: String,
    pub timeout_secs: u32,
    /// A directory of `claim` files, one per selected page
    /// (`<doc_id>.json`): the pages came off the work queues, so each job
    /// carries its queue message and ledger row instead of a new one.
    pub claims: Option<PathBuf>,
}
/// The Tesseract models the worker image holds, from the model lock.
pub fn installed_models() -> BTreeSet<String> {
    engine::MODELS_LOCK
        .lines()
        .filter(|s| !s.starts_with('#'))
        .filter_map(|s| s.split_whitespace().next())
        .filter_map(|s| s.strip_prefix("tessdata/")?.strip_suffix(".traineddata"))
        .map(str::to_owned)
        .collect()
}
/// Export a small, explicitly allocated cohort and register native requests.
pub async fn export(
    opts: &ExportOptions,
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
) -> anyhow::Result<Vec<String>> {
    ensure!(
        opts.plan.len() <= 64
            && !opts.plan.is_empty()
            && opts
                .plan
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "invalid plan id"
    );
    ensure!(
        usnm_ocr_contract::message::is_base_batch_name(&opts.batch),
        "invalid base batch"
    );
    let _lock = Lock::take(&opts.root)?;
    let selected: BTreeSet<String> = String::from_utf8(read(&opts.pages, 64 * 1024)?)?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    ensure!(
        !selected.is_empty() && selected.len() <= 100,
        "select 1 to 100 canonical page keys"
    );
    for s in &selected {
        let k: PageKey = s.parse()?;
        ensure!(k.to_string() == *s, "noncanonical page key");
    }
    let selected_docs: BTreeSet<String> = selected
        .iter()
        .map(|page| page.parse::<PageKey>().map(|key| key.doc_id()))
        .collect::<Result<_, _>>()?;
    // A queue-fed page comes from its claim: a user-requested page is in no prefetch
    // manifest (#1055), and its claim already holds the batch version the ledger row and
    // the published snapshot agreed on. A cohort page comes from the manifest.
    let (rows, mut claims) = if let Some(dir) = &opts.claims {
        let (rows, claims) = claimed_rows(dir, &opts.batch, &selected)?;
        (rows, Some(claims))
    } else {
        let rows = usnm_ocr_contract::prefetch::decode_where(
            bytes::Bytes::from(read(&opts.manifest, 128 << 20)?),
            |row| row.batch == opts.batch && selected_docs.contains(&row.doc_id),
        )?;
        (rows, None)
    };
    let mut chosen = BTreeMap::new();
    for row in rows {
        if row.batch == opts.batch && selected.contains(&row.key()?.to_string()) {
            ensure!(
                chosen.insert(row.key()?.to_string(), row).is_none(),
                "duplicate selected manifest page"
            );
        }
    }
    ensure!(
        chosen.len() == selected.len(),
        "{}",
        if claims.is_some() {
            "selected pages missing from their claims/batch"
        } else {
            "selected pages missing from manifest/batch"
        }
    );
    let installed = installed_models();
    let snapshot = ocr_local::boinc::Snapshot::load(state, reference).await?;
    let mut records = Vec::new();
    for (page, row) in chosen {
        let (scan, side) = cached(&opts.scans, &row)?;
        let title_languages = snapshot
            .titles
            .get(&row.lccn)
            .context("selected title is not published")?;
        let now = Utc::now();
        // A queued page keeps its queue message and ledger row; a cohort page
        // gets a bulk message of its own now.
        let (message, ledger_row) = if let Some(claims) = claims.as_mut() {
            let claim = claims
                .remove(&row.doc_id)
                .context("no claim for a selected page")?;
            let message = claim.message()?;
            ensure!(
                message.page == page
                    && message.batch == row.batch
                    && claim.ledger.batch_version == row.batch_version,
                "the claim for {} is not the manifest's page or batch version",
                row.doc_id
            );
            (message, Some(claim.ledger))
        } else {
            let language =
                ocr_local::enqueue::message_language(row.lang.as_deref(), title_languages)
                    .context("page has no mapped detected or title language")?;
            let message = OcrMessage {
                v: 1,
                page,
                batch: row.batch.clone(),
                language,
                reason: Reason::Bulk,
                score: Some(row.damage_rate),
                queued_at: now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            };
            (message, None)
        };
        let selected = engine::route(&message.language, &installed)
            .context("selected engine/profile unavailable on this Tesseract node")?;
        let profile = selected.profile;
        let mut job = Job {
            v: boinc::VERSION,
            id: String::new(),
            message,
            batch_version: row.batch_version,
            scan: boinc::Scan {
                url: side.loc_url,
                iiif_base: side.iiif_base,
                sha256: side.sha256,
                bytes: side.bytes,
                md5: side.md5,
            },
            engine: boinc::Engine {
                kind: selected.kind,
                image: opts.image.clone(),
                version: "5.5.0".into(),
                profile: profile.clone(),
                models: engine::models_digest(engine::MODELS_LOCK, &profile)
                    .context("unlocked model")?
                    .to_string(),
                psm: 3,
                dpi: 300,
                timeout_secs: opts.timeout_secs,
            },
        };
        job.id = job.identity()?;
        job.validate()?;
        let job_dir = dir(&opts.root, &job.id)?;
        let record = if job_dir.join("record.json").exists() {
            let old = load(&opts.root, &job.id)?;
            ensure!(
                old.plan == opts.plan,
                "job belongs to another allocation plan"
            );
            old
        } else {
            atomic(&job_dir.join("page"), &scan)?;
            atomic(
                &job_dir.join("request.json"),
                &serde_json::to_vec_pretty(&job)?,
            )?;
            let record = Record {
                job,
                plan: opts.plan.clone(),
                stage: Stage::Prepared,
                stamp: now.format("%Y%m%dT%H%M%S%6fZ").to_string(),
                error: None,
                ledger: ledger_row,
                bundle: None,
                unbundled: None,
            };
            save(&opts.root, &record)?;
            record
        };
        records.push(record);
    }
    let pending: Vec<_> = records
        .iter()
        .filter(|r| r.stage == Stage::Prepared)
        .collect();
    if !pending.is_empty() {
        let rows = pending
            .iter()
            .map(|r| r.queued())
            .collect::<anyhow::Result<Vec<_>>>()?;
        if opts.claims.is_some() {
            // Their rows are in the ledger already: check, don't write.
            ocr_local::boinc::adopt(state, curated, &snapshot, &rows).await?;
        } else {
            // Deterministic cohort name from full IDs; request times remain in
            // the persisted records. A retry can contain already-written
            // reservations.
            let ids: Vec<_> = pending.iter().map(|r| &r.job.id).collect();
            let run = format!("boinc-{}", boinc::sha256(&serde_json::to_vec(&ids)?));
            ocr_local::boinc::reserve(state, curated, &snapshot, &rows, &run).await?;
        }
        for record in &mut records {
            if record.stage == Stage::Prepared {
                record.stage = Stage::Reserved;
                save(&opts.root, record)?;
            }
        }
    }
    let ids = records.into_iter().map(|r| r.job.id).collect();
    Ok(ids)
}

/// A returned result's `text.txt` as a job's page is stored: normalized for the
/// job's engine and language (`Hyphenation::for_page`), so a word broken at a
/// line end is rejoined after `-`, `⸗`, `¬` or `‐` on any page, and after an
/// em dash `—` only on a German page or one read with a Fraktur model, and
/// then only before a lowercase letter (#1065). `import` stores this text,
/// and the spot check compares two hosts' answers as this text, so a pair
/// agrees exactly when what would be stored agrees.
pub(crate) fn normalized_text(job: &Job, text: &[u8]) -> anyhow::Result<String> {
    let hyphenation = usnm_core::text::Hyphenation::for_page(
        &job.engine_tag()?.to_string(),
        &job.message.language,
    );
    Ok(usnm_core::text::normalize_ocr_for(
        std::str::from_utf8(text)?,
        hyphenation,
    ))
}

/// Publish a validated result using fixed names. No source scans are deleted.
pub async fn import(
    root: &Path,
    id: &str,
    manifest: &Path,
    text_path: &Path,
    alto_path: &Path,
    curated: &dyn ObjectStore,
    out: &dyn ObjectStore,
) -> anyhow::Result<()> {
    let _lock = Lock::take(root)?;
    let mut record = load(root, id)?;
    if record.stage == Stage::Published {
        return Ok(());
    }
    ensure!(
        matches!(
            record.stage,
            Stage::Reserved | Stage::Submitted | Stage::Submitting
        ),
        "job is not reserved/submitted"
    );
    let accepted = dir(root, id)?.join("accepted");
    let (manifest, text_path, alto_path) = if accepted.join("result.json").exists() {
        (
            accepted.join("result.json"),
            accepted.join("text.txt"),
            accepted.join("layout.xml"),
        )
    } else {
        (
            manifest.to_owned(),
            text_path.to_owned(),
            alto_path.to_owned(),
        )
    };
    let result_bytes = read(&manifest, boinc::MAX_JSON)?;
    let result: ResultManifest = serde_json::from_slice(&result_bytes)?;
    let text = read(&text_path, boinc::MAX_TEXT)?;
    let alto = read(&alto_path, boinc::MAX_ALTO)?;
    result.matches(&record.job, &text, &alto)?;
    checks::check_alto(&alto).map_err(|r| anyhow::anyhow!("{}", r.code()))?;
    let key = record.job.message.key()?;
    let raw_text = text.clone();
    let text = normalized_text(&record.job, &text)?;
    let raw_alto = alto.clone();
    let alto = engine::set_alto_source(
        std::str::from_utf8(&alto)?,
        &record.job.scan.url,
        &record.job.scan.sha256,
        record.job.scan.md5.as_deref(),
    );
    checks::check_alto(alto.as_bytes()).map_err(|r| anyhow::anyhow!("{}", r.code()))?;
    let row = usnm_overlay::OverlayRow {
        key: key.clone(),
        batch: record.job.message.batch.clone(),
        ocr_source: format!("usnm-{}", record.job.engine.kind.family()),
        ocr_engine: record.job.engine_tag()?.to_string(),
        loc_text: record.job.message.reason.as_str().into(),
        text,
        image_url: record.job.scan.iiif_base.clone(),
        ocred_at: DateTime::parse_from_rfc3339(&result.completed_at)?.with_timezone(&Utc),
    };
    let part = usnm_overlay::write_part(&[row])?;
    let (alto_name, part_name) = record.paths()?;
    let parsed = checks::parse_part_name(&part_name).context("invalid output name")?;
    let ledger = ledger::load(curated).await?;
    let expected = record.queued()?;
    if ledger
        .outcomes
        .get(&key.doc_id())
        .is_some_and(|o| o.outcome == ledger::IMPORTED && o.source == part_name)
    {
        record.stage = Stage::Published;
        save(root, &record)?;
        return Ok(());
    }
    if record.ledger.is_some()
        && ledger.outstanding(&key.doc_id()).is_none()
        && ledger.imported(&key.doc_id())
    {
        // A queue-fed job shares its reservation with the nodes: its claim lapsed, and a
        // node imported the page first. That answer stands; this one is not published.
        record.stage = Stage::Published;
        record.error = Some("superseded: another producer imported this page".into());
        save(root, &record)?;
        return Ok(());
    }
    ensure!(
        ledger.outstanding(&key.doc_id()) == Some(&expected),
        "native reservation is no longer outstanding"
    );
    let ctx = checks::Context {
        published: BTreeMap::from([(record.job.message.batch.clone(), record.job.batch_version)])
            .into_iter()
            .collect(),
        titles: BTreeMap::from([(key.lccn.clone(), vec![record.job.message.language.clone()])])
            .into_iter()
            .collect(),
        ledger,
    };
    checks::check_part(&parsed, part.clone(), &ctx).map_err(|r| anyhow::anyhow!("{}", r.code()))?;
    // Snapshot first accepted bytes before external publication. The result manifest
    // is the commit marker; retries use this snapshot, even after spool cleanup.
    if !accepted.join("result.json").exists() {
        atomic(&accepted.join("text.txt"), &raw_text)?;
        atomic(&accepted.join("layout.xml"), &raw_alto)?;
        atomic(&accepted.join("result.json"), &result_bytes)?;
    }
    put_same(out, &alto_name, alto.into_bytes(), "application/xml").await?;
    put_same(out, &part_name, part, "application/vnd.apache.parquet").await?;
    record.stage = Stage::Published;
    record.error = None;
    save(root, &record)?;
    Ok(())
}
async fn put_same(
    store: &dyn ObjectStore,
    path: &str,
    bytes: Vec<u8>,
    mime: &str,
) -> anyhow::Result<()> {
    if !store.put_new(path, bytes.clone(), mime).await? {
        ensure!(
            store.get(path).await? == Some(bytes),
            "published object conflicts with retry"
        );
    }
    Ok(())
}

/// Why a workunit failed for good, from the assimilator's `_error` marker (its error mask, as
/// `0x<hex>`; none means `_no_output_files`), with the page deadline the job gave, so a dead
/// letter says what happened (#1159).
pub fn terminal_reason(marker: Option<&[u8]>, timeout_secs: u32) -> String {
    let mask = marker
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(str::trim)
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let Some(mask) = mask else {
        return format!("BOINC workunit returned no output files (page deadline {timeout_secs} s)");
    };
    let names: Vec<&str> = [
        (0x1, "couldn't send a result"),
        (0x2, "too many error results"),
        (0x4, "too many success results"),
        (0x8, "too many total results"),
        (0x10, "cancelled"),
        (0x20, "no canonical result"),
    ]
    .into_iter()
    .filter(|(bit, _)| mask & bit != 0)
    .map(|(_, name)| name)
    .collect();
    format!(
        "BOINC workunit failed for good: error mask {mask:#x} ({}); page deadline {timeout_secs} s",
        if names.is_empty() {
            "unknown".to_owned()
        } else {
            names.join(", ")
        }
    )
}

#[cfg(test)]
mod terminal_reason_tests {
    #[test]
    fn names_the_mask_and_the_deadline() {
        assert_eq!(
            super::terminal_reason(Some(b"0x2\n"), 1800),
            "BOINC workunit failed for good: error mask 0x2 (too many error results); page deadline 1800 s"
        );
        assert_eq!(
            super::terminal_reason(Some(b"0x22"), 600),
            "BOINC workunit failed for good: error mask 0x22 (too many error results, no canonical result); page deadline 600 s"
        );
        assert_eq!(
            super::terminal_reason(None, 600),
            "BOINC workunit returned no output files (page deadline 600 s)"
        );
        assert!(super::terminal_reason(Some(b"0x2"), 1800).len() <= 200);
    }
}

/// Match stock batch_collect_assimilator's canonical output names.
pub async fn collect(
    root: &Path,
    spool: &Path,
    id: &str,
    curated: &dyn ObjectStore,
    out: &dyn ObjectStore,
) -> anyhow::Result<()> {
    let job = load(root, id)?.job;
    let workunit = job.workunit();
    let error_marker = spool.join(format!("{workunit}_error"));
    if error_marker.exists() || spool.join(format!("{workunit}_no_output_files")).exists() {
        let _lock = Lock::take(root)?;
        let mut record = load(root, id)?;
        ensure!(record.stage != Stage::Published, "already published");
        record.stage = Stage::Failed;
        record.error = Some(terminal_reason(
            std::fs::read(&error_marker).ok().as_deref(),
            job.engine.timeout_secs,
        ));
        save(root, &record)?;
        bail!("BOINC terminal failure");
    }
    import(
        root,
        id,
        &spool.join(format!("{workunit}__file_result.json")),
        &spool.join(format!("{workunit}__file_text.txt")),
        &spool.join(format!("{workunit}__file_layout.xml")),
        curated,
        out,
    )
    .await
}
