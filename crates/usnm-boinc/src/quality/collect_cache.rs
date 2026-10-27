//! Derived row parts only: no scientific inputs or historical parts are removed.
use super::collect::{CropRow, Options, PageRow, Summary};
use anyhow::{ensure, Context};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{quality::Job, qwen};

const FORMAT: &[u8] = b"quality-row-part-v1";

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
    qwen: BTreeMap<String, Dependency>,
    surya: Dependency,
    qwen_root: PathBuf,
    surya_root: Option<PathBuf>,
    inventory: Vec<PathBuf>,
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
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    field(hash, &digest.finalize());
    Ok(())
}

fn jobs(root: &Path, name: &str) -> anyhow::Result<Vec<PathBuf>> {
    let dir = super::units(root);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut paths = std::fs::read_dir(dir)?
        .map(|entry| Ok(entry?.path().join(name)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    paths.retain(|p| p.is_file());
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
                qwen.entry(doc.clone()).or_default().extend(paths.clone());
            }
            job_bytes.push((path.clone(), bytes));
        }
        let mut surya = Vec::new();
        if let (Some(root), Some(spool)) = (&opts.surya_root, &opts.surya_spool) {
            let paths = jobs(root, crate::surya::JOB_FILE)?;
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
            }
        }
        let qwen = qwen
            .into_iter()
            .map(|(doc, paths)| Ok((doc, Dependency::new(paths)?)))
            .collect::<anyhow::Result<_>>()?;
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
            surya,
            qwen_root: opts.qwen_root.clone(),
            surya_root: opts.surya_root.clone(),
            inventory,
        })
    }

    pub fn key(&self, unit: &Path, job: &Job, canonical: &[PathBuf]) -> anyhow::Result<String> {
        let mut hash = Sha256::new();
        field(&mut hash, &self.version);
        ensure!(
            serde_json::from_slice::<Job>(&std::fs::read(unit.join(super::JOB_FILE))?)? == *job,
            "QUALITY job changed during collection"
        );
        // Enumerate optional OCR ALTO sidecars too, so appearing/disappearing files invalidate.
        let mut inputs = std::fs::read_dir(unit)?
            .map(|entry| Ok(entry?.path()))
            .collect::<anyhow::Result<Vec<_>>>()?;
        inputs.retain(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                matches!(
                    n,
                    "quality.json"
                        | "page.jpg"
                        | "loc.xml"
                        | "texts.json"
                        | "places.txt"
                        | "submitted"
                ) || n.ends_with("ours.xml")
            })
        });
        inputs.extend_from_slice(canonical);
        for doc in std::iter::once(&job.doc_id).chain(job.pages.iter().map(|p| &p.doc_id)) {
            if let Some(dep) = self.qwen.get(doc) {
                dep.verify()?;
                field(&mut hash, &dep.digest);
            }
        }
        self.surya.verify()?;
        field(&mut hash, &self.surya.digest);
        inputs.sort();
        inputs.dedup();
        for path in inputs {
            file(&mut hash, &path)?;
        }
        Ok(hex(&hash.finalize()))
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        let mut inventory = jobs(&self.qwen_root, crate::qwen::JOB_FILE)?;
        if let Some(root) = &self.surya_root {
            inventory.extend(jobs(root, crate::surya::JOB_FILE)?);
        }
        ensure!(
            inventory == self.inventory,
            "reading job inventory changed during collection"
        );
        for dep in self.qwen.values() {
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

    #[tokio::test]
    async fn input_canonical_and_collector_version_changes_invalidate_rows() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
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
        assert_eq!(std::fs::read_dir(opts.cache.unwrap()).unwrap().count(), 4);
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
