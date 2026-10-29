//! Derived row parts only: no scientific inputs or historical parts are removed.
use super::collect::{CropRow, Options, PageRow, Summary};
use anyhow::{ensure, Context};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{quality::Job, qwen};

const FORMAT: &[u8] = b"quality-row-part-v2";

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

#[cfg(unix)]
fn input_digest(dir: &Path, path: &Path, input: &mut File) -> anyhow::Result<Vec<u8>> {
    let identity = FileIdentity::read(input)?;
    // Coarse ctime cannot distinguish a same-size rewrite with restored mtime.
    if identity.changed.1 == 0 {
        return contents(input);
    }
    let mut key = Sha256::new();
    field(&mut key, b"quality-input-digest-v1");
    field(&mut key, path.as_os_str().as_encoded_bytes());
    field(&mut key, &serde_json::to_vec(&identity)?);
    let key = hex(&key.finalize());
    let receipt = dir.join(format!("{key}.json"));
    let digest = match std::fs::read(&receipt) {
        Ok(bytes) => {
            let envelope: Envelope = serde_json::from_slice(&bytes)?;
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
                saved.identity == identity && saved.sha256.len() == 32,
                "invalid input digest receipt"
            );
            saved.sha256
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let digest = contents(input)?;
            ensure!(
                FileIdentity::read(input)? == identity,
                "input changed while hashing"
            );
            let rows = serde_json::to_string(&InputDigest {
                identity: identity.clone(),
                sha256: digest.clone(),
            })?;
            let envelope = Envelope {
                fingerprint: key,
                rows_sha256: hex(&Sha256::digest(rows.as_bytes())),
                rows,
            };
            std::fs::create_dir_all(dir)?;
            let mut staged = tempfile::NamedTempFile::new_in(dir)?;
            staged.write_all(&serde_json::to_vec(&envelope)?)?;
            staged.as_file().sync_all()?;
            staged.persist_noclobber(&receipt)?;
            File::open(dir)?.sync_all()?;
            digest
        }
        Err(e) => return Err(e).context("reading retained input digest"),
    };
    ensure!(
        FileIdentity::read(input)? == identity
            && FileIdentity::read(&File::open(path)?)? == identity,
        "input changed while fingerprinting"
    );
    Ok(digest)
}

#[cfg(not(unix))]
fn input_digest(_dir: &Path, _path: &Path, input: &mut File) -> anyhow::Result<Vec<u8>> {
    contents(input)
}

#[cfg(unix)]
fn inventory_identity(path: &Path, input: &File) -> anyhow::Result<Option<Vec<u8>>> {
    let identity = FileIdentity::read(input)?;
    if identity.changed.1 == 0 {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(&identity)?;
    ensure!(
        FileIdentity::read(input)? == identity
            && FileIdentity::read(&File::open(path)?)? == identity,
        "input changed during scheduling inventory"
    );
    Ok(Some(bytes))
}

#[cfg(not(unix))]
fn inventory_identity(_path: &Path, _input: &File) -> anyhow::Result<Option<Vec<u8>>> {
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
    surya: Dependency,
    qwen_root: PathBuf,
    surya_root: Option<PathBuf>,
    inventory: Vec<PathBuf>,
    included: RefCell<BTreeMap<PathBuf, TrackedUnit>>,
}

struct TrackedUnit {
    canonical: Vec<PathBuf>,
    key: String,
    inventory: bool,
}

struct Dependency {
    paths: Vec<PathBuf>,
    digest: Vec<u8>,
}

impl Dependency {
    fn new(mut paths: Vec<PathBuf>) -> anyhow::Result<Self> {
        paths.sort();
        paths.dedup();
        let digest = Self::hash(&paths)?;
        Ok(Self { paths, digest })
    }

    fn hash(paths: &[PathBuf]) -> anyhow::Result<Vec<u8>> {
        let mut hash = Sha256::new();
        for path in paths {
            file(&mut hash, path)?;
        }
        Ok(hash.finalize().to_vec())
    }

    fn verify(&self) -> anyhow::Result<()> {
        ensure!(
            Self::hash(&self.paths)? == self.digest,
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

fn file(hash: &mut Sha256, path: &Path) -> anyhow::Result<()> {
    fingerprint(hash, path, None)
}

fn fingerprint(hash: &mut Sha256, path: &Path, memo: Option<&Path>) -> anyhow::Result<()> {
    fingerprint_input(hash, path, memo, false)
}

fn fingerprint_input(
    hash: &mut Sha256,
    path: &Path,
    memo: Option<&Path>,
    inventory: bool,
) -> anyhow::Result<()> {
    field(hash, path.as_os_str().as_encoded_bytes());
    let mut input = match File::open(path) {
        Ok(input) => input,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            field(hash, b"missing");
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("fingerprinting {path:?}")),
    };
    field(hash, b"present");
    // Scheduling hints never authorize cache reuse or accepted scientific rows.
    if inventory && memo.is_some() {
        if let Some(identity) = inventory_identity(path, &input)? {
            field(hash, b"inventory-identity-v1");
            field(hash, &identity);
            return Ok(());
        }
    }
    let digest = match memo {
        Some(dir) => input_digest(dir, path, &mut input)?,
        None => contents(&mut input)?,
    };
    field(hash, &digest);
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
        let mut version = Sha256::new();
        field(&mut version, FORMAT);
        field(&mut version, opts.campaign.as_bytes());
        // A release/rebuild (including hat or contract changes) invalidates all derived rows.
        file(&mut version, &std::env::current_exe()?)?;
        let mut qwen: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        let mut qwen_dependencies = BTreeMap::new();
        let mut job_bytes = Vec::new();
        let mut inventory = jobs(&opts.qwen_root, crate::qwen::JOB_FILE)?;
        for path in &inventory {
            let bytes = crate::read(path, qwen::MAX_JOB_JSON)?;
            let job: qwen::Job = serde_json::from_slice(&bytes)?;
            let mut paths = vec![path.clone()];
            paths.extend(super::collect::spool_files(
                &opts.qwen_spool,
                &job.workunit(),
                &crate::qwen::outputs(),
            ));
            for doc in std::iter::once(&job.doc_id).chain(job.pages.iter().map(|p| &p.doc_id)) {
                qwen.entry(doc.clone()).or_default().push(path.clone());
            }
            qwen_dependencies.insert(path.clone(), Dependency::new(paths)?);
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
        let surya = Dependency::new(surya)?;
        for (path, bytes) in job_bytes {
            ensure!(
                std::fs::read(path)? == bytes,
                "reading job changed during inventory"
            );
        }
        Ok(Self {
            dir: dir.to_owned(),
            version: version.finalize().to_vec(),
            qwen,
            qwen_dependencies,
            surya,
            qwen_root: opts.qwen_root.clone(),
            surya_root: opts.surya_root.clone(),
            inventory,
            included: RefCell::new(BTreeMap::new()),
        })
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
            "QUALITY job changed during collection"
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
            let memo = self.dir.join("input-digests");
            fingerprint_input(
                &mut hash,
                &path,
                staged_blob.then_some(memo.as_path()),
                inventory,
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
        self.verify_readings()
    }

    pub fn verify_readings(&self) -> anyhow::Result<()> {
        let mut inventory = jobs(&self.qwen_root, crate::qwen::JOB_FILE)?;
        if let Some(root) = &self.surya_root {
            inventory.extend(surya_jobs(root)?);
        }
        ensure!(
            inventory == self.inventory,
            "reading job inventory changed during collection"
        );
        for dep in self.qwen_dependencies.values() {
            dep.verify()?;
        }
        self.surya.verify()
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
        staged.persist_noclobber(&path)?;
        File::open(&self.dir)?.sync_all()?;
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

    #[cfg(unix)]
    #[test]
    fn scheduling_identity_does_not_read_blob_and_rejects_a_replaced_path() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        std::fs::write(&path, b"staged scientific input").unwrap();
        let write_only = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(inventory_identity(&path, &write_only).unwrap().is_some());
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"staged scientific input").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(inventory_identity(&path, &write_only).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cold_inventory_does_not_warm_row_cache_and_detects_restored_mtime_rewrites() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        let cache = Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let canonical = super::super::collect::spool_files(
            &opts.spool,
            &job.workunit(),
            &super::super::outputs(),
        );
        let hint = cache.inventory_key(&unit, &job, &canonical).unwrap();
        assert!(!opts.cache.as_ref().unwrap().join("input-digests").exists());
        assert!(cache.load(&hint).unwrap().is_none());
        cache.verify().unwrap();
        assert!(!opts.cache.as_ref().unwrap().join("input-digests").exists());
        let rows = cache.key(&unit, &job, &canonical).unwrap();
        assert_ne!(hint, rows);
        assert!(opts.cache.as_ref().unwrap().join("input-digests").is_dir());
        let path = unit.join(super::super::PAGE_FILE);
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

    #[cfg(unix)]
    #[test]
    fn input_digest_reuses_persistent_receipt_without_reading_unchanged_blob() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        let memo = dir.path().join("digests");
        std::fs::write(&path, b"staged scientific input").unwrap();
        let first = input_digest(&memo, &path, &mut File::open(&path).unwrap()).unwrap();
        let mut write_only = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert_eq!(input_digest(&memo, &path, &mut write_only).unwrap(), first);
    }

    #[cfg(unix)]
    #[test]
    fn same_size_rewrite_with_restored_mtime_and_replacement_invalidate_input_digest() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        let memo = dir.path().join("digests");
        std::fs::write(&path, b"first").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let first = input_digest(&memo, &path, &mut File::open(&path).unwrap()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, b"other").unwrap();
        File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let changed = input_digest(&memo, &path, &mut File::open(&path).unwrap()).unwrap();
        assert_ne!(changed, first);
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"third").unwrap();
        File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let replaced = input_digest(&memo, &path, &mut File::open(&path).unwrap()).unwrap();
        assert_ne!(replaced, changed);
        assert_ne!(replaced, first);
        assert_eq!(std::fs::read_dir(memo).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn replaced_path_cannot_reuse_an_old_open_descriptor_digest() {
        use std::io::Seek;
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        let memo = dir.path().join("digests");
        std::fs::write(&path, b"first").unwrap();
        let mut original = File::open(&path).unwrap();
        input_digest(&memo, &path, &mut original).unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"other").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        original.rewind().unwrap();
        assert!(input_digest(&memo, &path, &mut original)
            .unwrap_err()
            .to_string()
            .contains("input changed while fingerprinting"));
    }

    #[cfg(unix)]
    #[test]
    fn corrupt_input_digest_fails_closed_and_is_retained() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("page.jpg");
        let memo = dir.path().join("digests");
        std::fs::write(&path, b"first").unwrap();
        input_digest(&memo, &path, &mut File::open(&path).unwrap()).unwrap();
        let receipt = std::fs::read_dir(&memo)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut envelope: Envelope =
            serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        envelope.rows.push(' ');
        let corrupt = serde_json::to_vec(&envelope).unwrap();
        std::fs::write(&receipt, &corrupt).unwrap();
        assert!(input_digest(&memo, &path, &mut File::open(&path).unwrap())
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
        assert_eq!(std::fs::read(receipt).unwrap(), corrupt);
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
