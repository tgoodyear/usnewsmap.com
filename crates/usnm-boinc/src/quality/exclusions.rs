//! Qwen readings kept out of the quality tables until their unit is re-read.
//!
//! The rows live in `quality.excluded_qwen_readings`
//! (`scripts/onprem/quality/excluded_qwen_readings.sql`): a `job_id`, and a
//! `crop_id` when only that crop is excluded. `quality collect` reads the live
//! rows (`released_at IS NULL`) through the quality database connection the
//! release uses (`USNM_QUALITY_DSN_FILE`) and builds no `vlm` row from an
//! excluded unit or crop. The unit's crops and every other reader's rows stay.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use usnm_ocr_local::quality_db::ExcludedQwenReading;

/// What is excluded of one unit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobExclusion {
    /// Every reading of the unit.
    pub whole: bool,
    /// Single crops, by the id the unit's `readings.json` gives them (`<doc_id>:<crop>`
    /// in a bundle).
    pub crops: BTreeSet<String>,
}

impl JobExclusion {
    /// Whether the crop `id` of the page `doc_id` is excluded. A bundle's page
    /// names its crops without the page prefix, so both spellings count.
    pub fn hides(&self, doc_id: Option<&str>, id: &str) -> bool {
        self.whole
            || self.crops.contains(id)
            || doc_id.is_some_and(|d| self.crops.contains(&format!("{d}:{id}")))
    }
}

/// The exclusions in force, by job id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exclusions(BTreeMap<String, JobExclusion>);

impl Exclusions {
    pub fn from_rows(rows: impl IntoIterator<Item = ExcludedQwenReading>) -> Self {
        let mut out = BTreeMap::<String, JobExclusion>::new();
        for row in rows {
            let e = out.entry(row.job_id).or_default();
            match row.crop_id {
                None => e.whole = true,
                Some(c) => {
                    e.crops.insert(c);
                }
            }
        }
        Self(out)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The units named: those excluded whole, then those with crops excluded.
    pub fn counts(&self) -> (usize, usize) {
        (
            self.0.values().filter(|e| e.whole).count(),
            self.0.values().filter(|e| !e.whole).count(),
        )
    }

    /// What is excluded of the unit `job_id`, when anything is.
    pub fn job(&self, job_id: &str) -> Option<&JobExclusion> {
        self.0.get(job_id)
    }

    /// A digest of the exclusions, part of the row cache's key so that adding or
    /// releasing one re-keys the rows. None when nothing is excluded, which
    /// leaves the key as it was before exclusions existed.
    pub fn fingerprint(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let mut h = Sha256::new();
        for (job, e) in &self.0 {
            h.update(format!("{job}\t{}\n", e.whole));
            for c in &e.crops {
                h.update(format!("{c}\n"));
            }
            h.update(b"\0");
        }
        Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(job: &str, crop: Option<&str>) -> ExcludedQwenReading {
        ExcludedQwenReading {
            job_id: job.into(),
            wu_id: 1,
            crop_id: crop.map(Into::into),
        }
    }

    #[test]
    fn unit_and_crop_rows_group_by_job() {
        let e = Exclusions::from_rows([row("a", None), row("b", Some("c1")), row("b", Some("c2"))]);
        assert_eq!(e.counts(), (1, 1));
        assert!(e.job("a").unwrap().hides(None, "anything"));
        let b = e.job("b").unwrap();
        assert!(b.hides(None, "c1") && b.hides(None, "c2") && !b.hides(None, "c3"));
        assert!(e.job("z").is_none());
    }

    #[test]
    fn a_bundle_crop_matches_with_or_without_its_page_prefix() {
        let e = Exclusions::from_rows([row("j", Some("doc1:c1"))]);
        let j = e.job("j").unwrap();
        assert!(j.hides(Some("doc1"), "c1") && j.hides(None, "doc1:c1"));
        assert!(!j.hides(Some("doc2"), "c1"));
    }

    #[test]
    fn the_fingerprint_follows_the_set_and_is_absent_when_empty() {
        assert_eq!(Exclusions::default().fingerprint(), None);
        let a = Exclusions::from_rows([row("a", None)]).fingerprint();
        let b = Exclusions::from_rows([row("a", Some("c"))]).fingerprint();
        assert!(a.is_some() && a != b);
        let reordered = Exclusions::from_rows([row("b", None), row("a", None)]).fingerprint();
        assert_eq!(
            reordered,
            Exclusions::from_rows([row("a", None), row("b", None)]).fingerprint()
        );
    }
}
