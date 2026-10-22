//! The Surya 2 test on BOINC volunteers (#516) as a BOINC app, `usnm_surya`.
//!
//! - [`stage`] gets each page's full-size IIIF JPEG (the scan archive first,
//!   then tile.loc.gov one request at a time; see [`crate::scans`]) and
//!   writes one unit directory per page.
//! - [`gold`] stages the OCR gold set's crops as crop units and writes
//!   Surya's reading of them as the hypothesis file `gold.py score` takes.
//! - [`submit`] hands staged units to stock `create_work`, two results each.
//! - [`work`] runs one unit on a volunteer host, CPU or GPU, and always
//!   writes a result and its diagnostics.
//! - [`validate`] is what stock `script_validator` calls. It keeps a copy of
//!   every result it sees and logs every pair it compares, so the probe's
//!   timings and cross-host agreement survive BOINC's choice of a canonical
//!   result.
//! - [`summary`] reads those copies back into one table.
use anyhow::{bail, ensure, Context};
use std::path::{Path, PathBuf};
use usnm_core::page_id::{PageId, Source};

pub mod collect;
pub mod crops;
pub mod gold;
pub mod stage;
pub mod submit;
pub mod summary;
pub mod validate;
pub mod work;

/// The unit's request in the BOINC slot. Its presence makes `usnm-boinc work`
/// run a Surya unit.
pub const JOB_FILE: &str = "surya.json";
pub const RESULT_FILE: &str = "result.json";
pub const DIAG_FILE: &str = "diag.txt";
/// The page image's open name in the slot.
pub const PAGE_FILE: &str = "page.jpg";
pub const APP: &str = "usnm_surya";

/// One page to stage: its round 1 group, ISO 639-3 language, doc id
/// (`<lccn>_<date>_ed-<n>_seq-<n>`) and the batches that may hold it
/// (`<name>_ver<NN>`, joined with `+`, tried in order), given as
/// `group:lang:doc_id:batch[+batch...]`. The batches come from the
/// published set's `batches.json` (docs/boinc-surya.md): staging never asks
/// loc.gov's item API where a page is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub doc_id: String,
    pub group: String,
    pub lang: String,
    pub batches: Vec<(String, u16)>,
}
impl std::str::FromStr for Page {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        let mut it = s.splitn(4, ':');
        let (Some(group), Some(lang), Some(doc_id), Some(batches)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            bail!("a page is group:lang:doc_id:batch[+batch...], got {s:?}");
        };
        crate::assess::check_doc_id(doc_id)?;
        usnm_core::ids::PageKey::from_doc_id(doc_id)
            .with_context(|| format!("{doc_id:?} is not <lccn>_<date>_ed-<n>_seq-<n>"))?;
        let batches = batches
            .split('+')
            .map(|b| {
                crate::scans::split_batch(b)
                    .with_context(|| format!("{b:?} is not a batch as <name>_ver<NN>"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(batches.len() <= 8, "at most 8 batches for a page");
        Ok(Self {
            doc_id: doc_id.to_owned(),
            group: group.to_owned(),
            lang: lang.to_owned(),
            batches,
        })
    }
}

impl Page {
    /// A page of `group` from a row of the quality campaign's sample
    /// (`quality sample`, #968): its batch is the one the prefetch manifest
    /// places it in.
    pub fn from_sample(row: &crate::assess::SampleRow, group: &str) -> anyhow::Result<Self> {
        crate::assess::check_doc_id(&row.doc_id)?;
        check_group(group)?;
        check_word(&row.lang, "language")?;
        ensure!(
            !row.batch.is_empty(),
            "{}: sample row has no batch",
            row.doc_id
        );
        Ok(Self {
            doc_id: row.doc_id.clone(),
            group: group.to_owned(),
            lang: row.lang.clone(),
            batches: vec![(row.batch.clone(), row.batch_version)],
        })
    }
}

/// Where the units live under the staging root.
pub fn units(root: &Path) -> PathBuf {
    root.join("units")
}

/// Marks a campaign whose units live at `units/<source>/<local id>`
/// (ADR-0026), so a LoC page unit and a benchmark sample's unit cannot share
/// a directory. A campaign with units and no marker was staged before, at
/// `units/<id>`, and keeps that layout: a resumed run adds no second copy of
/// a unit it already staged.
pub const LAYOUT_FILE: &str = "units-layout";
const LAYOUT: &[u8] = b"source/local\n";

/// Where a campaign's writers put its units, in the layout it was started
/// with.
#[derive(Debug, Clone)]
pub struct Units {
    dir: PathBuf,
    qualified: bool,
}

impl Units {
    /// Creates `units/` if need be. A campaign with no units yet gets the
    /// qualified layout and its marker.
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        let dir = units(root);
        let marker = root.join(LAYOUT_FILE);
        let qualified = if marker.exists() {
            ensure!(
                crate::read(&marker, 64)? == LAYOUT,
                "unknown unit layout in {}",
                marker.display()
            );
            true
        } else if std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
            false
        } else {
            crate::atomic(&marker, LAYOUT)?;
            true
        };
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, qualified })
    }

    /// Whether units go under `units/<source>/<local id>`.
    pub fn qualified(&self) -> bool {
        self.qualified
    }

    /// The unit of `page`, or with `suffix` the name beside it: a crop unit
    /// (`<page>.<crop id>`) or a marker (`<page>.<crop id>.missing`).
    pub fn path(&self, page: &PageId, suffix: Option<&str>) -> anyhow::Result<PathBuf> {
        let base = if self.qualified {
            page.path()
        } else {
            ensure!(
                page.source() == Source::Loc,
                "{page}: a campaign staged before ADR-0026 holds LoC pages only"
            );
            page.local().to_owned()
        };
        Ok(self.dir.join(match suffix {
            Some(s) => format!("{base}.{s}"),
            None => base,
        }))
    }
}

/// Every unit (a directory holding `surya.json`) of the campaign at `root`,
/// sorted, in either layout: `units/<id>` from before ADR-0026, or
/// `units/<source>/<local id>`, which is two or three levels deep.
pub fn unit_dirs(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let e = e?;
            if !e.file_type()?.is_dir() {
                continue;
            }
            let p = e.path();
            if p.join(JOB_FILE).exists() {
                out.push(p);
            } else if depth > 1 {
                walk(&p, depth - 1, out)?;
            }
        }
        Ok(())
    }
    let dir = units(root);
    let mut out = Vec::new();
    if dir.exists() {
        walk(&dir, 3, &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// A hold written when LoC refused or failed a request, as for the
/// assessment: staging will not start again until a person removes it.
pub fn check_no_hold(root: &Path) -> anyhow::Result<()> {
    crate::assess::check_no_hold(root)
}

/// The output files in output template order.
pub fn outputs() -> [&'static str; 5] {
    use usnm_ocr_contract::surya::InputMode;
    [
        InputMode::Tiled.text(),
        InputMode::Whole.text(),
        InputMode::Capped.text(),
        RESULT_FILE,
        DIAG_FILE,
    ]
}

/// A group as the quality campaign names them: lower-case ASCII letters,
/// digits and hyphens, 1 to 32 bytes.
pub fn check_group(s: &str) -> anyhow::Result<()> {
    usnm_ocr_contract::quality::check_name(s, "group")
}

/// Lower-case ASCII letters and hyphens, as a language.
pub fn check_word(s: &str, what: &str) -> anyhow::Result<()> {
    ensure!(
        !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
        "invalid {what} {s:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_sample_row_becomes_a_page_of_the_group() {
        let row: crate::assess::SampleRow = serde_json::from_value(serde_json::json!({
            "batch": "dlc_arlington", "batch_version": 1, "crash_weight": 1.0,
            "damage": "mid", "damage_rate": 0.3, "decade": 1910,
            "doc_id": "sn85055101_1917-04-03_ed-1_seq-3", "lang": "srp", "mode": "ocr",
            "prefix": "dlc", "stratum_pages": 10, "weight": 1.0, "year": 1917
        }))
        .unwrap();
        let p = Page::from_sample(&row, "campaign-a").unwrap();
        assert_eq!(p.group, "campaign-a");
        assert_eq!(p.lang, "srp");
        assert_eq!(p.batches, vec![("dlc_arlington".to_owned(), 1)]);
        // The quality campaign's groups carry digits.
        assert_eq!(
            Page::from_sample(&row, "campaign-1").unwrap().group,
            "campaign-1"
        );
        assert!(Page::from_sample(&row, "Campaign 1").is_err());
    }

    #[test]
    fn pages_parse() {
        let p: Page = "serbian:srp:sn85055101_1917-04-03_ed-1_seq-3:dlc_arlington_ver01"
            .parse()
            .unwrap();
        assert_eq!(p.group, "serbian");
        assert_eq!(p.batches, vec![("dlc_arlington".to_owned(), 1)]);
        let p: Page =
            "russian:rus:sn82015743_1931-04-10_ed-1_seq-4:iune_diamond_ver02+iune_garnet_ver01"
                .parse()
                .unwrap();
        assert_eq!(p.batches.len(), 2);
        assert!("serbian:srp".parse::<Page>().is_err());
        // No batch: staging has nowhere to look without the item API.
        assert!("serbian:srp:sn85055101_1917-04-03_ed-1_seq-3"
            .parse::<Page>()
            .is_err());
        assert!("serbian:srp:sn85055101_1917-04-03_ed-1_seq-3:dlc_arlington"
            .parse::<Page>()
            .is_err());
        assert!(
            "serbian:srp:sn85055101_1917-04-03_ed1_seq3:dlc_arlington_ver01"
                .parse::<Page>()
                .is_err()
        );
    }

    #[test]
    fn new_campaigns_qualify_units_and_old_ones_keep_their_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let loc: PageId = "sn85055101_1917-04-03_ed-1_seq-3".parse().unwrap();
        let sample: PageId = "sample:overproof-ca/overproof-1".parse().unwrap();

        let new = tmp.path().join("new");
        let units = Units::open(&new).unwrap();
        assert!(units.qualified());
        let page = units.path(&loc, None).unwrap();
        assert_eq!(page, new.join("units/loc/sn85055101_1917-04-03_ed-1_seq-3"));
        assert_eq!(
            units.path(&loc, Some("c01")).unwrap(),
            new.join("units/loc/sn85055101_1917-04-03_ed-1_seq-3.c01")
        );
        let bench = units.path(&sample, None).unwrap();
        assert_eq!(bench, new.join("units/sample/overproof-ca/overproof-1"));
        for unit in [&page, &bench] {
            std::fs::create_dir_all(unit).unwrap();
            std::fs::write(unit.join(JOB_FILE), b"{}").unwrap();
        }
        // A directory without a job (a missing page) is not a unit.
        std::fs::create_dir_all(new.join("units/loc/sn85055101_1917-04-03_ed-1_seq-4")).unwrap();
        assert_eq!(unit_dirs(&new).unwrap(), vec![page, bench]);
        // Reopened, it stays qualified.
        assert!(Units::open(&new).unwrap().qualified());

        // Units from before the marker: the bare layout, LoC pages only.
        let old = tmp.path().join("old");
        let unit = units_dir_with_job(&old, "sn85055101_1917-04-03_ed-1_seq-3");
        let units = Units::open(&old).unwrap();
        assert!(!units.qualified());
        assert!(!old.join(LAYOUT_FILE).exists());
        assert_eq!(units.path(&loc, None).unwrap(), unit);
        assert_eq!(
            units.path(&loc, Some("c01")).unwrap(),
            old.join("units/sn85055101_1917-04-03_ed-1_seq-3.c01")
        );
        assert!(units.path(&sample, None).is_err());
        assert_eq!(unit_dirs(&old).unwrap(), vec![unit]);

        // An unknown marker is refused.
        std::fs::write(new.join(LAYOUT_FILE), b"other\n").unwrap();
        assert!(Units::open(&new).is_err());
    }

    fn units_dir_with_job(root: &Path, name: &str) -> PathBuf {
        let unit = units(root).join(name);
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(JOB_FILE), b"{}").unwrap();
        unit
    }
}
