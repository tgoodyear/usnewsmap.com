//! Line-job inputs from local copies (#1621): the page list a line campaign reads, and the
//! staging tree one export of it takes.
//!
//! - [`inventory`] lists every page of LoC's bulk OCR for some titles (unpacked, as
//!   [`super::loc_alto_path`] reads it), with its ALTO's sha256, the lines `export --line-read`
//!   would read ([`super::select_lines`]), whether the frozen test set holds it, and the local
//!   copy of its scan, if there is one: LoC's full-size IIIF JPEG, by sha256. Nothing is
//!   fetched.
//! - [`stage`] writes what one export of up to 100 of those pages, from one batch, needs on the
//!   BOINC host: each scan with a sidecar in the scan fetcher's layout (`keep/`), its ALTO
//!   (`loc-alto/`), and a prefetch manifest of the pages (`manifests/<name>/`). Each file is
//!   checked against the inventory first. `onprem-step.sh --target scans --step receive` takes
//!   the tree as a tar.
use anyhow::{ensure, Context};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::boinc;
use usnm_ocr_contract::prefetch::ManifestRow;
use usnm_ocr_worker::{
    loc::{ImageSize, ScanFormat},
    prefetch,
};

/// Where a staging tree keeps LoC's ALTO, beside `keep/`: `loc-alto/<lccn>/<yyyy>/<mm>/<dd>/
/// ed-<n>/seq-<m>/ocr.xml`.
pub const STAGE_ALTO_DIR: &str = "loc-alto";
/// Where a staging tree keeps its prefetch manifest: `manifests/<name>/manifest.parquet`.
pub const STAGE_MANIFEST_DIR: &str = "manifests";
/// The scan's sidecar `user_agent`: these copies weren't fetched by the scan fetcher.
pub const LOCAL_COPY: &str = "usnm-boinc line-stage (local copy of LoC's IIIF JPEG)";

/// A title's pages in LoC's bulk OCR: its versioned batch (`dlc_misctopsn83020866_ver03`) and
/// the reel segment of its IIIF ids (`print` for the _Cherokee Phoenix_).
#[derive(Debug, Clone, PartialEq)]
pub struct Title {
    pub lccn: String,
    pub batch: String,
    pub reel: String,
}

impl std::str::FromStr for Title {
    type Err = anyhow::Error;

    /// `<lccn>=<versioned batch>:<reel>`.
    fn from_str(s: &str) -> anyhow::Result<Self> {
        let (lccn, rest) = s
            .split_once('=')
            .context("a title is <lccn>=<batch>:<reel>")?;
        let (batch, reel) = rest
            .split_once(':')
            .context("a title is <lccn>=<batch>:<reel>")?;
        let t = Self {
            lccn: lccn.into(),
            batch: batch.into(),
            reel: reel.into(),
        };
        ensure!(
            !t.lccn.is_empty() && t.lccn.bytes().all(|b| b.is_ascii_alphanumeric()),
            "invalid lccn {lccn}"
        );
        ensure!(
            usnm_ocr_contract::message::is_versioned_batch(&t.batch),
            "{batch} is not a versioned batch name"
        );
        ensure!(
            !t.reel.is_empty() && t.reel.bytes().all(|b| b.is_ascii_alphanumeric()),
            "invalid reel {reel}"
        );
        Ok(t)
    }
}

impl Title {
    /// The batch without its version, as the scan fetcher and the queues name it, and the
    /// version.
    fn base(&self) -> anyhow::Result<(String, u16)> {
        let (base, version) = self
            .batch
            .rsplit_once("_ver")
            .context("unversioned batch")?;
        Ok((base.to_owned(), version.parse()?))
    }

    /// The page's IIIF id on tile.loc.gov.
    fn iiif_base(&self, key: &PageKey) -> anyhow::Result<String> {
        let awardee = self
            .batch
            .split('_')
            .next()
            .context("batch has no awardee")?;
        Ok(format!(
            "https://tile.loc.gov/image-services/iiif/service:ndnp:{awardee}:batch_{}:data:{}:{}:{}{:02}:{:04}",
            self.batch,
            key.lccn,
            self.reel,
            key.date.format("%Y%m%d"),
            key.edition,
            key.seq
        ))
    }
}

/// The local copy of a page's scan: LoC's full-size IIIF JPEG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanCopy {
    pub bytes: u64,
    pub sha256: String,
    pub url: String,
}

/// One page of the inventory, a line of its JSONL. Fields in alphabetical order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageEntry {
    pub alto_sha256: String,
    /// The batch without its version (`dlc_misctopsn83020866`).
    pub batch: String,
    pub batch_version: u16,
    pub doc_id: String,
    /// The lines `export --line-read` reads on this page; it skips a page with none.
    pub lines: usize,
    pub page: String,
    /// None when no local copy of the scan was found.
    pub scan: Option<ScanCopy>,
    /// The page is in the frozen Cherokee test set (#1620).
    pub test_set: bool,
}

/// The scan copies in `dirs`, by doc id: files named `<doc_id>.jpg`. Two different copies of
/// one page are refused.
fn local_scans(dirs: &[PathBuf]) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let mut out: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in dirs {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            let Some(doc) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".jpg"))
            else {
                continue;
            };
            if PageKey::from_doc_id(doc).is_err() || !path.is_file() {
                continue;
            }
            if let Some(have) = out.get(doc) {
                ensure!(
                    std::fs::read(have)? == std::fs::read(&path)?,
                    "two different scans of {doc}: {} and {}",
                    have.display(),
                    path.display()
                );
                continue;
            }
            out.insert(doc.to_owned(), path);
        }
    }
    Ok(out)
}

/// Every page of `titles` under `loc_alto`, sorted by page key, with the lines read in
/// `language` and the local copy of its scan in `scans`. `test_pages` are the test set's page
/// keys.
pub fn inventory(
    loc_alto: &Path,
    titles: &[Title],
    scans: &[PathBuf],
    test_pages: &BTreeSet<String>,
    language: &str,
) -> anyhow::Result<Vec<PageEntry>> {
    ensure!(
        boinc::LINE_LANGUAGES.contains(&language),
        "no line route for {language}"
    );
    let copies = local_scans(scans)?;
    let mut out = Vec::new();
    for title in titles {
        let (batch, batch_version) = title.base()?;
        let root = loc_alto.join(&title.lccn);
        for path in walk_alto(&root)? {
            let rel: Vec<String> = path
                .strip_prefix(loc_alto)?
                .iter()
                .map(|c| c.to_string_lossy().into_owned())
                .collect();
            let [lccn, y, m, d, ed, seq, _] = rel.as_slice() else {
                continue;
            };
            let key: PageKey = format!("{lccn}/{y}-{m}-{d}/{ed}/{seq}").parse()?;
            ensure!(
                super::loc_alto_path(loc_alto, &key)? == path,
                "unexpected ALTO path {}",
                path.display()
            );
            let alto = crate::read(&path, boinc::MAX_ALTO)?;
            let scan = match copies.get(&key.doc_id()) {
                Some(p) => {
                    let bytes = crate::read(p, boinc::MAX_SCAN)?;
                    let size = super::scan_size(&bytes)?;
                    let copy = ScanCopy {
                        bytes: bytes.len() as u64,
                        sha256: boinc::sha256(&bytes),
                        url: format!("{}/full/full/0/default.jpg", title.iiif_base(&key)?),
                    };
                    Some((copy, size))
                }
                None => None,
            };
            // Without a scan, the line count is read in the ALTO's own 300 dpi frame.
            let frame = match &scan {
                Some((_, size)) => *size,
                None => alto_frame(&alto)?,
            };
            let lines = super::select_lines(&alto, frame, language)
                .with_context(|| format!("LoC's ALTO for {key}"))?
                .len();
            out.push(PageEntry {
                alto_sha256: boinc::sha256(&alto),
                batch: batch.clone(),
                batch_version,
                doc_id: key.doc_id(),
                lines,
                page: key.to_string(),
                scan: scan.map(|(copy, _)| copy),
                test_set: test_pages.contains(&key.to_string()),
            });
        }
    }
    out.sort_by(|a, b| a.page.cmp(&b.page));
    for w in out.windows(2) {
        ensure!(w[0].page != w[1].page, "{} is listed twice", w[0].page);
    }
    Ok(out)
}

/// The ALTO's page size in pixels at 300 dpi.
fn alto_frame(alto: &[u8]) -> anyhow::Result<(u32, u32)> {
    let page = super::parse_loc(alto)?;
    ensure!(
        page.width > 0.0 && page.height > 0.0,
        "LoC's ALTO has no page size"
    );
    Ok((page.width.round() as u32, page.height.round() as u32))
}

/// Every `.../ed-<n>/seq-<m>/ocr.xml` under `root`, sorted.
fn walk_alto(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && path.file_name().is_some_and(|n| n == "ocr.xml") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Reads an inventory's JSONL.
pub fn read_inventory(path: &Path) -> anyhow::Result<Vec<PageEntry>> {
    let text = String::from_utf8(crate::read(path, 64 << 20)?)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("a line of {}", path.display())))
        .collect()
}

/// What [`stage`] wrote, as the hand-off export's inputs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Staged {
    /// `HANDOFF_BATCH`.
    pub batch: String,
    /// `HANDOFF_MANIFEST`: `<name>/manifest.parquet`.
    pub manifest: String,
    /// `HANDOFF_PAGES`, in order.
    pub pages: Vec<String>,
    /// `HANDOFF_SCANS`, in the pages' order: `keep/...` paths.
    pub scans: Vec<String>,
}

/// Writes the staging tree for `select` (page keys, 1 to 100, of one batch) into `out`, which
/// must not exist yet: each page's scan from `scans` with its sidecar, its ALTO from
/// `loc_alto`, and a prefetch manifest named `manifest_name`. Every file must be the one
/// `inventory` recorded, and every page must have lines to read.
pub fn stage(
    inventory: &[PageEntry],
    select: &BTreeSet<String>,
    scans: &[PathBuf],
    loc_alto: &Path,
    manifest_name: &str,
    out: &Path,
) -> anyhow::Result<Staged> {
    ensure!(
        !select.is_empty() && select.len() <= 100,
        "stage 1 to 100 pages at a time (one export)"
    );
    ensure!(
        manifest_name.len() <= 64
            && manifest_name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            && manifest_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "a manifest name is 1 to 64 of [A-Za-z0-9_-], starting with a letter or digit, as the BOINC host's receive takes"
    );
    ensure!(
        !out.exists(),
        "{} exists; stage into a new directory",
        out.display()
    );
    let by_page: BTreeMap<&str, &PageEntry> =
        inventory.iter().map(|e| (e.page.as_str(), e)).collect();
    let copies = local_scans(scans)?;
    let mut entries = Vec::new();
    for page in select {
        let e = by_page
            .get(page.as_str())
            .with_context(|| format!("{page} is not in the inventory"))?;
        ensure!(
            (1..=boinc::MAX_LINES).contains(&e.lines),
            "{page} has {} lines to read; export would skip it",
            e.lines
        );
        let copy = e
            .scan
            .as_ref()
            .with_context(|| format!("{page} has no local scan"))?;
        entries.push((*e, copy));
    }
    let batches: BTreeSet<(&str, u16)> = entries
        .iter()
        .map(|(e, _)| (e.batch.as_str(), e.batch_version))
        .collect();
    ensure!(
        batches.len() == 1,
        "one export takes one batch; these pages are in {batches:?}"
    );
    let (batch, batch_version) = *batches.iter().next().unwrap();
    ensure!(
        usnm_ocr_contract::message::is_base_batch_name(batch),
        "invalid base batch {batch}"
    );
    let tmp = out.with_extension("partial");
    ensure!(!tmp.exists(), "{} exists; remove it first", tmp.display());
    std::fs::create_dir_all(&tmp)?;
    let mut rows = Vec::new();
    let mut staged = Staged {
        batch: batch.to_owned(),
        manifest: format!("{manifest_name}/manifest.parquet"),
        pages: Vec::new(),
        scans: Vec::new(),
    };
    for (e, copy) in &entries {
        let key: PageKey = e.page.parse()?;
        let src = copies
            .get(&e.doc_id)
            .with_context(|| format!("no local scan of {} among the scan directories", e.page))?;
        let bytes = crate::read(src, boinc::MAX_SCAN)?;
        ensure!(
            bytes.len() as u64 == copy.bytes && boinc::sha256(&bytes) == copy.sha256,
            "{} is not the scan the inventory recorded for {}",
            src.display(),
            e.page
        );
        let alto_src = super::loc_alto_path(loc_alto, &key)?;
        let alto = crate::read(&alto_src, boinc::MAX_ALTO)?;
        ensure!(
            boinc::sha256(&alto) == e.alto_sha256,
            "{} is not the ALTO the inventory recorded for {}",
            alto_src.display(),
            e.page
        );
        let date: NaiveDate = key.date;
        let keep = prefetch::page_path(
            &tmp.join("keep"),
            batch,
            &key.lccn,
            date,
            key.edition,
            key.seq,
            ImageSize::Full,
            ScanFormat::Jpeg,
        );
        let fetched_at = std::fs::metadata(src)?
            .modified()
            .map(chrono::DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        let iiif_base = copy
            .url
            .strip_suffix("/full/full/0/default.jpg")
            .context("the recorded scan URL is not a full-size IIIF JPEG")?;
        let side = prefetch::Sidecar {
            doc_id: e.doc_id.clone(),
            batch: batch.to_owned(),
            batch_version,
            loc_url: copy.url.clone(),
            iiif_base: iiif_base.to_owned(),
            resolution: "full".into(),
            fetched_at,
            bytes: copy.bytes,
            sha256: copy.sha256.clone(),
            download_ms: 0,
            user_agent: LOCAL_COPY.into(),
            format: ScanFormat::Jpeg.name().into(),
            md5: None,
        };
        ensure!(prefetch::matches_sidecar(&bytes, &side));
        crate::atomic(&keep, &bytes)?;
        crate::atomic(
            &prefetch::sidecar_path(&keep),
            &serde_json::to_vec_pretty(&side)?,
        )?;
        crate::atomic(
            &super::loc_alto_path(&tmp.join(STAGE_ALTO_DIR), &key)?,
            &alto,
        )?;
        rows.push(ManifestRow::new(
            &key,
            batch,
            batch_version,
            None,
            "mixed",
            0.0,
        ));
        staged.pages.push(e.page.clone());
        staged.scans.push(
            keep.strip_prefix(&tmp)?
                .to_str()
                .context("non-UTF-8 path")?
                .to_owned(),
        );
    }
    crate::atomic(
        &tmp.join(STAGE_MANIFEST_DIR).join(&staged.manifest),
        &usnm_ocr_contract::prefetch::encode(&rows)?,
    )?;
    std::fs::rename(&tmp, out)?;
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LCCN: &str = "sn83020866";

    fn title() -> Title {
        format!("{LCCN}=dlc_misctopsn83020866_ver03:print")
            .parse()
            .unwrap()
    }

    /// An ALTO tree with three pages: one with Cherokee lines and a local scan, one with
    /// Cherokee lines and no scan, one English only.
    fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
        let alto = dir.join("loc");
        let scans = dir.join("scans");
        std::fs::create_dir_all(&scans).unwrap();
        let english = super::super::tests::LOC_ALTO
            .replace(" LANGUAGE=\"chr\"", "")
            .replace("ᎦᎪᎯ", "The")
            .replace("ᎠᎴ", "and")
            .replace("ᎦᏙᎯ", "end");
        for (page, xml) in [
            (
                "1828-03-06/ed-1/seq-1",
                super::super::tests::LOC_ALTO.to_owned(),
            ),
            (
                "1828-03-06/ed-1/seq-2",
                super::super::tests::LOC_ALTO.to_owned(),
            ),
            ("1828-03-06/ed-1/seq-3", english),
        ] {
            let key: PageKey = format!("{LCCN}/{page}").parse().unwrap();
            let p = super::super::loc_alto_path(&alto, &key).unwrap();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, xml).unwrap();
        }
        let mut jpg = Vec::new();
        image::GrayImage::from_pixel(2000, 2000, image::Luma([255]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        std::fs::write(
            scans.join(format!("{LCCN}_1828-03-06_ed-1_seq-1.jpg")),
            &jpg,
        )
        .unwrap();
        // Not a page's scan: passed over.
        std::fs::write(scans.join("notes.jpg"), b"x").unwrap();
        (alto, scans)
    }

    #[test]
    fn inventories_every_page_with_its_lines_scan_and_test_set_mark() {
        let tmp = tempfile::tempdir().unwrap();
        let (alto, scans) = fixture(tmp.path());
        let test = BTreeSet::from([format!("{LCCN}/1828-03-06/ed-1/seq-2")]);
        let inv = inventory(
            &alto,
            &[title()],
            std::slice::from_ref(&scans),
            &test,
            "chr",
        )
        .unwrap();
        let summary: Vec<(&str, usize, bool, bool)> = inv
            .iter()
            .map(|e| (e.page.as_str(), e.lines, e.scan.is_some(), e.test_set))
            .collect();
        assert_eq!(
            summary,
            [
                ("sn83020866/1828-03-06/ed-1/seq-1", 3, true, false),
                ("sn83020866/1828-03-06/ed-1/seq-2", 3, false, true),
                ("sn83020866/1828-03-06/ed-1/seq-3", 0, false, false),
            ]
        );
        let first = &inv[0];
        assert_eq!(
            (first.batch.as_str(), first.batch_version),
            ("dlc_misctopsn83020866", 3)
        );
        assert_eq!(first.doc_id, "sn83020866_1828-03-06_ed-1_seq-1");
        assert_eq!(
            first.scan.as_ref().unwrap().url,
            "https://tile.loc.gov/image-services/iiif/service:ndnp:dlc:batch_dlc_misctopsn83020866_ver03:data:sn83020866:print:1828030601:0001/full/full/0/default.jpg"
        );
        // A line of the JSONL keeps its keys in alphabetical order.
        let line = serde_json::to_string(first).unwrap();
        let keys: Vec<String> = serde_json::from_str::<serde_json::Value>(&line)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(line.find("\"alto_sha256\"") < line.find("\"test_set\""));
        // Read back as written.
        let path = tmp.path().join("pages.jsonl");
        let text: String = inv
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect();
        std::fs::write(&path, text).unwrap();
        assert_eq!(read_inventory(&path).unwrap(), inv);
        assert!(inventory(&alto, &[title()], &[scans], &test, "eng").is_err());
    }

    #[test]
    fn stages_checked_scans_alto_and_a_manifest_for_one_export() {
        let tmp = tempfile::tempdir().unwrap();
        let (alto, scans) = fixture(tmp.path());
        let inv = inventory(
            &alto,
            &[title()],
            std::slice::from_ref(&scans),
            &BTreeSet::new(),
            "chr",
        )
        .unwrap();
        let page = "sn83020866/1828-03-06/ed-1/seq-1".to_owned();
        let out = tmp.path().join("stage");
        let staged = stage(
            &inv,
            &BTreeSet::from([page.clone()]),
            std::slice::from_ref(&scans),
            &alto,
            "chr-lines-test",
            &out,
        )
        .unwrap();
        assert_eq!(
            staged,
            Staged {
                batch: "dlc_misctopsn83020866".into(),
                manifest: "chr-lines-test/manifest.parquet".into(),
                pages: vec![page.clone()],
                scans: vec!["keep/dlc_misctopsn83020866/sn83020866/1828-03-06/1/1.jpg".into()],
            }
        );
        // The scan is found and checked as export finds it.
        let rows = usnm_ocr_contract::prefetch::decode_where(
            bytes::Bytes::from(
                std::fs::read(out.join("manifests").join(&staged.manifest)).unwrap(),
            ),
            |_| true,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key().unwrap().to_string(), page);
        let (bytes, side) = crate::cached(&out, &rows[0]).unwrap();
        assert_eq!(boinc::sha256(&bytes), inv[0].scan.as_ref().unwrap().sha256);
        assert_eq!(side.user_agent, LOCAL_COPY);
        assert!(side.iiif_base.ends_with(":0001"));
        let key: PageKey = page.parse().unwrap();
        let staged_alto = super::super::loc_alto_path(&out.join(STAGE_ALTO_DIR), &key).unwrap();
        assert_eq!(
            boinc::sha256(&std::fs::read(staged_alto).unwrap()),
            inv[0].alto_sha256
        );
        // Only names the BOINC host's receive takes.
        for bad in ["chr.lines", "-chr", "_chr", ""] {
            assert!(stage(
                &inv,
                &BTreeSet::from([page.clone()]),
                std::slice::from_ref(&scans),
                &alto,
                bad,
                &tmp.path().join("bad-name"),
            )
            .is_err());
        }
        // Not again into the same directory.
        assert!(stage(
            &inv,
            &BTreeSet::from([page.clone()]),
            std::slice::from_ref(&scans),
            &alto,
            "chr-lines-test",
            &out
        )
        .is_err());
        // A page without a scan, without lines or not listed is refused; so is a changed scan.
        for (p, why) in [
            ("sn83020866/1828-03-06/ed-1/seq-2", "no local scan"),
            ("sn83020866/1828-03-06/ed-1/seq-3", "export would skip it"),
            ("sn83020866/1828-03-06/ed-1/seq-9", "not in the inventory"),
        ] {
            let err = stage(
                &inv,
                &BTreeSet::from([p.to_owned()]),
                std::slice::from_ref(&scans),
                &alto,
                "x",
                &tmp.path().join("other"),
            )
            .unwrap_err();
            assert!(err.to_string().contains(why), "{p}: {err}");
        }
        std::fs::write(scans.join(format!("{}.jpg", inv[0].doc_id)), b"changed").unwrap();
        let err = stage(
            &inv,
            &BTreeSet::from([page]),
            &[scans],
            &alto,
            "x",
            &tmp.path().join("changed"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the scan"), "{err}");
    }
}
