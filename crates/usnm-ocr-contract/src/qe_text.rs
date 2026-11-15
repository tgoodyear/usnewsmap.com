//! Text-only units for the reference-free quality measure (`usnm_core::qe`)
//! on LoC's own OCR text: the `usnm_quality` app scores each page's text
//! with the language's `qe.bin` and reports the measures, with no page
//! image, no ALTO geometry, no crops and no comparison between readers.
//!
//! A unit holds one archive's pages of one language: `qe_text.json` (this
//! job, with the pages' texts) and `qe.bin` when the language has a
//! resource file. The app writes `qe_text_result.json`.
use crate::boinc::{is_sha256, sha256};
use crate::quality::{check_name, QeInput, QeReport};
use anyhow::ensure;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const JOB_FILE: &str = "qe_text.json";
pub const RESULT_FILE: &str = "qe_text_result.json";
/// Pages in one unit.
pub const MAX_PAGES: usize = 10_000;
/// The job file, with every page's text.
pub const MAX_JOB: usize = 256 * 1024 * 1024;
/// One page's text.
pub const MAX_PAGE_TEXT: usize = 4 * 1024 * 1024;
pub const MAX_RESULT: usize = 16 * 1024 * 1024;

/// Why a page has no measures.
pub const NO_LANGUAGE: &str = "no_language";
pub const NO_RESOURCE_FILE: &str = "no_resource_file";
pub const NO_WORDS: &str = "no_words";

/// Where a page's language came from.
pub const SOURCE_PAGE: &str = "page";
pub const SOURCE_TITLE: &str = "title_fallback";
pub const SOURCE_NONE: &str = "none";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QeTextPage {
    pub doc_id: String,
    /// `page`, `title_fallback` or `none`.
    pub language_source: String,
    /// LoC's text of the page, as `usnm_alto::text` gives it.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QeTextJob {
    pub v: u32,
    pub id: String,
    /// The scan archive the pages were read from.
    pub archive: String,
    pub group: String,
    /// ISO 639-3 of every page in the unit; none for pages with no language.
    pub language: Option<String>,
    pub pages: Vec<QeTextPage>,
    /// The language's resource file; none when it has none, or the pages
    /// have no language.
    pub qe: Option<QeInput>,
}

impl QeTextJob {
    pub fn identity(&self) -> anyhow::Result<String> {
        Ok(sha256(&serde_json::to_vec(&(
            self.v,
            &self.archive,
            &self.group,
            &self.language,
            &self.pages,
            &self.qe,
        ))?))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.v == VERSION, "unsupported qe_text job version");
        ensure!(
            is_sha256(&self.id) && self.id == self.identity()?,
            "job id is not its identity"
        );
        check_name(&self.group, "group")?;
        ensure!(
            !self.archive.is_empty()
                && self.archive.len() <= 128
                && self
                    .archive
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid archive"
        );
        ensure!(
            !self.pages.is_empty() && self.pages.len() <= MAX_PAGES,
            "a unit holds 1 to {MAX_PAGES} pages"
        );
        ensure!(
            self.qe.is_none() || self.language.is_some(),
            "qe.bin without a language"
        );
        if let (Some(q), Some(l)) = (&self.qe, &self.language) {
            ensure!(
                &q.language == l,
                "qe.bin is for another language than the unit"
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.pages {
            ensure!(
                !p.doc_id.is_empty()
                    && p.doc_id.len() <= 128
                    && p.doc_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
                "invalid doc id"
            );
            ensure!(seen.insert(&p.doc_id), "duplicate doc id {}", p.doc_id);
            ensure!(p.text.len() <= MAX_PAGE_TEXT, "page text over size limit");
            let ok = match p.language_source.as_str() {
                SOURCE_PAGE | SOURCE_TITLE => self.language.is_some(),
                SOURCE_NONE => self.language.is_none(),
                _ => false,
            };
            ensure!(
                ok,
                "language_source {:?} does not fit the unit",
                p.language_source
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QeTextRow {
    pub doc_id: String,
    /// The page's measures; none when `null_reason` says why.
    pub qe: Option<QeReport>,
    pub null_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QeTextResult {
    pub v: u32,
    /// The job's id.
    pub id: String,
    pub rows: Vec<QeTextRow>,
}

impl QeTextResult {
    /// The result answers the job: one row per page, in order, each with
    /// measures or a reason.
    pub fn check(&self, job: &QeTextJob) -> anyhow::Result<()> {
        ensure!(
            self.v == VERSION && self.id == job.id,
            "result is for another job"
        );
        ensure!(
            self.rows.len() == job.pages.len(),
            "result has the wrong number of rows"
        );
        for (r, p) in self.rows.iter().zip(&job.pages) {
            ensure!(r.doc_id == p.doc_id, "rows are out of order");
            ensure!(
                r.qe.is_some() != r.null_reason.is_some(),
                "{}: measures or a reason, not both or neither",
                r.doc_id
            );
            if let Some(q) = &r.qe {
                ensure!(
                    q.version == "qe-v1" && is_sha256(&q.resources),
                    "{}: bad measure header",
                    r.doc_id
                );
                for x in [q.oov, q.charlm_bpc, q.lm_oov].into_iter().flatten() {
                    ensure!(x.is_finite(), "{}: non-finite measure", r.doc_id);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn job() -> QeTextJob {
        let mut j = QeTextJob {
            v: VERSION,
            id: String::new(),
            archive: "x_ver01.tar.bz2".into(),
            group: "qe-loc-v1".into(),
            language: None,
            pages: vec![QeTextPage {
                doc_id: "sn1_1900-01-01_ed-1_seq-1".into(),
                language_source: SOURCE_NONE.into(),
                text: "text".into(),
            }],
            qe: None,
        };
        j.id = j.identity().unwrap();
        j
    }

    #[test]
    fn a_job_is_checked() {
        let mut j = job();
        j.validate().unwrap();
        j.pages[0].language_source = SOURCE_PAGE.into();
        assert!(
            j.validate().is_err(),
            "a page language needs a unit language"
        );
        j.id = "0".repeat(64);
        assert!(j.validate().is_err());
    }

    #[test]
    fn a_result_needs_a_reason_or_measures() {
        let j = job();
        let mut r = QeTextResult {
            v: VERSION,
            id: j.id.clone(),
            rows: vec![QeTextRow {
                doc_id: j.pages[0].doc_id.clone(),
                qe: None,
                null_reason: Some(NO_LANGUAGE.into()),
            }],
        };
        r.check(&j).unwrap();
        r.rows[0].null_reason = None;
        assert!(r.check(&j).is_err());
    }
}
