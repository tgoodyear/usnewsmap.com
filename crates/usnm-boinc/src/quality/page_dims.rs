//! The pixel size of each page of a bundle unit, kept beside its
//! `quality.json` as `dims.json` (#1743).
//!
//! `quality collect` needs a bundle page's image only for its width and
//! height: `parse_alto` scales the ALTO to them and the crop recipe cuts in
//! those pixels. Stage records them when it packs the bundle, and
//! [`backfill`] records them for bundles packed before, from the JPEG
//! headers alone. Each page's entry carries the sha256 of its slice of
//! `page.jpg`, the one `quality.json` binds, so a record is only used with
//! the job it was made from.
//!
//! With the record in place collect reads no image, and the unit's
//! `page.jpg` (a JPEG that staging re-encoded from the scans, so derived
//! data) can be pruned once the unit is collected. A pruned unit can no
//! longer be submitted again, cut into crops by `qwen stage` or
//! `surya stage-crops` from `units/`, or run through `quality estimate`;
//! those stop with [`require_image`]'s error instead of reading nothing.
use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{Job, MAX_JOB_JSON, MAX_SCAN},
};

use super::{units, JOB_FILE, PAGE_FILE};

/// The record's file name in a bundle unit.
pub const DIMS_FILE: &str = "dims.json";
/// The record's format version.
pub const VERSION: u32 = 1;
/// Far more than 500 pages' entries need.
const MAX_DIMS_JSON: usize = 1 << 20;

/// Every page's size, in the bundle's page order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageDims {
    /// The bundle job's id.
    pub job: String,
    pub pages: Vec<PageDim>,
    pub v: u32,
}

/// One page: its doc id, size in pixels and the sha256 of its slice.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageDim {
    pub doc_id: String,
    pub height: u32,
    pub sha256: String,
    pub width: u32,
}

/// A JPEG's width and height from its header, without decoding it.
pub fn header_dims(bytes: &[u8]) -> anyhow::Result<(u32, u32)> {
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()
        .context("reading the image header")?;
    ensure!(w > 0 && h > 0, "image header gives an empty page");
    Ok((w, h))
}

impl PageDims {
    /// The record of `job`'s pages from its whole `page.jpg` in memory.
    /// Each slice must match the job's hash.
    pub fn of_blob(job: &Job, blob: &[u8]) -> anyhow::Result<Self> {
        ensure!(!job.pages.is_empty(), "{}: not a bundle", job.doc_id);
        let mut pages = Vec::with_capacity(job.pages.len());
        for bp in &job.pages {
            let slice = bp.inputs.page.extract(blob, "page")?;
            let (width, height) =
                header_dims(slice).with_context(|| format!("{}: page.jpg", bp.doc_id))?;
            pages.push(PageDim {
                doc_id: bp.doc_id.clone(),
                height,
                sha256: bp.inputs.page.sha256.clone(),
                width,
            });
        }
        Ok(Self {
            job: job.id.clone(),
            pages,
            v: VERSION,
        })
    }

    /// The record of `job`'s pages from the bundle's `page.jpg` at `path`,
    /// one slice in memory at a time. Each slice must match the job's hash.
    /// With `decode`, each page is also fully decoded and must have the
    /// size its header gives.
    pub fn of_file(job: &Job, path: &Path, decode: bool) -> anyhow::Result<Self> {
        ensure!(!job.pages.is_empty(), "{}: not a bundle", job.doc_id);
        let mut file =
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        ensure!(
            len == job.inputs.page.bytes,
            "{}: {len} bytes, quality.json says {}",
            path.display(),
            job.inputs.page.bytes
        );
        let mut pages = Vec::with_capacity(job.pages.len());
        for bp in &job.pages {
            let s = &bp.inputs.page;
            ensure!(
                s.bytes as usize <= MAX_SCAN,
                "{}: page slice over size limit",
                bp.doc_id
            );
            let mut slice = vec![0; s.bytes as usize];
            file.seek(SeekFrom::Start(s.offset))?;
            file.read_exact(&mut slice)
                .with_context(|| format!("{}: reading its page.jpg slice", bp.doc_id))?;
            ensure!(
                sha256(&slice) == s.sha256,
                "{}: page.jpg slice checksum mismatch at offset {}",
                bp.doc_id,
                s.offset
            );
            let (width, height) =
                header_dims(&slice).with_context(|| format!("{}: page.jpg", bp.doc_id))?;
            if decode {
                use image::GenericImageView as _;
                let full = image::load_from_memory(&slice)
                    .with_context(|| format!("{}: decoding page.jpg", bp.doc_id))?
                    .dimensions();
                ensure!(
                    full == (width, height),
                    "{}: header says {width}x{height}, the decoded image is {}x{}",
                    bp.doc_id,
                    full.0,
                    full.1
                );
            }
            pages.push(PageDim {
                doc_id: bp.doc_id.clone(),
                height,
                sha256: s.sha256.clone(),
                width,
            });
        }
        Ok(Self {
            job: job.id.clone(),
            pages,
            v: VERSION,
        })
    }

    /// Whether this record belongs to `job`: the same id, and the same
    /// pages in the same order with the same slice hashes.
    pub fn check(&self, job: &Job) -> anyhow::Result<()> {
        ensure!(self.v == VERSION, "dims.json version {} unknown", self.v);
        ensure!(self.job == job.id, "dims.json is for another job");
        ensure!(
            self.pages.len() == job.pages.len(),
            "dims.json has {} pages, the job {}",
            self.pages.len(),
            job.pages.len()
        );
        for (d, bp) in self.pages.iter().zip(&job.pages) {
            ensure!(
                d.doc_id == bp.doc_id && d.sha256 == bp.inputs.page.sha256,
                "dims.json entry {} does not match the job's page {}",
                d.doc_id,
                bp.doc_id
            );
            ensure!(
                d.width > 0 && d.height > 0,
                "dims.json gives {} an empty size",
                d.doc_id
            );
        }
        Ok(())
    }

    /// The size of page `i` of the job.
    pub fn get(&self, i: usize) -> Option<(u32, u32)> {
        self.pages.get(i).map(|d| (d.width, d.height))
    }

    pub fn write(&self, unit: &Path) -> anyhow::Result<()> {
        atomic(
            &unit.join(DIMS_FILE),
            &serde_json::to_vec_pretty(self).context("dims.json")?,
        )
    }
}

/// The unit's record when it has one, checked against `job`. A record that
/// does not parse or belongs to another job is an error, not `None`.
pub fn load(unit: &Path, job: &Job) -> anyhow::Result<Option<PageDims>> {
    let path = unit.join(DIMS_FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let dims: PageDims = serde_json::from_slice(&read(&path, MAX_DIMS_JSON)?)
        .with_context(|| format!("{}", path.display()))?;
    dims.check(job)
        .with_context(|| format!("{}", path.display()))?;
    Ok(Some(dims))
}

/// Fails, saying why, when the unit's `page.jpg` was pruned after
/// collection (only `dims.json` is left) and `what` needs the image.
pub fn require_image(unit: &Path, what: &str) -> anyhow::Result<()> {
    if !unit.join(PAGE_FILE).try_exists()? && unit.join(DIMS_FILE).try_exists()? {
        anyhow::bail!(
            "{}: page.jpg was pruned after collection and only dims.json is left; \
             {what} needs the image, so restage the pages instead (#1743)",
            unit.display()
        );
    }
    Ok(())
}

/// What [`backfill`] does.
#[derive(Debug, Clone)]
pub struct BackfillOptions {
    /// The campaign's staging root (holds units/).
    pub root: PathBuf,
    /// Only these unit names; all bundle units when empty.
    pub units: Vec<String>,
    /// At most this many records written (or, in a dry run, computed).
    pub max_units: usize,
    /// Compute, write nothing.
    pub dry_run: bool,
    /// Also read the image of units that already have a record and
    /// require the same sizes.
    pub check: bool,
    /// Also fully decode every page and require the header's size.
    pub decode: bool,
}

/// What [`backfill`] found, by unit.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BackfillSummary {
    pub bundles: usize,
    /// Units already holding a record that matches their job.
    pub present: usize,
    /// Of those, units whose image was read again and gave the same sizes.
    pub checked: usize,
    pub written: usize,
    /// Units left for a later run by --max-units.
    pub deferred: usize,
    /// Units with no record and no image: nothing to record from.
    pub no_image: usize,
    /// Units that failed: a slice that doesn't match its hash, an unreadable
    /// header, a record for another job or with other sizes.
    pub failed: usize,
    pub single_page: usize,
    pub pages: usize,
}

/// Records the page sizes of bundle units staged before stage wrote them.
/// Reads each page's slice once (for its hash) and only its header for the
/// size. Every unit is tried; the failures are printed and counted.
pub fn backfill(opts: &BackfillOptions) -> anyhow::Result<BackfillSummary> {
    let dir = units(&opts.root);
    let mut names: Vec<String> = if opts.units.is_empty() {
        std::fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect()
    } else {
        opts.units.clone()
    };
    names.sort();
    names.dedup();
    let mut s = BackfillSummary::default();
    for name in names {
        ensure!(
            usnm_store::is_safe_segment(&name),
            "invalid unit name {name:?}"
        );
        let unit = dir.join(&name);
        if !unit.join(JOB_FILE).is_file() {
            ensure!(opts.units.is_empty(), "{}: no quality.json", unit.display());
            continue;
        }
        let outcome = (|| -> anyhow::Result<()> {
            let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)
                .context("quality.json")?;
            if job.pages.is_empty() {
                s.single_page += 1;
                return Ok(());
            }
            s.bundles += 1;
            let has_image = unit.join(PAGE_FILE).try_exists()?;
            if let Some(dims) = load(&unit, &job)? {
                s.present += 1;
                s.pages += dims.pages.len();
                if opts.check && has_image {
                    let again = PageDims::of_file(&job, &unit.join(PAGE_FILE), opts.decode)?;
                    ensure!(again == dims, "dims.json differs from the image's headers");
                    s.checked += 1;
                }
                return Ok(());
            }
            if !has_image {
                s.no_image += 1;
                anyhow::bail!("no dims.json and no page.jpg");
            }
            if s.written >= opts.max_units {
                s.deferred += 1;
                return Ok(());
            }
            let dims = PageDims::of_file(&job, &unit.join(PAGE_FILE), opts.decode)?;
            if !opts.dry_run {
                dims.write(&unit)?;
            }
            s.written += 1;
            s.pages += dims.pages.len();
            Ok(())
        })();
        if let Err(e) = outcome {
            s.failed += 1;
            eprintln!("{}: {e:#}", unit.display());
        }
    }
    Ok(s)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::quality::work::tests::synthetic_jpeg;

    /// Two staged page units of different sizes packed into a bundle under
    /// `root`'s units/, as stage packs them. Returns the bundle's dir and job.
    pub(crate) fn bundle(root: &Path) -> (PathBuf, Job) {
        let pages = root.join("pages");
        let mut dirs = Vec::new();
        for (doc, (w, h)) in [
            ("sn83045462_1890-05-03_ed-1_seq-1", (1000, 1400)),
            ("sn83045462_1890-05-03_ed-1_seq-2", (1200, 1680)),
        ] {
            let dir = pages.join(doc);
            let base = crate::quality::work::tests::unit(&dir);
            let texts: usnm_ocr_contract::quality::Texts =
                serde_json::from_slice(&std::fs::read(dir.join(super::super::TEXTS_FILE)).unwrap())
                    .unwrap();
            let alto = std::fs::read(dir.join(super::super::ALTO_FILE)).unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            super::super::stage::write_unit(&super::super::stage::WriteUnit {
                dir: &dir,
                doc_id: doc,
                group: &base.group,
                language: &base.language,
                page: &synthetic_jpeg(w, h),
                page_source: "local:page.jpg",
                alto: &alto,
                alto_source: "local:loc.xml",
                texts: &texts,
                texts_source: "local:texts.json",
                places: "Albany\nTroy\n",
                places_source: "local:places.txt",
                engine: base.engine.clone(),
                function_words: base.function_words.clone(),
                crop_seed: base.crop_seed,
                k_crops: base.k_crops,
                qe: None,
            })
            .unwrap();
            dirs.push(dir);
        }
        let unit = units(root).join("bundle-00001-sn83045462_1890-05-03_ed-1_seq-1");
        let job = super::super::stage::write_bundle(&unit, &dirs).unwrap();
        (unit, job)
    }

    #[test]
    fn header_sizes_are_the_decoded_sizes() {
        use image::GenericImageView as _;
        for (w, h) in [(1000, 1400), (1200, 1680), (333, 77)] {
            let jpeg = synthetic_jpeg(w, h);
            assert_eq!(header_dims(&jpeg).unwrap(), (w, h));
            assert_eq!(image::load_from_memory(&jpeg).unwrap().dimensions(), (w, h));
        }
        assert!(header_dims(b"not an image").is_err());
    }

    #[test]
    fn stage_writes_the_record_and_backfill_writes_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let (unit, job) = bundle(tmp.path());
        let staged = load(&unit, &job).unwrap().unwrap();
        assert_eq!(staged.get(0), Some((1000, 1400)));
        assert_eq!(staged.get(1), Some((1200, 1680)));
        assert_eq!(staged.job, job.id);
        // The record binds the slices quality.json binds.
        for (d, bp) in staged.pages.iter().zip(&job.pages) {
            assert_eq!(d.sha256, bp.inputs.page.sha256);
        }
        let from_file = PageDims::of_file(&job, &unit.join(PAGE_FILE), true).unwrap();
        assert_eq!(from_file, staged);

        // A bundle packed before stage wrote records: backfill writes the
        // same one; a dry run and --max-units 0 write nothing.
        std::fs::remove_file(unit.join(DIMS_FILE)).unwrap();
        let mut opts = BackfillOptions {
            root: tmp.path().to_owned(),
            units: vec![],
            max_units: usize::MAX,
            dry_run: true,
            check: false,
            decode: true,
        };
        let s = backfill(&opts).unwrap();
        assert_eq!((s.bundles, s.written, s.pages, s.failed), (1, 1, 2, 0));
        assert!(!unit.join(DIMS_FILE).exists());
        opts.dry_run = false;
        opts.max_units = 0;
        assert_eq!(backfill(&opts).unwrap().deferred, 1);
        assert!(!unit.join(DIMS_FILE).exists());
        opts.max_units = usize::MAX;
        assert_eq!(backfill(&opts).unwrap().written, 1);
        assert_eq!(load(&unit, &job).unwrap().unwrap(), staged);
        opts.check = true;
        let s = backfill(&opts).unwrap();
        assert_eq!((s.present, s.checked, s.written, s.failed), (1, 1, 0, 0));
    }

    #[test]
    fn a_record_for_other_bytes_or_sizes_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (unit, job) = bundle(tmp.path());
        let good = load(&unit, &job).unwrap().unwrap();

        let mut other = good.clone();
        other.pages[1].sha256 = "0".repeat(64);
        other.write(&unit).unwrap();
        assert!(load(&unit, &job).is_err());

        let mut other = good.clone();
        other.job = "f".repeat(64);
        other.write(&unit).unwrap();
        assert!(load(&unit, &job).is_err());

        let mut other = good.clone();
        other.pages.pop();
        other.write(&unit).unwrap();
        assert!(load(&unit, &job).is_err());

        // Bound to the job but with another size: --check reads the image
        // and fails the unit.
        let mut other = good.clone();
        other.pages[0].width += 1;
        other.write(&unit).unwrap();
        let s = backfill(&BackfillOptions {
            root: tmp.path().to_owned(),
            units: vec![],
            max_units: usize::MAX,
            dry_run: false,
            check: true,
            decode: false,
        })
        .unwrap();
        assert_eq!((s.present, s.checked, s.failed), (1, 0, 1));

        // A page.jpg that is not the one quality.json binds: no record.
        std::fs::remove_file(unit.join(DIMS_FILE)).unwrap();
        let mut bytes = std::fs::read(unit.join(PAGE_FILE)).unwrap();
        let n = bytes.len();
        bytes[n - 10] ^= 0xff;
        std::fs::write(unit.join(PAGE_FILE), &bytes).unwrap();
        assert!(PageDims::of_file(&job, &unit.join(PAGE_FILE), false).is_err());
    }

    #[test]
    fn a_pruned_unit_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let (unit, _) = bundle(tmp.path());
        require_image(&unit, "this").unwrap();
        std::fs::remove_file(unit.join(PAGE_FILE)).unwrap();
        let e = require_image(&unit, "Surya crop staging")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("pruned") && e.contains("Surya crop staging"),
            "{e}"
        );
        // No record either: not a pruned unit, the caller's own read fails.
        std::fs::remove_file(unit.join(DIMS_FILE)).unwrap();
        require_image(&unit, "this").unwrap();
        let s = backfill(&BackfillOptions {
            root: tmp.path().to_owned(),
            units: vec![],
            max_units: usize::MAX,
            dry_run: false,
            check: false,
            decode: false,
        })
        .unwrap();
        assert_eq!((s.no_image, s.failed), (1, 1));
    }
}
