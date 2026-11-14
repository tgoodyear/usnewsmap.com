//! The quality measure's resource files on the server (`usnm_core::qe`,
//! `ocr/qe/README.md`): the lock that pins them, and a directory that holds
//! them as `<lang>.qe`. `stage --qe-dir` gives each unit of a language with a
//! file the file as `qe.bin`; `collect --qe-dir` measures the crop texts with
//! the same files.
use anyhow::{ensure, Context as _};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use usnm_ocr_contract::quality::{Input, QeInput, MAX_QE};
use usnm_ocr_quality::qe::Measure;

/// `ocr/qe/resources.lock.json`: the one file per language this code ships.
pub const LOCK: &str = include_str!("../../../../ocr/qe/resources.lock.json");

#[derive(Debug, Clone, Deserialize)]
pub struct LockEntry {
    pub bytes: u64,
    pub file: String,
    pub hunspell: bool,
    pub lexicon_words: u64,
    pub licence: String,
    pub model_entries: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Lock {
    pub languages: BTreeMap<String, LockEntry>,
    pub measure: String,
    pub prune_from_order: u32,
    pub prune_min_count: u32,
}

pub fn lock() -> anyhow::Result<Lock> {
    let l: Lock = serde_json::from_str(LOCK).context("ocr/qe/resources.lock.json")?;
    ensure!(
        l.measure == usnm_core::qe::VERSION,
        "the lock is for {}, this code measures {}",
        l.measure,
        usnm_core::qe::VERSION
    );
    Ok(l)
}

/// The lock's language for a job's language code (ISO 639-3, or MARC as
/// some catalogs give it).
pub fn iso3(language: &str) -> &str {
    usnm_ocr_quality::qe::iso3(language)
}

/// A directory of resource files, each checked against the lock when first read.
pub struct Dir {
    root: PathBuf,
    lock: Lock,
    measures: Mutex<BTreeMap<String, Option<Arc<Measure>>>>,
}

impl Dir {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        ensure!(root.is_dir(), "{} is not a directory", root.display());
        Ok(Self {
            root: root.to_owned(),
            lock: lock()?,
            measures: Mutex::new(BTreeMap::new()),
        })
    }

    /// The language's file and its bytes, checked; none for a language the
    /// lock has no file for.
    pub fn file(&self, language: &str) -> anyhow::Result<Option<(PathBuf, Vec<u8>, &LockEntry)>> {
        let Some(e) = self.lock.languages.get(iso3(language)) else {
            return Ok(None);
        };
        let path = self.root.join(&e.file);
        let bytes = crate::read(&path, MAX_QE)?;
        ensure!(
            bytes.len() as u64 == e.bytes && usnm_core::sha256_hex(&bytes) == e.sha256,
            "{} is not the file the lock pins",
            path.display()
        );
        Ok(Some((path, bytes, e)))
    }

    /// The job input for a unit of `language`: the file the lock pins.
    pub fn input(&self, language: &str) -> anyhow::Result<Option<(PathBuf, QeInput)>> {
        Ok(self.file(language)?.map(|(path, bytes, e)| {
            (
                path,
                QeInput {
                    input: Input::of(&bytes, &format!("lake:qe-resources/qe-v1/{}", e.file)),
                    // The file's own code, so a host checks the header against it.
                    language: iso3(language).to_owned(),
                },
            )
        }))
    }

    /// The loaded measure for `language`, read once.
    pub fn measure(&self, language: &str) -> anyhow::Result<Option<Arc<Measure>>> {
        let key = iso3(language).to_owned();
        if let Some(m) = self.measures.lock().expect("qe lock").get(&key) {
            return Ok(m.clone());
        }
        let m = match self.file(language)? {
            Some((_, bytes, _)) => Some(Arc::new(Measure::load(&bytes)?)),
            None => None,
        };
        self.measures
            .lock()
            .expect("qe lock")
            .insert(key, m.clone());
        Ok(m)
    }
}

/// Puts the resource file in a unit directory as `qe.bin`: a hard link
/// where the file system allows one (every unit of a language shares the
/// bytes), else a copy.
pub fn place(src: &Path, unit: &Path) -> anyhow::Result<()> {
    let dst = unit.join(super::QE_FILE);
    if dst.exists() {
        std::fs::remove_file(&dst)?;
    }
    if std::fs::hard_link(src, &dst).is_err() {
        std::fs::copy(src, &dst)
            .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_pins_one_file_per_language_of_the_study() {
        let l = lock().unwrap();
        for lang in [
            "ces", "dan", "deu", "eng", "fin", "fra", "nor", "pol", "rus", "slv", "spa", "srp",
            "swe", "yid",
        ] {
            let e = &l.languages[lang];
            assert_eq!(e.file, format!("{lang}.qe"));
            assert!(usnm_ocr_contract::boinc::is_sha256(&e.sha256));
            assert!(e.bytes > 0 && e.bytes <= MAX_QE as u64);
            assert!(e.licence.starts_with("CC-BY-SA-4.0"));
            // GPL-only dictionaries are not shipped.
            assert!(!e.licence.contains("GPL-") || e.licence.contains("LGPL-"));
        }
        assert!(!l.languages["ces"].hunspell && !l.languages["deu"].hunspell);
        assert_eq!(iso3("cze"), "ces");
        assert_eq!(iso3("srp"), "srp");
    }

    #[test]
    fn a_file_the_lock_does_not_pin_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("eng.qe"), b"not it").unwrap();
        let d = Dir::open(tmp.path()).unwrap();
        assert!(d.input("eng").is_err());
        assert!(d.input("haw").unwrap().is_none());
    }
}
