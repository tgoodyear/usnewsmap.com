//! Text-only units (`usnm_ocr_contract::qe_text`): the quality measure of
//! each page's LoC text, with no image. Each text goes through
//! [`Measure::report`], the one function the crop path's
//! [`Measure::add`] and the server's crop join also call, so a page's
//! numbers are the same whichever way it was measured.
use crate::qe::Measure;
use anyhow::{ensure, Context};
pub use usnm_ocr_contract::qe_text::JOB_FILE;
use usnm_ocr_contract::{
    qe_text::{QeTextJob, QeTextResult, QeTextRow, NO_LANGUAGE, NO_RESOURCE_FILE, VERSION},
    quality::MAX_QE,
};

/// Scores a unit. `qe` is `qe.bin`, present exactly when the job names it.
pub fn score(job: &QeTextJob, qe: Option<&[u8]>) -> anyhow::Result<QeTextResult> {
    job.validate()?;
    let measure = match (&job.qe, qe) {
        (None, _) => None,
        (Some(_), None) => anyhow::bail!("the job names qe.bin and it is missing"),
        (Some(q), Some(b)) => {
            ensure!(
                b.len() <= MAX_QE && q.input.matches(b),
                "input qe checksum mismatch"
            );
            let m = Measure::load(b).context("qe.bin")?;
            ensure!(
                m.serves(&q.language),
                "qe.bin is for {}, the job says {}",
                m.language(),
                q.language
            );
            Some(m)
        }
    };
    let reason = if job.language.is_none() {
        NO_LANGUAGE
    } else {
        NO_RESOURCE_FILE
    };
    let rows = job
        .pages
        .iter()
        .map(|p| match &measure {
            Some(m) => QeTextRow {
                doc_id: p.doc_id.clone(),
                qe: Some(m.report(&p.text)),
                null_reason: None,
            },
            None => QeTextRow {
                doc_id: p.doc_id.clone(),
                qe: None,
                null_reason: Some(reason.to_owned()),
            },
        })
        .collect();
    let r = QeTextResult {
        v: VERSION,
        id: job.id.clone(),
        rows,
    };
    r.check(job)?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use usnm_core::qe;
    use usnm_ocr_contract::{
        qe_text::{QeTextPage, SOURCE_NONE, SOURCE_PAGE},
        quality::{Input, QeInput, ReaderReport},
    };

    const TEXTS: [&str; 3] = [
        "The mayor of Albany spoke with the council about the bridges and roads",
        "Tbe mayor oi Albanv spokc witli tbe cou?cil abont",
        "",
    ];

    /// A small English resource file, packed as the tests in `usnm_core::qe` do.
    fn resources() -> Vec<u8> {
        let train = qe::lm_text(&TEXTS[0].repeat(20), "eng");
        let (entries, vocab) = qe::counts(&train, 5);
        let words: Vec<String> = TEXTS[0]
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        let mut words = words;
        words.sort();
        words.dedup();
        qe::pack(
            words,
            entries,
            &qe::PackOptions {
                language: "eng",
                order: 5,
                vocab,
                prune_min_count: 1,
                prune_from_order: 5,
                hunspell: false,
                licence: "CC0-1.0",
                notice: "test",
            },
        )
        .unwrap()
    }

    fn job(bytes: Option<&[u8]>) -> QeTextJob {
        let mut j = QeTextJob {
            v: 1,
            id: String::new(),
            archive: "x_ver01.tar.bz2".into(),
            group: "qe-loc-v1".into(),
            language: bytes.map(|_| "eng".to_owned()),
            pages: TEXTS
                .iter()
                .enumerate()
                .map(|(i, t)| QeTextPage {
                    doc_id: format!("sn1_1900-01-01_ed-1_seq-{i}"),
                    language_source: if bytes.is_some() {
                        SOURCE_PAGE
                    } else {
                        SOURCE_NONE
                    }
                    .into(),
                    text: (*t).into(),
                })
                .collect(),
            qe: bytes.map(|b| QeInput {
                input: Input::of(b, "lake:qe-resources/qe-v1/eng.qe"),
                language: "eng".into(),
            }),
        };
        j.id = j.identity().unwrap();
        j
    }

    fn reader(text: &str) -> ReaderReport {
        let _ = text;
        ReaderReport {
            chars: 0,
            completeness: None,
            content_found: 0,
            content_total: 0,
            est_cer: None,
            est_cer_low_bound_note: String::new(),
            est_cer_raw: None,
            findable_content: None,
            findable_places: None,
            name: "loc".into(),
            pairs: BTreeMap::new(),
            places_found: 0,
            places_total: 0,
            precision: None,
            source: "loc".into(),
            words: 0,
            qe: None,
        }
    }

    #[test]
    fn the_text_only_path_gives_the_crop_paths_numbers() {
        let bytes = resources();
        let got = score(&job(Some(&bytes)), Some(&bytes)).unwrap();
        // The crop path: `Measure::add` over a unit's reader reports.
        let m = Measure::load(&bytes).unwrap();
        for (row, text) in got.rows.iter().zip(TEXTS) {
            let mut readers = [reader(text)];
            m.add(
                &mut readers,
                &[crate::report::ReaderInput {
                    name: "loc".into(),
                    source: "loc".into(),
                    text: text.into(),
                }],
            );
            assert_eq!(row.qe, readers[0].qe, "{text:?}");
            assert_eq!(row.null_reason, None);
        }
        assert!(
            got.rows[0].qe.as_ref().unwrap().oov.unwrap()
                < got.rows[1].qe.as_ref().unwrap().oov.unwrap()
        );
        assert!(got.rows[2].qe.as_ref().unwrap().no_words);
    }

    #[test]
    fn a_page_without_a_resource_file_has_a_reason() {
        let got = score(&job(None), None).unwrap();
        assert!(got
            .rows
            .iter()
            .all(|r| r.qe.is_none() && r.null_reason.as_deref() == Some(NO_LANGUAGE)));
        let mut j = job(None);
        j.language = Some("ita".into());
        for p in &mut j.pages {
            p.language_source = SOURCE_PAGE.into();
        }
        j.id = j.identity().unwrap();
        let got = score(&j, None).unwrap();
        assert!(got
            .rows
            .iter()
            .all(|r| r.null_reason.as_deref() == Some(NO_RESOURCE_FILE)));
    }

    #[test]
    fn a_resource_file_the_job_does_not_pin_is_refused() {
        let bytes = resources();
        let j = job(Some(&bytes));
        assert!(score(&j, Some(b"not it")).is_err());
        assert!(score(&j, None).is_err());
    }
}
