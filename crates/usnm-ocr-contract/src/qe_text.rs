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
    /// sha256 of `ocr/qe/resources.lock.json` as staged against, kept with the unit so the
    /// collector stores the pin the unit was made with.
    pub lock: String,
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
            &self.lock,
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
        ensure!(is_sha256(&self.lock), "invalid lock digest");
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
    /// What a result must look like whatever its job: one of measures or a reason on every
    /// row, finite numbers, the measure's version.
    pub fn check_shape(&self) -> anyhow::Result<()> {
        ensure!(
            self.v == VERSION && is_sha256(&self.id),
            "bad result header"
        );
        for r in &self.rows {
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
                    ensure!(
                        x.is_finite() && x >= 0.0,
                        "{}: measure out of range",
                        r.doc_id
                    );
                }
                ensure!(
                    q.no_words == (q.words == 0)
                        && q.known3 <= q.words
                        && i64::try_from(q.words).is_ok()
                        && i64::try_from(q.chars).is_ok(),
                    "{}: inconsistent counts",
                    r.doc_id
                );
            }
        }
        Ok(())
    }

    /// The result answers the job: one row per page, in order. A unit with a pinned resource
    /// file has measures on every row, from that file; one without has the reason its job
    /// implies on every row and no measures.
    pub fn check(&self, job: &QeTextJob) -> anyhow::Result<()> {
        ensure!(self.id == job.id, "result is for another job");
        self.check_shape()?;
        ensure!(
            self.rows.len() == job.pages.len(),
            "result has the wrong number of rows"
        );
        let reason = if job.language.is_none() {
            NO_LANGUAGE
        } else {
            NO_RESOURCE_FILE
        };
        for (r, p) in self.rows.iter().zip(&job.pages) {
            ensure!(r.doc_id == p.doc_id, "rows are out of order");
            match (&job.qe, &r.qe) {
                (Some(q), Some(m)) => ensure!(
                    m.resources == q.input.sha256,
                    "{}: measured with another resource file",
                    r.doc_id
                ),
                (Some(_), None) => {
                    anyhow::bail!("{}: the pinned resource file gave no measures", r.doc_id)
                }
                (None, Some(_)) => anyhow::bail!("{}: measures without a resource file", r.doc_id),
                (None, None) => ensure!(
                    r.null_reason.as_deref() == Some(reason),
                    "{}: the reason is not {reason}",
                    r.doc_id
                ),
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
            lock: "a".repeat(64),
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

    #[test]
    fn counts_and_measures_must_be_consistent() {
        let j = job();
        let q = QeReport {
            version: "qe-v1".into(),
            resources: "b".repeat(64),
            words: 3,
            known3: 2,
            chars: 13,
            no_words: false,
            oov: Some(0.25),
            charlm_bpc: Some(2.5),
            lm_oov: Some(4.0),
        };
        let r = |q: QeReport| QeTextResult {
            v: VERSION,
            id: j.id.clone(),
            rows: vec![QeTextRow {
                doc_id: j.pages[0].doc_id.clone(),
                qe: Some(q),
                null_reason: None,
            }],
        };
        r(q.clone()).check_shape().unwrap();
        for bad in [
            QeReport {
                known3: 4,
                ..q.clone()
            },
            QeReport {
                no_words: true,
                ..q.clone()
            },
            QeReport {
                oov: Some(-0.1),
                ..q.clone()
            },
            QeReport {
                words: u64::MAX,
                ..q.clone()
            },
        ] {
            assert!(r(bad).check_shape().is_err());
        }
    }

    #[test]
    fn a_pinned_resource_file_must_give_measures() {
        use crate::quality::Input;
        let mut j = job();
        j.language = Some("eng".into());
        j.pages[0].language_source = SOURCE_PAGE.into();
        j.qe = Some(QeInput {
            input: Input::of(b"qe", "lake:qe-resources/qe-v1/eng.qe"),
            language: "eng".into(),
        });
        j.id = j.identity().unwrap();
        j.validate().unwrap();
        let r = QeTextResult {
            v: VERSION,
            id: j.id.clone(),
            rows: vec![QeTextRow {
                doc_id: j.pages[0].doc_id.clone(),
                qe: None,
                null_reason: Some(NO_RESOURCE_FILE.into()),
            }],
        };
        assert!(r.check(&j).is_err());
    }
}
