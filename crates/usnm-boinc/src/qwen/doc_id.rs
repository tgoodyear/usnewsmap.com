//! A `usnm_qwen` job's `doc_id` and the page it names (ADR-0026, #983).
//!
//! Every released `usnm_qwen` worker checks `Job.doc_id` with [`check_doc_id`]: 1 to 128
//! bytes of `[A-Za-z0-9_-]`. A qualified sample id, `sample:<dataset>/<sample id>`, fails
//! that check, so the wire spells a sample in a form those workers already accept, and the
//! job's version stays 1. A `doc_id` is one of:
//!
//! - a LoC page: its bare `doc_id`, as stored;
//! - a dataset sample: `sample-<dataset>_<sample id>`, with the sample id's `_` written
//!   `_5F` and its `.` written `_2E`;
//! - a benchmark crop staged before #983: `benchmark-<sha256 of the crop id>`. The hash
//!   can't be reversed; the campaign's `benchmark.json` maps it back to its crop.
//!
//! The three never collide. A LoC `doc_id` begins with its LCCN, letters and digits up to the
//! first `_`; the other two hold a `-` before any `_`, and `sample-` and `benchmark-` differ.
//! A dataset has no `_`, so a sample's first `_` ends its dataset and every later `_` starts
//! an escape. No wire id holds a `:`, so a bundle's `<doc_id>:<crop id>` still splits at its
//! only `:`. [`DocId::parse`] takes only the canonical spelling, so each page has one wire id.
use anyhow::{bail, ensure, Context};
use usnm_core::page_id::{PageId, Source};
use usnm_ocr_contract::{
    boinc::{is_sha256, sha256},
    quality::check_doc_id,
};

const SAMPLE: &str = "sample-";
const LEGACY_BENCHMARK: &str = "benchmark-";
/// The sample-id characters the wire can't carry, and how it writes them.
const ESCAPES: [(char, &str); 2] = [('_', "_5F"), ('.', "_2E")];

/// What a `usnm_qwen` job's `doc_id` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocId {
    /// A LoC page or a dataset sample.
    Page(PageId),
    /// A benchmark crop staged before #983, known only by the sha256 of its crop id.
    LegacyBenchmark { crop_sha256: String },
}

impl DocId {
    /// The `doc_id` campaigns staged before #983 gave benchmark crop `crop_id`.
    pub fn legacy_benchmark(crop_id: &str) -> Self {
        DocId::LegacyBenchmark {
            crop_sha256: sha256(crop_id.as_bytes()),
        }
    }

    /// Reads a wire `doc_id`, refusing anything not in one of the three forms or not in its
    /// canonical spelling.
    pub fn parse(wire: &str) -> anyhow::Result<Self> {
        check_doc_id(wire)?;
        let id = if let Some(hash) = wire.strip_prefix(LEGACY_BENCHMARK) {
            ensure!(is_sha256(hash), "invalid legacy benchmark doc id {wire:?}");
            DocId::LegacyBenchmark {
                crop_sha256: hash.to_owned(),
            }
        } else if let Some(rest) = wire.strip_prefix(SAMPLE) {
            let (dataset, escaped) = rest
                .split_once('_')
                .with_context(|| format!("sample doc id {wire:?} has no sample id"))?;
            let local = format!("{dataset}/{}", unescape(escaped)?);
            DocId::Page(PageId::new(Source::Sample, &local)?)
        } else {
            DocId::Page(PageId::new(Source::Loc, wire)?)
        };
        ensure!(
            id.wire()? == wire,
            "doc id {wire:?} is not in its canonical spelling"
        );
        Ok(id)
    }

    /// The `doc_id` a job carries for this page. Only `loc` and `sample` pages have one, and
    /// a sample too long for the wire is refused, never shortened.
    pub fn wire(&self) -> anyhow::Result<String> {
        let wire = match self {
            DocId::LegacyBenchmark { crop_sha256 } => {
                ensure!(is_sha256(crop_sha256), "invalid crop sha256");
                format!("{LEGACY_BENCHMARK}{crop_sha256}")
            }
            DocId::Page(page) => match page.source() {
                Source::Loc => page.local().to_owned(),
                Source::Sample => {
                    let (dataset, sample) = page
                        .local()
                        .split_once('/')
                        .context("a sample id is <dataset>/<sample id>")?;
                    format!("{SAMPLE}{dataset}_{}", escape(sample))
                }
                other => bail!("{other} pages have no usnm_qwen doc id"),
            },
        };
        check_doc_id(&wire).with_context(|| format!("{self:?} does not fit a usnm_qwen doc id"))?;
        Ok(wire)
    }

    /// Whether this names benchmark crop `crop_id` of `dataset`: a legacy id by the crop
    /// id's hash (it holds no dataset), a sample by its dataset and id.
    pub fn names_crop(&self, dataset: Option<&str>, crop_id: &str) -> bool {
        match self {
            DocId::LegacyBenchmark { crop_sha256 } => *crop_sha256 == sha256(crop_id.as_bytes()),
            DocId::Page(page) => {
                page.source() == Source::Sample
                    && dataset.is_some_and(|d| page.local() == format!("{d}/{crop_id}"))
            }
        }
    }
}

fn escape(sample: &str) -> String {
    let mut out = String::with_capacity(sample.len());
    for c in sample.chars() {
        match ESCAPES.iter().find(|(raw, _)| *raw == c) {
            Some((_, escaped)) => out.push_str(escaped),
            None => out.push(c),
        }
    }
    out
}

fn unescape(escaped: &str) -> anyhow::Result<String> {
    let mut out = String::with_capacity(escaped.len());
    let mut rest = escaped;
    while let Some(c) = rest.chars().next() {
        if c == '_' {
            let (raw, code) = ESCAPES
                .iter()
                .find(|(_, code)| rest.starts_with(code))
                .with_context(|| format!("unknown escape in sample id {escaped:?}"))?;
            out.push(*raw);
            rest = &rest[code.len()..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const LOC: &str = "sn85055101_1917-04-03_ed-1_seq-3";

    fn page(s: &str) -> DocId {
        DocId::Page(s.parse().unwrap())
    }

    #[test]
    fn each_form_round_trips_through_the_released_check() {
        for (id, wire) in [
            (page(LOC), LOC.to_owned()),
            (
                page("sample:overproof-ca/pub-overproof-ca-001"),
                "sample-overproof-ca_pub-overproof-ca-001".to_owned(),
            ),
            (
                page("sample:bln600/Scan_01.v2.png"),
                "sample-bln600_Scan_5F01_2Ev2_2Epng".to_owned(),
            ),
            (
                DocId::legacy_benchmark("public-001"),
                "benchmark-6f892088f1a6f86391beba14f4f5b382bc3621592e906db75762a7a1db726c61"
                    .to_owned(),
            ),
        ] {
            assert_eq!(id.wire().unwrap(), wire);
            check_doc_id(&wire).unwrap();
            assert_eq!(DocId::parse(&wire).unwrap(), id, "{wire}");
        }
    }

    #[test]
    fn distinct_pages_never_share_a_wire_id() {
        // Same sample in two datasets, a `-` moved across the dataset boundary, and escapes
        // next to the characters they stand for.
        let ids = [
            format!("loc:{LOC}"),
            "sample:a/b-c".into(),
            "sample:a-b/c".into(),
            "sample:a/b_c".into(),
            "sample:a/b.c".into(),
            "sample:a/b_5Fc".into(),
            "sample:a/b_2Ec".into(),
            "sample:a/b__c".into(),
            "sample:bln600/x-1".into(),
            "sample:overproof-ca/x-1".into(),
            "sample:sn85055101/1917-04-03_ed-1_seq-3".into(),
        ];
        let mut seen = BTreeMap::new();
        for s in &ids {
            let id = page(s);
            let wire = id.wire().unwrap();
            assert_eq!(DocId::parse(&wire).unwrap(), id);
            assert!(seen.insert(wire.clone(), s).is_none(), "{s} and {wire}");
        }
        let legacy = DocId::legacy_benchmark("x-1").wire().unwrap();
        assert!(!seen.contains_key(&legacy));
        // No sample spells a LoC page, and no LoC page reads as a sample.
        for wire in seen.keys().filter(|w| w.starts_with(SAMPLE)) {
            assert!(PageId::new(Source::Loc, wire).is_err(), "{wire}");
        }
    }

    #[test]
    fn non_canonical_punctuated_and_foreign_ids_are_refused() {
        for bad in [
            "sample:bln600/x",
            "sample-bln600/x",
            "sample-bln600_x.png",
            "sample-bln600_x/y",
            "sample-bln600_x_41",
            "sample-bln600_x_5f",
            "sample-bln600_x_",
            "sample-bln600_",
            "sample-bln600",
            "sample-_x",
            "sample-BLN600_x",
            "sample-bln600__5Fx",
            "sample-bln600_x_2F..",
            "benchmark-6F892088F1A6F86391BEBA14F4F5B382BC3621592E906DB75762A7A1DB726C61",
            "benchmark-6f892088",
            "pub-bln600-063",
            "overproof-1",
            "SERIALSET-00033_00_00-009-0042-0000_p1",
            "sn85055101_1917-04-03_ed-01_seq-3",
            "",
        ] {
            assert!(DocId::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn other_sources_and_overlong_samples_have_no_wire_id() {
        assert!(page("unt:unt-metadc123456_p12").wire().is_err());
        assert!(page("serialset:SERIALSET-00033_00_00-009-0042-0000_p1")
            .wire()
            .is_err());
        let long = page(&format!("sample:{}/{}", "d".repeat(40), "s".repeat(81)));
        assert!(long.wire().is_err());
        let fits = page(&format!("sample:{}/{}", "d".repeat(40), "s".repeat(80)));
        assert_eq!(fits.wire().unwrap().len(), 128);
    }

    #[test]
    fn a_doc_id_names_only_its_own_crop() {
        let legacy = DocId::legacy_benchmark("pub-bln600-063");
        assert!(legacy.names_crop(None, "pub-bln600-063"));
        assert!(legacy.names_crop(Some("bln600"), "pub-bln600-063"));
        assert!(!legacy.names_crop(None, "pub-bln600-064"));
        let sample = page("sample:bln600/pub-bln600-063");
        assert!(sample.names_crop(Some("bln600"), "pub-bln600-063"));
        assert!(!sample.names_crop(Some("nzz"), "pub-bln600-063"));
        assert!(!sample.names_crop(None, "pub-bln600-063"));
        assert!(!page(LOC).names_crop(Some("bln600"), LOC));
    }
}
