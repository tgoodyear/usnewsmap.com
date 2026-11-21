//! Derived row parts only: no scientific inputs or historical parts are removed.
use super::collect::{CropRow, Options, PageRow, Summary};
use crate::qwen::{ledger::MAX_MEMBERSHIP, stage::Membership};
use anyhow::{ensure, Context};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::time::Duration;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};
use usnm_ocr_contract::{quality::Job, qwen};

const FORMAT: &[u8] = b"quality-row-part-v4";
/// The digest index in the digest directory, and the format it is written in.
const DIGESTS_FILE: &str = "index-v2.json";
const DIGESTS_FORMAT: &str = "quality-input-digests-v2";
/// A file whose ctime is more recent than this is hashed every time and its digest never kept:
/// a rewrite within the same timestamp tick as the hash could leave its identity as it was, and
/// the file server's clock may be some seconds off ours.
#[cfg(unix)]
const SETTLE: Duration = Duration::from_secs(60);
/// Digests not yet written out are written this often, so a run that is killed (a warming round
/// at its limit) keeps what it hashed until then and the next one resumes from there.
#[cfg(unix)]
const FLUSH: Duration = Duration::from_secs(15);
/// Past this many digests, an index keeps only the ones its last run used or added.
const MAX_DIGESTS: usize = 400_000;

#[cfg(test)]
thread_local! {
    /// Tests write their inputs a moment before they read them; they may settle them sooner.
    pub(super) static TEST_SETTLE: Cell<Option<std::time::Duration>> = const { Cell::new(None) };
}

#[cfg(unix)]
fn settle() -> Duration {
    #[cfg(test)]
    {
        if let Some(settle) = TEST_SETTLE.with(Cell::get) {
            return settle;
        }
    }
    SETTLE
}

/// An input was rewritten while it was keyed. A run under the step lock treats it as a failure; a
/// warming run, which has no lock to keep inputs still, skips the unit and keys it again.
#[derive(Debug)]
pub(super) struct InputChanged(pub(super) &'static str);

impl std::fmt::Display for InputChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for InputChanged {}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

#[cfg(unix)]
impl FileIdentity {
    fn read(file: &File) -> anyhow::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata()?;
        ensure!(m.is_file(), "fingerprint input is not a regular file");
        Ok(Self {
            device: m.dev(),
            inode: m.ino(),
            size: m.size(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        })
    }

    /// The path's identity as an open of it sees it: on NFS, opening revalidates the attributes
    /// (close-to-open), where a bare stat may answer from the attribute cache.
    fn of_path(path: &Path) -> Option<Self> {
        Self::read(&File::open(path).ok()?).ok()
    }

    /// Whether a digest of the file may be kept for its identity: its ctime has nanoseconds (a
    /// coarse one cannot tell a same-size rewrite with restored mtime) and has settled.
    fn fine(&self) -> bool {
        self.changed.1 != 0
    }

    fn settled(&self) -> bool {
        let (Ok(secs), Ok(nanos)) = (u64::try_from(self.changed.0), u32::try_from(self.changed.1))
        else {
            return false;
        };
        let changed = std::time::UNIX_EPOCH + Duration::new(secs, nanos);
        self.fine() && changed + settle() <= std::time::SystemTime::now()
    }

    /// The index key: the identity alone, not the path, so a unit read through a partial scope's
    /// link and through its root, or by another run naming the same directory, shares a digest.
    fn key(&self) -> String {
        format!(
            "{}:{}:{}:{}.{:09}:{}.{:09}",
            self.device,
            self.inode,
            self.size,
            self.modified.0,
            self.modified.1,
            self.changed.0,
            self.changed.1
        )
    }
}

#[cfg(unix)]
#[derive(serde::Serialize, serde::Deserialize)]
struct InputDigest {
    identity: FileIdentity,
    sha256: Vec<u8>,
}

fn contents(input: &mut File) -> anyhow::Result<Vec<u8>> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(digest.finalize().to_vec())
}

/// The digest a retained receipt holds for this input, or why it can't be trusted.
#[cfg(unix)]
fn digest_receipt(bytes: &[u8], key: &str, identity: &FileIdentity) -> anyhow::Result<Vec<u8>> {
    let envelope: Envelope = serde_json::from_slice(bytes)?;
    ensure!(
        envelope.fingerprint == key,
        "input digest identity mismatch"
    );
    ensure!(
        envelope.rows_sha256 == hex(&Sha256::digest(envelope.rows.as_bytes())),
        "input digest checksum mismatch; receipt retained"
    );
    let saved: InputDigest = serde_json::from_str(&envelope.rows)?;
    ensure!(
        &saved.identity == identity && saved.sha256.len() == 32,
        "invalid input digest receipt"
    );
    Ok(saved.sha256)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (text.len() == 64 && text.is_ascii())
        .then(|| {
            (0..64)
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
                .collect()
        })
        .flatten()
}

/// SHA-256 digests of input files by file identity (device, inode, size, nanosecond mtime and
/// ctime), kept in one index that every run naming the directory shares. A file whose identity is
/// in the index is opened (to revalidate its attributes) but not read; any change of identity
/// reads it again. Only settled files with nanosecond ctime are kept, so other filesystems, and
/// files just written, are hashed every time.
pub(super) struct Digests {
    dir: PathBuf,
    /// Where `quality-input-digest-v1` receipts were kept, one file per input keyed on its path
    /// too. They are read on a miss, so a cache keeps what it hashed before; never written.
    #[cfg(unix)]
    legacy: PathBuf,
    known: RefCell<BTreeMap<String, String>>,
    added: RefCell<BTreeMap<String, String>>,
    used: RefCell<BTreeSet<String>>,
    flushed: Cell<Instant>,
}

fn read_index(path: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e).context("reading the input digest index"),
    };
    let envelope: Envelope = serde_json::from_slice(&bytes).context("input digest index")?;
    ensure!(
        envelope.fingerprint == DIGESTS_FORMAT,
        "input digest index format mismatch; index retained"
    );
    ensure!(
        envelope.rows_sha256 == hex(&Sha256::digest(envelope.rows.as_bytes())),
        "input digest index checksum mismatch; index retained"
    );
    let index: BTreeMap<String, String> = serde_json::from_str(&envelope.rows)?;
    ensure!(
        index.values().all(|digest| unhex(digest).is_some()),
        "invalid input digest index; index retained"
    );
    Ok(index)
}

impl Digests {
    pub(super) fn open(dir: &Path, legacy: &Path) -> anyhow::Result<Self> {
        #[cfg(not(unix))]
        let _ = legacy;
        Ok(Self {
            dir: dir.to_owned(),
            #[cfg(unix)]
            legacy: legacy.to_owned(),
            known: RefCell::new(read_index(&dir.join(DIGESTS_FILE))?),
            added: RefCell::new(BTreeMap::new()),
            used: RefCell::new(BTreeSet::new()),
            flushed: Cell::new(Instant::now()),
        })
    }

    /// The SHA-256 of `path`'s contents, or None when there is no such file. With `strict`, an
    /// input rewritten while it was hashed is `InputChanged`; without, what was read is returned
    /// and not kept.
    fn digest(&self, path: &Path, strict: bool) -> anyhow::Result<Option<Vec<u8>>> {
        let mut input = match File::open(path) {
            Ok(input) => input,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("fingerprinting {path:?}")),
        };
        #[cfg(unix)]
        {
            let identity = FileIdentity::read(&input)?;
            if !identity.fine() {
                return Ok(Some(contents(&mut input)?));
            }
            let settled = identity.settled();
            if settled {
                let key = identity.key();
                if let Some(digest) = self.known.borrow().get(&key).and_then(|d| unhex(d)) {
                    self.used.borrow_mut().insert(key);
                    return Ok(Some(digest));
                }
            }
            let digest = match settled.then(|| self.legacy(path, &identity)).transpose()? {
                Some(Some(digest)) => digest,
                _ => contents(&mut input)?,
            };
            if FileIdentity::read(&input)? == identity
                && FileIdentity::of_path(path).as_ref() == Some(&identity)
            {
                if settled {
                    self.keep(&identity, &digest)?;
                }
            } else {
                ensure!(!strict, InputChanged("input changed while fingerprinting"));
            }
            Ok(Some(digest))
        }
        #[cfg(not(unix))]
        {
            let _ = strict;
            Ok(Some(contents(&mut input)?))
        }
    }

    /// The digest a `quality-input-digest-v1` receipt holds for this input, if there is one.
    #[cfg(unix)]
    fn legacy(&self, path: &Path, identity: &FileIdentity) -> anyhow::Result<Option<Vec<u8>>> {
        let mut key = Sha256::new();
        field(&mut key, b"quality-input-digest-v1");
        field(&mut key, path.as_os_str().as_encoded_bytes());
        field(&mut key, &serde_json::to_vec(identity)?);
        let key = hex(&key.finalize());
        match std::fs::read(self.legacy.join(format!("{key}.json"))) {
            Ok(bytes) => Ok(Some(digest_receipt(&bytes, &key, identity)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading retained input digest"),
        }
    }

    #[cfg(unix)]
    fn keep(&self, identity: &FileIdentity, digest: &[u8]) -> anyhow::Result<()> {
        let key = identity.key();
        self.known.borrow_mut().insert(key.clone(), hex(digest));
        self.added.borrow_mut().insert(key.clone(), hex(digest));
        self.used.borrow_mut().insert(key);
        if self.flushed.get().elapsed() >= FLUSH {
            self.save()?;
        }
        Ok(())
    }

    /// Writes the digests added since the last save into the index, merged with what other runs
    /// wrote there meanwhile. Two runs saving at once can lose one's additions, which are then
    /// hashed again; an identity two runs saw with different digests is dropped from it.
    pub(super) fn save(&self) -> anyhow::Result<()> {
        self.flushed.set(Instant::now());
        if self.added.borrow().is_empty() {
            return Ok(());
        }
        let added = std::mem::take(&mut *self.added.borrow_mut());
        let path = self.dir.join(DIGESTS_FILE);
        let mut index = read_index(&path)?;
        for (key, digest) in added {
            match index.get(&key) {
                Some(other) if *other != digest => {
                    index.remove(&key);
                    self.known.borrow_mut().remove(&key);
                }
                _ => {
                    index.insert(key, digest);
                }
            }
        }
        if index.len() > MAX_DIGESTS {
            let used = self.used.borrow();
            index.retain(|key, _| used.contains(key));
        }
        let rows = serde_json::to_string(&index)?;
        let envelope = Envelope {
            fingerprint: DIGESTS_FORMAT.to_owned(),
            rows_sha256: hex(&Sha256::digest(rows.as_bytes())),
            rows,
        };
        std::fs::create_dir_all(&self.dir)?;
        let mut staged = tempfile::NamedTempFile::new_in(&self.dir)?;
        staged.write_all(&serde_json::to_vec(&envelope)?)?;
        staged.as_file().sync_all()?;
        staged.persist(&path)?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
}

impl Drop for Digests {
    /// A run that fails still keeps the digests it computed; one whose save fails hashes again.
    fn drop(&mut self) {
        let _ = self.save();
    }
}

/// The identity of a staged input for the scheduling inventory, which never reads its contents.
#[cfg(unix)]
fn inventory_identity(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let input = match File::open(path) {
        Ok(input) => input,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("fingerprinting {path:?}")),
    };
    let identity = FileIdentity::read(&input)?;
    if !identity.fine() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_vec(&identity)?))
}

#[cfg(not(unix))]
fn inventory_identity(_path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    Ok(None)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Part {
    pub summary: Summary,
    pub pages: Vec<PageRow>,
    pub crops: Vec<CropRow>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    fingerprint: String,
    rows_sha256: String,
    rows: String,
}

pub(super) struct Cache {
    dir: PathBuf,
    version: Vec<u8>,
    qwen: BTreeMap<String, Vec<PathBuf>>,
    qwen_dependencies: BTreeMap<PathBuf, Dependency>,
    /// Each Qwen job file's job id.
    job_ids: BTreeMap<PathBuf, String>,
    /// Each Qwen job's crop-state records as listed: `(dir, job id, records)`.
    crop_states: Vec<(PathBuf, String, Vec<PathBuf>)>,
    /// Each bundle page's pair records as listed: `(dir, job id, doc id, records)`.
    page_records: Vec<(PathBuf, String, String, Vec<PathBuf>)>,
    surya: Dependency,
    qwen_root: PathBuf,
    surya_root: Option<PathBuf>,
    inventory: Vec<PathBuf>,
    included: RefCell<BTreeMap<PathBuf, TrackedUnit>>,
    digests: Digests,
}

struct TrackedUnit {
    canonical: Vec<PathBuf>,
    key: String,
    inventory: bool,
}

struct Dependency {
    paths: Vec<PathBuf>,
    /// Hashed ahead of the paths when not empty, so an empty one keeps the digest as it was.
    salt: Vec<u8>,
    digest: Vec<u8>,
}

impl Dependency {
    fn new(paths: Vec<PathBuf>, digests: &Digests) -> anyhow::Result<Self> {
        Self::salted(paths, Vec::new(), digests)
    }

    fn salted(mut paths: Vec<PathBuf>, salt: Vec<u8>, digests: &Digests) -> anyhow::Result<Self> {
        paths.sort();
        paths.dedup();
        let digest = Self::hash(&paths, &salt, digests)?;
        Ok(Self {
            paths,
            salt,
            digest,
        })
    }

    fn hash(paths: &[PathBuf], salt: &[u8], digests: &Digests) -> anyhow::Result<Vec<u8>> {
        let mut hash = Sha256::new();
        if !salt.is_empty() {
            field(&mut hash, salt);
        }
        for path in paths {
            fingerprint_input(&mut hash, path, digests, false, false)?;
        }
        Ok(hash.finalize().to_vec())
    }

    fn unchanged(&self, digests: &Digests) -> anyhow::Result<bool> {
        Ok(Self::hash(&self.paths, &self.salt, digests)? == self.digest)
    }

    #[cfg(test)]
    fn verify(&self, digests: &Digests) -> anyhow::Result<()> {
        ensure!(
            self.unchanged(digests)?,
            "reading dependencies changed during collection"
        );
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn field_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u64).to_le_bytes());
    out.extend(bytes);
}

/// The path, whether it is there and the digest of its contents (or, for a staged input in the
/// scheduling inventory, its identity), as every key hashes an input.
fn fingerprint_input(
    hash: &mut Sha256,
    path: &Path,
    digests: &Digests,
    identity_only: bool,
    strict: bool,
) -> anyhow::Result<()> {
    field(hash, path.as_os_str().as_encoded_bytes());
    // Scheduling hints never authorize cache reuse or accepted scientific rows.
    if identity_only {
        if let Some(identity) = inventory_identity(path)? {
            field(hash, b"present");
            field(hash, b"inventory-identity-v1");
            field(hash, &identity);
            return Ok(());
        }
    }
    match digests.digest(path, strict)? {
        Some(digest) => {
            field(hash, b"present");
            field(hash, &digest);
        }
        None => field(hash, b"missing"),
    }
    Ok(())
}

fn jobs(root: &Path, name: &str) -> anyhow::Result<Vec<PathBuf>> {
    let dir = super::units(root);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).context("reading canonical dependency inventory"),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let unit = entry?.path();
        if !std::fs::metadata(&unit)?.is_dir() {
            continue;
        }
        let path = unit.join(name);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => paths.push(path),
            Ok(_) => anyhow::bail!("reading job is not a regular file: {path:?}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e).context("reading canonical dependency job"),
        }
    }
    paths.sort();
    Ok(paths)
}

fn surya_jobs(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = crate::surya::unit_dirs(root)?
        .into_iter()
        .map(|unit| unit.join(crate::surya::JOB_FILE))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

impl Cache {
    pub fn new(dir: &Path, opts: &Options) -> anyhow::Result<Self> {
        let legacy = dir.join("input-digests");
        let digests = Digests::open(opts.digest_cache.as_deref().unwrap_or(&legacy), &legacy)?;
        let mut version = Sha256::new();
        field(&mut version, FORMAT);
        field(&mut version, opts.campaign.as_bytes());
        // A release/rebuild (including hat or contract changes) invalidates all derived rows.
        fingerprint_input(
            &mut version,
            &std::env::current_exe()?,
            &digests,
            false,
            false,
        )?;
        // Whether readings join crop for crop changes every row that has one.
        match &opts.qwen_crop_states {
            Some(dir) => {
                field(&mut version, b"qwen-crop-states");
                field(&mut version, dir.as_os_str().as_encoded_bytes());
            }
            None => field(&mut version, b"no-qwen-crop-states"),
        }
        // Whether crop texts are measured, and with which files, changes every crop row.
        match &opts.qe_dir {
            Some(_) => {
                field(&mut version, b"qe-dir");
                field(&mut version, super::qe::LOCK.as_bytes());
            }
            None => field(&mut version, b"no-qe-dir"),
        }
        let mut crop_states = Vec::new();
        let mut page_records = Vec::new();
        let mut qwen: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        let mut qwen_dependencies = BTreeMap::new();
        let mut job_ids = BTreeMap::new();
        let mut job_bytes = Vec::new();
        let mut inventory = jobs(&opts.qwen_root, crate::qwen::JOB_FILE)?;
        for path in &inventory {
            let bytes = crate::read(path, qwen::MAX_JOB_JSON)?;
            let job: qwen::Job = serde_json::from_slice(&bytes)?;
            let mut paths = vec![path.clone()];
            let mut salt = Vec::new();
            // Which of this unit's readings are excluded re-keys the pages that carry them, and
            // only those; a unit with no exclusion keeps its key.
            if let Some(print) = opts.qwen_exclusions.job_fingerprint(&job.id) {
                field_bytes(&mut salt, b"qwen-exclusions");
                field_bytes(&mut salt, print.as_bytes());
            }
            if let Some(spool) = super::collect::qwen_spool_for(
                &job,
                &opts.qwen_spool,
                opts.qwen_xiaomi_spool.as_deref(),
            ) {
                paths.extend(super::collect::spool_files(
                    spool,
                    &job.workunit(),
                    &crate::qwen::outputs(),
                ));
            }
            if let Some(dir) = &opts.qwen_crop_states {
                // A pair judged later can vouch for more of the same canonical result.
                let records = crate::qwen::consensus::record_paths(dir, &job.id)?;
                paths.extend(records.iter().cloned());
                crop_states.push((dir.clone(), job.id.clone(), records));
            }
            if let Some(dir) = opts.qwen_pages.as_ref().filter(|_| !job.pages.is_empty()) {
                // A bundle joins its agreed pages from their pair records
                // (and, settled by its pages, only those), traced through the membership to
                // the page units it was packed from. Only bundles' keys
                // change, so a root of single pages needn't warm again.
                field_bytes(&mut salt, b"qwen-pages-v2");
                field_bytes(&mut salt, dir.as_os_str().as_encoded_bytes());
                let unit = path.parent().context("Qwen unit directory")?;
                let membership = unit.join(crate::qwen::stage::MEMBERSHIP_FILE);
                // When the membership parses, the job of every member it names
                // under a plain unit name is in the key, valid or not: whether
                // the ledger accepts the membership depends on those files
                // (`check_sources`), so mending one must re-key. A membership
                // that doesn't parse, or a name that would leave `pages/`, is
                // refused by the ledger whatever the files hold, and the
                // membership's own bytes are in the key below.
                if let Some(m) = crate::read(&membership, MAX_MEMBERSHIP)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Membership>(&b).ok())
                {
                    paths.extend(
                        m.pages
                            .iter()
                            .filter(|m| !m.unit.is_empty() && !m.unit.contains('/'))
                            .filter(|m| !m.unit.starts_with('.'))
                            .map(|m| {
                                opts.qwen_root
                                    .join("pages")
                                    .join(&m.unit)
                                    .join(crate::qwen::JOB_FILE)
                            }),
                    );
                }
                paths.push(membership);
                // Whether a `page-v1` pair settled the bundle decides whether
                // its canonical result joins the pages it did not accept.
                if usnm_ocr_contract::boinc::is_sha256(&job.id) {
                    paths.push(
                        dir.join(&job.id)
                            .join(crate::qwen::validate::UNIT_VERDICT_FILE),
                    );
                }
                if job.validate().is_ok() {
                    for page in &job.pages {
                        let records =
                            crate::qwen::ledger::record_paths(dir, &job.id, &page.doc_id)?;
                        paths.extend(records.iter().cloned());
                        page_records.push((
                            dir.clone(),
                            job.id.clone(),
                            page.doc_id.clone(),
                            records,
                        ));
                    }
                }
            }
            for doc in std::iter::once(&job.doc_id).chain(job.pages.iter().map(|p| &p.doc_id)) {
                qwen.entry(doc.clone()).or_default().push(path.clone());
            }
            qwen_dependencies.insert(path.clone(), Dependency::salted(paths, salt, &digests)?);
            job_ids.insert(path.clone(), job.id.clone());
            job_bytes.push((path.clone(), bytes));
        }
        let mut surya = Vec::new();
        if let (Some(root), Some(spool)) = (&opts.surya_root, &opts.surya_spool) {
            let paths = surya_jobs(root)?;
            inventory.extend(paths.clone());
            for path in paths {
                let bytes = crate::read(&path, usnm_ocr_contract::surya::MAX_JSON)?;
                let job: usnm_ocr_contract::surya::Job = serde_json::from_slice(&bytes)?;
                job_bytes.push((path.clone(), bytes));
                surya.push(path);
                surya.extend(super::collect::spool_files(
                    spool,
                    &job.workunit(),
                    &crate::surya::outputs(),
                ));
                surya.push(spool.join(format!("{}_error", job.workunit())));
                surya.push(spool.join(format!("{}_no_output_files", job.workunit())));
            }
        }
        let surya = Dependency::new(surya, &digests)?;
        // Each job as it was parsed is still there: checked against its digest, which an
        // unchanged identity gives without reading the job again.
        for (path, bytes) in job_bytes {
            ensure!(
                digests.digest(&path, false)? == Some(Sha256::digest(&bytes).to_vec()),
                "reading job changed during inventory"
            );
        }
        Ok(Self {
            dir: dir.to_owned(),
            version: version.finalize().to_vec(),
            qwen,
            qwen_dependencies,
            job_ids,
            crop_states,
            page_records,
            surya,
            qwen_root: opts.qwen_root.clone(),
            surya_root: opts.surya_root.clone(),
            inventory,
            included: RefCell::new(BTreeMap::new()),
            digests,
        })
    }

    /// Writes the digests this run computed for the next one (see `Digests`).
    pub fn save_digests(&self) -> anyhow::Result<()> {
        self.digests.save()
    }

    pub fn key(&self, unit: &Path, job: &Job, canonical: &[PathBuf]) -> anyhow::Result<String> {
        self.key_for(unit, job, canonical, false)
    }

    pub fn inventory_key(
        &self,
        unit: &Path,
        job: &Job,
        canonical: &[PathBuf],
    ) -> anyhow::Result<String> {
        let key = self.key_for(unit, job, canonical, true)?;
        self.included.borrow_mut().insert(
            unit.to_owned(),
            TrackedUnit {
                canonical: canonical.to_vec(),
                key: key.clone(),
                inventory: true,
            },
        );
        Ok(key)
    }

    fn key_for(
        &self,
        unit: &Path,
        job: &Job,
        canonical: &[PathBuf],
        inventory: bool,
    ) -> anyhow::Result<String> {
        let mut hash = Sha256::new();
        field(&mut hash, &self.version);
        if inventory {
            field(&mut hash, b"quality-scheduling-inventory-v1");
        }
        ensure!(
            serde_json::from_slice::<Job>(&std::fs::read(unit.join(super::JOB_FILE))?)? == *job,
            InputChanged("QUALITY job changed during collection")
        );
        let mut inputs = [
            super::JOB_FILE,
            super::PAGE_FILE,
            super::ALTO_FILE,
            super::TEXTS_FILE,
            super::PLACES_FILE,
            "submitted",
            super::collect::OURS_FILE,
        ]
        .iter()
        .map(|name| unit.join(name))
        .collect::<Vec<_>>();
        // A bundle with a dims.json gives collect its pages' sizes from that, not
        // from page.jpg (#1743), so the record is the input and the image is not:
        // pruning the image after collection leaves the key as it was. Units
        // without one keep the image in their key, as before.
        if !job.pages.is_empty() && unit.join(super::page_dims::DIMS_FILE).try_exists()? {
            for path in inputs.iter_mut() {
                if *path == unit.join(super::PAGE_FILE) {
                    *path = unit.join(super::page_dims::DIMS_FILE);
                }
            }
        }
        // Qualified doc IDs can place optional bundle ALTO sidecars below the unit directory.
        inputs.extend(
            job.pages
                .iter()
                .map(|page| unit.join(format!("{}.ours.xml", page.doc_id))),
        );
        inputs.extend_from_slice(canonical);
        if let Some(spool) = canonical.first().and_then(|p| p.parent()) {
            inputs.push(spool.join(format!("{}_error", job.workunit())));
            inputs.push(spool.join(format!("{}_no_output_files", job.workunit())));
        }
        let mut dependencies = BTreeSet::new();
        for doc in std::iter::once(&job.doc_id).chain(job.pages.iter().map(|p| &p.doc_id)) {
            if let Some(paths) = self.qwen.get(doc) {
                dependencies.extend(paths);
            }
        }
        for path in dependencies {
            field(&mut hash, &self.qwen_dependencies[path].digest);
        }
        field(&mut hash, &self.surya.digest);
        inputs.sort();
        inputs.dedup();
        for path in inputs {
            let staged_blob = [super::PAGE_FILE, super::ALTO_FILE, super::TEXTS_FILE]
                .iter()
                .any(|name| path == unit.join(name));
            fingerprint_input(
                &mut hash,
                &path,
                &self.digests,
                inventory && staged_blob,
                true,
            )?;
        }
        Ok(hex(&hash.finalize()))
    }

    pub fn track(&self, unit: &Path, canonical: &[PathBuf], key: &str) {
        self.included.borrow_mut().insert(
            unit.to_owned(),
            TrackedUnit {
                canonical: canonical.to_vec(),
                key: key.to_owned(),
                inventory: false,
            },
        );
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        for (unit, tracked) in self.included.borrow().iter() {
            let job = serde_json::from_slice(&crate::read(
                &unit.join(super::JOB_FILE),
                usnm_ocr_contract::quality::MAX_JOB_JSON,
            )?)?;
            ensure!(
                self.key_for(unit, &job, &tracked.canonical, tracked.inventory)? == tracked.key,
                "QUALITY inputs changed before campaign publication"
            );
        }
        // Staging takes the step lock the collector holds, so the job inventory is still.
        self.verify_inventory()
    }

    /// The reading jobs are the ones keyed: none staged or removed since.
    fn verify_inventory(&self) -> anyhow::Result<()> {
        let mut inventory = jobs(&self.qwen_root, crate::qwen::JOB_FILE)?;
        if let Some(root) = &self.surya_root {
            inventory.extend(surya_jobs(root)?);
        }
        ensure!(
            inventory == self.inventory,
            "reading job inventory changed during collection"
        );
        Ok(())
    }

    /// Whether every reading input of the page `doc` (its Qwen jobs'
    /// dependencies and record listings, and Surya's results) is as it was
    /// keyed. A unit whose
    /// readings changed while the run collected is published but not cached,
    /// so the next run collects it again under its new key. Checking only the
    /// page's own inputs lets a busy root, where results land every minute,
    /// still publish (#1435): the root-wide check failed nearly every run.
    pub fn readings_unchanged(&self, doc: &str) -> anyhow::Result<bool> {
        // Surya's results are one dependency for the root: any change counts.
        if !self.surya.unchanged(&self.digests)? {
            return Ok(false);
        }
        let Some(paths) = self.qwen.get(doc) else {
            return Ok(true);
        };
        for path in paths {
            if !self.qwen_dependencies[path].unchanged(&self.digests)? {
                return Ok(false);
            }
            let id = &self.job_ids[path];
            for (dir, job, records) in self.crop_states.iter().filter(|c| &c.1 == id) {
                if &crate::qwen::consensus::record_paths(dir, job)? != records {
                    return Ok(false);
                }
            }
            for (dir, job, page, records) in self.page_records.iter().filter(|r| &r.1 == id) {
                if &crate::qwen::ledger::record_paths(dir, job, page)? != records {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Every reading input of the root as it was keyed (tests only: a run
    /// checks each page's own inputs, `readings_unchanged`).
    #[cfg(test)]
    pub fn verify_readings(&self) -> anyhow::Result<()> {
        self.verify_inventory()?;
        for (dir, job, records) in &self.crop_states {
            ensure!(
                &crate::qwen::consensus::record_paths(dir, job)? == records,
                "crop states changed during collection"
            );
        }
        for (dir, job, doc, records) in &self.page_records {
            ensure!(
                &crate::qwen::ledger::record_paths(dir, job, doc)? == records,
                "page records changed during collection"
            );
        }
        for dep in self.qwen_dependencies.values() {
            dep.verify(&self.digests)?;
        }
        self.surya.verify(&self.digests)
    }

    pub fn load(&self, key: &str) -> anyhow::Result<Option<Part>> {
        let path = self.dir.join(format!("{key}.json"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading cached rows {path:?}")),
        };
        let envelope: Envelope = serde_json::from_slice(&bytes)?;
        ensure!(envelope.fingerprint == key, "cached row identity mismatch");
        ensure!(
            envelope.rows_sha256 == hex(&Sha256::digest(envelope.rows.as_bytes())),
            "cached rows checksum mismatch; historical part retained"
        );
        Ok(Some(serde_json::from_str(&envelope.rows)?))
    }

    /// Whether a part is stored under `key`; its contents are not read. For a warming run, which
    /// publishes nothing: a part that fails its checks is found by the publication that reads it.
    pub fn has(&self, key: &str) -> bool {
        self.dir.join(format!("{key}.json")).is_file()
    }

    pub fn store(&self, key: &str, part: &Part) -> anyhow::Result<()> {
        let rows = serde_json::to_string(part)?;
        let envelope = Envelope {
            fingerprint: key.to_owned(),
            rows_sha256: hex(&Sha256::digest(rows.as_bytes())),
            rows,
        };
        let path = self.dir.join(format!("{key}.json"));
        std::fs::create_dir_all(&self.dir)?;
        let mut staged = tempfile::NamedTempFile::new_in(&self.dir)?;
        staged.write_all(&serde_json::to_vec(&envelope)?)?;
        staged.as_file().sync_all()?;
        match staged.persist_noclobber(&path) {
            Ok(_) => File::open(&self.dir)?.sync_all()?,
            // Another collector of this cache stored this part first: the key names its inputs,
            // so it holds the same rows. Take it if it is valid; a corrupt one fails closed.
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.load(key)?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::collect::{collect, run, tests::cached_fixture};

    #[test]
    fn dependency_inventory_ignores_nonunits_but_rejects_nonfile_jobs() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let units = super::super::units(dir.path());
        std::fs::create_dir_all(&units).unwrap();
        std::fs::write(units.join("nonunit"), b"").unwrap();
        assert!(jobs(dir.path(), "job.json").unwrap().is_empty());
        std::fs::create_dir_all(units.join("unit/job.json")).unwrap();
        assert!(jobs(dir.path(), "job.json").is_err());
    }

    /// Settles inputs at once for this thread's test, as if they were written long ago.
    #[cfg(unix)]
    fn settled() {
        TEST_SETTLE.with(|s| s.set(Some(Duration::ZERO)));
    }

    #[cfg(unix)]
    fn kept(digests: &Digests, path: &Path) -> bool {
        let key = FileIdentity::of_path(path).unwrap().key();
        digests.known.borrow().contains_key(&key)
    }

    #[cfg(unix)]
    #[test]
    fn scheduling_identity_changes_when_the_path_is_replaced() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        std::fs::write(&path, b"staged scientific input").unwrap();
        let first = inventory_identity(&path).unwrap().unwrap();
        assert_eq!(inventory_identity(&path).unwrap().unwrap(), first);
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"staged scientific input").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert_ne!(inventory_identity(&path).unwrap().unwrap(), first);
        assert!(inventory_identity(&dir.path().join("missing"))
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cold_inventory_does_not_warm_row_cache_and_detects_restored_mtime_rewrites() {
        settled();
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let canonical = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let hint = cache.inventory_key(&unit, &job, &canonical).unwrap();
        let path = unit.join(super::super::PAGE_FILE);
        // The scheduling inventory keeps the canonical result's digest, never a staged blob's.
        assert!(!kept(&cache.digests, &path));
        assert!(kept(&cache.digests, &canonical[0]));
        assert!(cache.load(&hint).unwrap().is_none());
        cache.verify().unwrap();
        assert!(!kept(&cache.digests, &path));
        let rows = cache.key(&unit, &job, &canonical).unwrap();
        assert_ne!(hint, rows);
        assert!(kept(&cache.digests, &path));
        cache.save_digests().unwrap();
        assert!(opts
            .cache
            .as_ref()
            .unwrap()
            .join("input-digests")
            .join(DIGESTS_FILE)
            .is_file());
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 1;
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, bytes).unwrap();
        File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(cache.verify().is_err());
        let changed = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        assert_ne!(
            changed.inventory_key(&unit, &job, &canonical).unwrap(),
            hint
        );
        assert_ne!(changed.key(&unit, &job, &canonical).unwrap(), rows);
    }

    #[tokio::test]
    async fn a_part_another_collector_stored_first_is_accepted_only_if_valid() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let canonical = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let key = cache.key(&unit, &job, &canonical).unwrap();
        let part = Part {
            summary: Summary::default(),
            pages: vec![],
            crops: vec![],
        };
        cache.store(&key, &part).unwrap();
        // The same key stored again, as by a warming run and a publication racing: not an error.
        cache.store(&key, &part).unwrap();
        // A corrupt entry under the key still fails closed, and is kept.
        let path = opts.cache.as_ref().unwrap().join(format!("{key}.json"));
        std::fs::write(&path, b"{garbage").unwrap();
        assert!(cache.store(&key, &part).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{garbage");
    }

    /// Rewrites the index with `digest` for `path`'s identity, keeping its checksum valid.
    #[cfg(unix)]
    fn forge(dir: &Path, path: &Path, digest: &[u8]) {
        let mut index = read_index(&dir.join(DIGESTS_FILE)).unwrap();
        index.insert(FileIdentity::of_path(path).unwrap().key(), hex(digest));
        let rows = serde_json::to_string(&index).unwrap();
        let envelope = Envelope {
            fingerprint: DIGESTS_FORMAT.into(),
            rows_sha256: hex(&Sha256::digest(rows.as_bytes())),
            rows,
        };
        std::fs::write(
            dir.join(DIGESTS_FILE),
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_kept_digest_is_used_without_reading_the_unchanged_input() {
        settled();
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("result.json");
        let memo = dir.path().join("digests");
        std::fs::write(&path, b"canonical result").unwrap();
        let digests = Digests::open(&memo, &memo).unwrap();
        let first = digests.digest(&path, true).unwrap().unwrap();
        assert_eq!(first, Sha256::digest(b"canonical result").to_vec());
        drop(digests);
        // What the index says is what a later run uses: it does not read the file again.
        forge(&memo, &path, &[7; 32]);
        let again = Digests::open(&memo, &memo).unwrap();
        assert_eq!(again.digest(&path, true).unwrap().unwrap(), vec![7; 32]);
        assert!(again
            .digest(&dir.path().join("missing"), true)
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn same_size_rewrite_with_restored_mtime_and_replacement_read_the_input_again() {
        settled();
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        let memo = dir.path().join("digests");
        std::fs::write(&path, b"first").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let digests = Digests::open(&memo, &memo).unwrap();
        let first = digests.digest(&path, true).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, b"other").unwrap();
        File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let changed = digests.digest(&path, true).unwrap().unwrap();
        assert_eq!(changed, Sha256::digest(b"other").to_vec());
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"third").unwrap();
        File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let replaced = digests.digest(&path, true).unwrap().unwrap();
        assert_eq!(replaced, Sha256::digest(b"third").to_vec());
        assert_ne!(first, changed);
        digests.save().unwrap();
        assert_eq!(read_index(&memo.join(DIGESTS_FILE)).unwrap().len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn an_unsettled_input_is_hashed_every_time_and_never_kept() {
        TEST_SETTLE.with(|s| s.set(Some(Duration::from_secs(3600))));
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("result.json");
        let memo = dir.path().join("digests");
        std::fs::write(&path, b"just written").unwrap();
        let digests = Digests::open(&memo, &memo).unwrap();
        assert_eq!(
            digests.digest(&path, true).unwrap().unwrap(),
            Sha256::digest(b"just written").to_vec()
        );
        assert!(!kept(&digests, &path));
        digests.save().unwrap();
        assert!(!memo.join(DIGESTS_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn earlier_receipts_are_reused_and_a_corrupt_one_or_index_fails_closed() {
        settled();
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        let memo = dir.path().join("digests");
        std::fs::create_dir_all(&memo).unwrap();
        std::fs::write(&path, b"first").unwrap();
        // A receipt as collectors before the index wrote it, saying a digest the file can't have.
        let identity = FileIdentity::of_path(&path).unwrap();
        let mut key = Sha256::new();
        field(&mut key, b"quality-input-digest-v1");
        field(&mut key, path.as_os_str().as_encoded_bytes());
        field(&mut key, &serde_json::to_vec(&identity).unwrap());
        let key = hex(&key.finalize());
        let rows = serde_json::to_string(&InputDigest {
            identity,
            sha256: vec![9; 32],
        })
        .unwrap();
        let mut envelope = Envelope {
            fingerprint: key.clone(),
            rows_sha256: hex(&Sha256::digest(rows.as_bytes())),
            rows,
        };
        let receipt = memo.join(format!("{key}.json"));
        std::fs::write(&receipt, serde_json::to_vec(&envelope).unwrap()).unwrap();
        let digests = Digests::open(&memo, &memo).unwrap();
        assert_eq!(digests.digest(&path, true).unwrap().unwrap(), vec![9; 32]);
        assert!(kept(&digests, &path));
        digests.save().unwrap();
        envelope.rows.push(' ');
        let corrupt = serde_json::to_vec(&envelope).unwrap();
        std::fs::write(&receipt, &corrupt).unwrap();
        let fresh = dir.path().join("fresh");
        assert!(Digests::open(&fresh, &memo)
            .unwrap()
            .digest(&path, true)
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
        assert_eq!(std::fs::read(&receipt).unwrap(), corrupt);
        let index = memo.join(DIGESTS_FILE);
        let mut bytes = std::fs::read(&index).unwrap();
        let at = bytes.len() - 3;
        bytes[at] ^= 1;
        std::fs::write(&index, &bytes).unwrap();
        assert!(Digests::open(&memo, &memo).is_err());
        assert_eq!(std::fs::read(&index).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn saves_merge_with_another_runs_and_drop_conflicting_digests() {
        settled();
        let dir = tempfile::tempdir_in(".").unwrap();
        let memo = dir.path().join("digests");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let one = Digests::open(&memo, &memo).unwrap();
        let other = Digests::open(&memo, &memo).unwrap();
        one.digest(&a, true).unwrap();
        other.digest(&b, true).unwrap();
        one.save().unwrap();
        other.save().unwrap();
        let index = read_index(&memo.join(DIGESTS_FILE)).unwrap();
        assert_eq!(index.len(), 2);
        // The same identity saved with another digest is trusted by neither.
        forge(&memo, &a, &[1; 32]);
        let late = Digests::open(&memo, &memo).unwrap();
        late.known.borrow_mut().clear();
        late.digest(&a, true).unwrap();
        late.save().unwrap();
        assert!(!read_index(&memo.join(DIGESTS_FILE))
            .unwrap()
            .contains_key(&FileIdentity::of_path(&a).unwrap().key()));
    }

    #[tokio::test]
    async fn final_guard_detects_same_size_input_change_despite_restored_mtime() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        collect(&opts).unwrap();
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let canonical = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let key = cache.key(&unit, &job, &canonical).unwrap();
        cache.track(&unit, &canonical, &key);
        let path = unit.join(super::super::PAGE_FILE);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(cache
            .verify()
            .unwrap_err()
            .to_string()
            .contains("QUALITY inputs changed"));
    }

    #[tokio::test]
    async fn bundled_qwen_dependencies_are_shared_and_invalidate_every_affected_page() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        let other = super::super::units(&opts.root).join("other");
        let mut peer = crate::quality::work::tests::unit(&other);
        peer.doc_id = "other".into();
        peer.id = peer.identity().unwrap();
        std::fs::write(
            other.join(super::super::JOB_FILE),
            serde_json::to_vec(&peer).unwrap(),
        )
        .unwrap();
        let mut sources = Vec::new();
        for (i, doc) in [&job.doc_id, &peer.doc_id].into_iter().enumerate() {
            let path = dir.path().join(format!("qwen-stage-{i}"));
            let mut q = crate::qwen::stage::local_unit(
                &unit.join("page.jpg"),
                &unit.join("loc.xml"),
                &path,
                crate::qwen::stage::tests::engine(),
                1,
                4,
                600,
                4,
            )
            .unwrap();
            q.doc_id = doc.clone();
            q.id = q.identity().unwrap();
            std::fs::write(
                path.join(crate::qwen::JOB_FILE),
                serde_json::to_vec(&q).unwrap(),
            )
            .unwrap();
            sources.push(path);
        }
        let bundled = crate::qwen::stage::write_bundle(
            &super::super::units(&opts.qwen_root).join("bundle"),
            &sources,
        )
        .unwrap();
        let before = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        assert_eq!(before.qwen.len(), 2);
        assert_eq!(before.qwen_dependencies.len(), 1);
        let canonical = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let a = before.key(&unit, &job, &canonical).unwrap();
        let b = before.key(&other, &peer, &canonical).unwrap();
        std::fs::create_dir_all(&opts.qwen_spool).unwrap();
        std::fs::write(
            opts.qwen_spool
                .join(format!("{}__file_diag.txt", bundled.workunit())),
            b"late",
        )
        .unwrap();
        assert!(before.verify_readings().is_err());
        let after = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        assert_ne!(after.key(&unit, &job, &canonical).unwrap(), a);
        assert_ne!(after.key(&other, &peer, &canonical).unwrap(), b);
    }

    #[tokio::test]
    async fn final_quality_guard_revalidates_earlier_hits_and_spool_markers() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        collect(&opts).unwrap();
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let canonical = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let key = cache.key(&unit, &job, &canonical).unwrap();
        assert!(cache.load(&key).unwrap().is_some());
        cache.track(&unit, &canonical, &key);
        std::fs::write(
            unit.join("ours.xml"),
            std::fs::read(unit.join("loc.xml")).unwrap(),
        )
        .unwrap();
        assert!(cache
            .verify()
            .unwrap_err()
            .to_string()
            .contains("QUALITY inputs changed"));
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let key = cache.key(&unit, &job, &canonical).unwrap();
        cache.track(&unit, &canonical, &key);
        std::fs::write(opts.spool.join(format!("{}_error", job.workunit())), b"").unwrap();
        assert!(cache
            .verify()
            .unwrap_err()
            .to_string()
            .contains("QUALITY inputs changed"));
    }

    #[tokio::test]
    async fn nested_surya_late_canonical_and_failure_markers_refresh_cached_joins() {
        use usnm_ocr_contract::surya::{InputMode, Outcome};
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, job) = cached_fixture(dir.path()).await;
        let sroot = dir.path().join("surya");
        let spool = dir.path().join("surya-spool");
        opts.surya_root = Some(sroot.clone());
        opts.surya_spool = Some(spool.clone());
        let crops = crate::qwen::stage::cut(
            &std::fs::read(unit.join("page.jpg")).unwrap(),
            &std::fs::read(unit.join("loc.xml")).unwrap(),
            job.crop_seed,
            job.k_crops,
        )
        .unwrap();
        let mut base = crate::surya::work::tests::job();
        base.doc_id = job.doc_id;
        let crop = crate::surya::collect::tests::crop_job(
            &base,
            &crops[0].id,
            crops[0].rect,
            &crops[0].stratum,
        );
        crate::surya::collect::tests::unit_with_result(
            &sroot,
            &spool,
            &crop,
            &[(InputMode::Whole, Outcome::Ok, "a late Surya reading")],
        );
        let original = crate::surya::unit_dirs(&sroot).unwrap().remove(0);
        let nested = super::super::units(&sroot).join("loc/moved-crop");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::rename(original, &nested).unwrap();
        let missing = spool.join(format!(
            "{}__file_{}",
            crop.workunit(),
            InputMode::Whole.text()
        ));
        let bytes = std::fs::read(&missing).unwrap();
        std::fs::remove_file(&missing).unwrap();
        let pending = collect(&opts).unwrap();
        assert_eq!(pending.summary.with_surya, 0);
        std::fs::write(missing, bytes).unwrap();
        let joined = collect(&opts).unwrap();
        assert_eq!(joined.summary.with_surya, 1);
        assert!(joined
            .crops
            .iter()
            .any(|r| r.reader == super::super::collect::SURYA));
        for suffix in ["error", "no_output_files"] {
            let marker = spool.join(format!("{}_{suffix}", crop.workunit()));
            std::fs::write(&marker, b"").unwrap();
            let skipped = collect(&opts).unwrap();
            assert_eq!(skipped.summary.with_surya, 0);
            assert!(skipped
                .crops
                .iter()
                .all(|r| r.reader != super::super::collect::SURYA));
            let fresh = collect(&Options {
                cache: None,
                ..opts.clone()
            })
            .unwrap();
            assert_eq!(skipped.crops, fresh.crops);
            std::fs::remove_file(marker).unwrap();
            assert_eq!(collect(&opts).unwrap().crops, joined.crops);
        }
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let sample = super::super::units(&sroot).join("sample/synthetic/page");
        std::fs::create_dir_all(&sample).unwrap();
        std::fs::copy(
            nested.join(crate::surya::JOB_FILE),
            sample.join(crate::surya::JOB_FILE),
        )
        .unwrap();
        assert!(cache
            .verify()
            .unwrap_err()
            .to_string()
            .contains("inventory changed"));
        assert_eq!(surya_jobs(&sroot).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn input_canonical_and_collector_version_changes_invalidate_rows() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, mut job) = cached_fixture(dir.path()).await;
        let files = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let mut cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let first = cache.key(&unit, &job, &files).unwrap();
        collect(&opts).unwrap();
        assert!(cache.load(&first).unwrap().is_some());
        std::fs::write(
            unit.join("ours.xml"),
            std::fs::read(unit.join("loc.xml")).unwrap(),
        )
        .unwrap();
        let sidecar = cache.key(&unit, &job, &files).unwrap();
        assert_ne!(first, sidecar);
        collect(&opts).unwrap();
        let texts = unit.join("texts.json");
        let mut bytes = std::fs::read(&texts).unwrap();
        bytes.push(b'\n');
        std::fs::write(&texts, bytes).unwrap();
        assert_ne!(cache.key(&unit, &job, &files).unwrap(), sidecar);
        collect(&opts).unwrap();
        let result = opts
            .spool
            .join(format!("{}__file_result.json", job.workunit()));
        let mut canonical: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&result).unwrap()).unwrap();
        canonical["host"]["plan_class"] = "changed-canonical".into();
        std::fs::write(result, serde_json::to_vec(&canonical).unwrap()).unwrap();
        let changed = collect(&opts).unwrap();
        assert!(changed
            .pages
            .iter()
            .all(|row| row.plan_class.as_deref() == Some("changed-canonical")));
        let current = cache.key(&unit, &job, &files).unwrap();
        assert!(cache.load(&current).unwrap().is_some());
        cache.version.push(1);
        let updated = cache.key(&unit, &job, &files).unwrap();
        assert_ne!(updated, current);
        assert!(cache.load(&updated).unwrap().is_none());
        // All historical parts are retained, not replaced by the newest dependencies.
        assert_eq!(
            std::fs::read_dir(opts.cache.unwrap())
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_file())
                .count(),
            4
        );
        use usnm_ocr_contract::quality::{BundlePage, BundlePageInputs, BundleSlice};
        let slice = |input: &usnm_ocr_contract::quality::Input| BundleSlice {
            bytes: input.bytes,
            offset: 0,
            sha256: input.sha256.clone(),
            source: input.source.clone(),
        };
        job.pages.push(BundlePage {
            doc_id: "sample:synthetic/page".into(),
            crop_seed: job.crop_seed,
            function_words: job.function_words.clone(),
            group: job.group.clone(),
            k_crops: job.k_crops,
            language: job.language.clone(),
            readers: job.readers.clone(),
            inputs: BundlePageInputs {
                alto: slice(&job.inputs.alto),
                page: slice(&job.inputs.page),
                texts: slice(&job.inputs.texts),
            },
        });
        job.id = job.identity().unwrap();
        std::fs::write(
            unit.join(super::super::JOB_FILE),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        let before = cache.key(&unit, &job, &files).unwrap();
        let sidecar = unit.join("sample:synthetic/page.ours.xml");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(sidecar, b"nested sidecar").unwrap();
        assert_ne!(before, cache.key(&unit, &job, &files).unwrap());
    }

    #[tokio::test]
    async fn corrupt_parts_and_changed_reading_inventory_fail_closed_without_replacing_snapshot() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        run(&opts).await.unwrap();
        let output = opts.out_dir.as_ref().unwrap().join("pages.parquet");
        let published = std::fs::read(&output).unwrap();
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let canonical = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let key = cache.key(&unit, &job, &canonical).unwrap();
        let path = opts.cache.as_ref().unwrap().join(format!("{key}.json"));
        let mut envelope: Envelope =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        envelope.rows.push(' ');
        let corrupted = serde_json::to_vec(&envelope).unwrap();
        std::fs::write(&path, &corrupted).unwrap();
        assert!(run(&opts)
            .await
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        assert_eq!(std::fs::read(&output).unwrap(), published);
        assert_eq!(std::fs::read(&path).unwrap(), corrupted);
        crate::qwen::stage::local_unit(
            &unit.join("page.jpg"),
            &unit.join("loc.xml"),
            &super::super::units(&opts.qwen_root).join("new"),
            crate::qwen::stage::tests::engine(),
            1,
            4,
            600,
            4,
        )
        .unwrap();
        assert!(cache
            .verify()
            .unwrap_err()
            .to_string()
            .contains("inventory changed"));
    }
}
