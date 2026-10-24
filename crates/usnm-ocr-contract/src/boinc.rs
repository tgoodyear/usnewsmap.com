//! File-based transport for stock BOINC. Identity covers the selected page,
//! scan bytes and processing specification; no credentials or LoC text.
#[cfg(feature = "pipeline")]
use crate::message::OcrMessage;
#[cfg(feature = "pipeline")]
use anyhow::{bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;
pub const MAX_JSON: usize = 16 * 1024;
pub const MAX_TEXT: usize = 512 * 1024;
pub const MAX_ALTO: usize = 32 * 1024 * 1024;
pub const MAX_SCAN: usize = 128 * 1024 * 1024;

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Words as score.py counts them, close enough for agreement between two
/// hosts: lower case, letters and digits only.
fn words(text: &str) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for w in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        *out.entry(w.to_owned()).or_insert(0) += 1;
    }
    out
}

/// 2 * shared words / (words of both), 1.0 for two empty texts.
pub fn dice(a: &str, b: &str) -> f64 {
    let (wa, wb) = (words(a), words(b));
    let total: usize = wa.values().sum::<usize>() + wb.values().sum::<usize>();
    if total == 0 {
        return 1.0;
    }
    let shared: usize = wa
        .iter()
        .map(|(w, n)| (*n).min(*wb.get(w).unwrap_or(&0)))
        .sum();
    2.0 * shared as f64 / total as f64
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    pub url: String,
    pub iiif_base: String,
    pub sha256: String,
    pub bytes: u64,
    pub md5: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    #[serde(
        default,
        skip_serializing_if = "crate::routing::EngineKind::is_tesseract"
    )]
    pub kind: crate::routing::EngineKind,
    /// Full digest of the released BOINC worker image, not the base image.
    pub image: String,
    pub version: String,
    pub profile: String,
    /// Same profile hash as usnm_core::ocr_engine (sha256:12 hex).
    pub models: String,
    pub psm: u8,
    pub dpi: u16,
    pub timeout_secs: u32,
}

#[cfg(feature = "pipeline")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub v: u32,
    pub id: String,
    pub message: OcrMessage,
    pub batch_version: u16,
    pub scan: Scan,
    pub engine: Engine,
}
#[cfg(feature = "pipeline")]
impl Job {
    pub fn identity(&self) -> anyhow::Result<String> {
        Ok(sha256(&serde_json::to_vec(&(
            self.v,
            &self.message.page,
            &self.message.batch,
            &self.message.language,
            self.message.reason,
            self.message.score,
            self.batch_version,
            &self.scan,
            &self.engine,
        ))?))
    }
    pub fn workunit(&self) -> String {
        format!("usnm_{}", self.id)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.v == VERSION, "unsupported BOINC job version");
        self.message.validate()?;
        ensure!(
            self.engine.kind == crate::routing::EngineKind::for_language(&self.message.language),
            "engine does not match language route"
        );
        ensure!(
            self.batch_version > 0 && self.batch_version <= 32767,
            "invalid batch version"
        );
        ensure!(
            is_sha256(&self.id) && self.id == self.identity()?,
            "job identity mismatch"
        );
        ensure!(is_sha256(&self.scan.sha256), "invalid scan checksum");
        ensure!(
            self.scan.bytes > 0 && self.scan.bytes <= MAX_SCAN as u64,
            "invalid scan size"
        );
        for url in [&self.scan.url, &self.scan.iiif_base] {
            ensure!(
                url.starts_with("https://tile.loc.gov/")
                    && url.len() <= 512
                    && !url.chars().any(|c| c.is_control() || c.is_whitespace()),
                "invalid scan URL"
            );
        }
        if let Some(md5) = &self.scan.md5 {
            ensure!(
                md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid scan md5"
            );
        }
        ensure!(
            self.engine
                .image
                .strip_prefix("sha256:")
                .is_some_and(is_sha256),
            "worker image must be a full digest"
        );
        ensure!(
            self.engine
                .models
                .strip_prefix("sha256:")
                .is_some_and(|s| s.len() == 12 && s.bytes().all(|c| c.is_ascii_hexdigit())),
            "invalid profile digest"
        );
        let tag = self.engine_tag()?;
        ensure!(tag.pinned(), "unpinned engine");
        ensure!(
            self.engine.psm == 3 && self.engine.dpi == 300,
            "pilot uses PSM 3 and DPI 300"
        );
        ensure!(
            (1..=3600).contains(&self.engine.timeout_secs),
            "invalid page deadline"
        );
        Ok(())
    }
    pub fn engine_tag(&self) -> anyhow::Result<usnm_core::ocr_engine::OcrEngineTag> {
        use usnm_core::ocr_engine::{Digest, Fields, OcrEngineTag};
        let image = self
            .engine
            .image
            .strip_prefix("sha256:")
            .ok_or_else(|| anyhow::anyhow!("invalid image digest"))?;
        Ok(OcrEngineTag::new(Fields {
            family: self.engine.kind.family(),
            version: &self.engine.version,
            profile: &self.engine.profile,
            image: Digest::short(image)?,
            models: Digest::short(&self.engine.models)?,
            route: "boinc",
            route_version: 1,
            contract: 0,
        })?)
    }
}

#[cfg(feature = "pipeline")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultManifest {
    pub v: u32,
    pub job_id: String,
    pub scan_sha256: String,
    pub engine: Engine,
    pub text_sha256: String,
    pub alto_sha256: String,
    pub elapsed_ms: u64,
    pub completed_at: String,
}
#[cfg(feature = "pipeline")]
impl ResultManifest {
    pub fn matches(&self, job: &Job, text: &[u8], alto: &[u8]) -> anyhow::Result<()> {
        job.validate()?;
        chrono::DateTime::parse_from_rfc3339(&self.completed_at)?;
        ensure!(
            self.v == VERSION
                && self.job_id == job.id
                && self.scan_sha256 == job.scan.sha256
                && self.engine == job.engine,
            "returned job/provenance mismatch"
        );
        ensure!(
            text.len() <= MAX_TEXT && alto.len() <= MAX_ALTO,
            "result over size limit"
        );
        ensure!(
            self.text_sha256 == sha256(text) && self.alto_sha256 == sha256(alto),
            "returned output checksum mismatch"
        );
        if std::str::from_utf8(text).is_err() {
            bail!("text is not UTF-8");
        }
        Ok(())
    }
}

/// Several pages in one `usnm_ocr` workunit (#1059). Each page keeps its own [`Job`]; the
/// unit carries their scans back to back in `pages` and this manifest as `ocr_bundle.json`.
pub mod bundle {
    #[cfg(feature = "pipeline")]
    use super::{is_sha256, sha256, Job, ResultManifest, MAX_ALTO, MAX_TEXT};
    #[cfg(feature = "pipeline")]
    use anyhow::{ensure, Context};
    use serde::{Deserialize, Serialize};

    pub const VERSION: u32 = 1;
    /// The unit's manifest, next to `pages`.
    pub const JOB_FILE: &str = "ocr_bundle.json";
    /// The scans, back to back.
    pub const PAGES_FILE: &str = "pages";
    /// The outputs: one entry per page, the texts and the ALTO files back to back.
    pub const RESULTS_FILE: &str = "results.json";
    pub const TEXTS_FILE: &str = "texts";
    pub const LAYOUTS_FILE: &str = "layouts";
    /// The per-page checkpoint in the slot directory.
    pub const CHECKPOINT_FILE: &str = "ocr_checkpoint.json";
    pub const MAX_PAGES: usize = 32;
    pub const MAX_JSON: usize = 1024 * 1024;
    pub const MAX_PAGES_BYTES: usize = 512 * 1024 * 1024;
    pub const MAX_TEXTS: usize = MAX_PAGES * 512 * 1024;
    /// The ALTO files of one unit together (`usnm_ocr_bundle_out`'s limit). A page whose ALTO
    /// would go past it is returned as an error and requeued alone.
    pub const MAX_LAYOUTS: usize = 256 * 1024 * 1024;
    pub const MAX_ERROR: usize = 1024;
    /// Workunit name prefix; `^usnm_[0-9a-f]{64}$` (a single page) never matches it.
    pub const WORKUNIT_PREFIX: &str = "usnm_ocrb_";

    /// One page of a bundle: its job, and where its scan starts in `pages`.
    #[cfg(feature = "pipeline")]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Page {
        pub job: Job,
        pub offset: u64,
    }

    #[cfg(feature = "pipeline")]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Bundle {
        pub v: u32,
        pub id: String,
        pub pages: Vec<Page>,
    }

    /// The bundle id: the version and the member job ids, in order.
    pub fn identity<S: AsRef<str>>(ids: &[S]) -> anyhow::Result<String> {
        let ids: Vec<&str> = ids.iter().map(AsRef::as_ref).collect();
        Ok(super::sha256(&serde_json::to_vec(&(VERSION, ids))?))
    }

    pub fn workunit(id: &str) -> String {
        format!("{WORKUNIT_PREFIX}{id}")
    }

    #[cfg(feature = "pipeline")]
    impl Bundle {
        /// Lays `jobs` out back to back, in the order given.
        pub fn new(jobs: Vec<Job>) -> anyhow::Result<Self> {
            let ids: Vec<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
            let id = identity(&ids)?;
            let mut offset = 0u64;
            let mut pages = Vec::with_capacity(jobs.len());
            for job in jobs {
                let bytes = job.scan.bytes;
                pages.push(Page { job, offset });
                offset = offset.checked_add(bytes).context("bundle size overflow")?;
            }
            let bundle = Self {
                v: VERSION,
                id,
                pages,
            };
            bundle.validate()?;
            Ok(bundle)
        }
        pub fn workunit(&self) -> String {
            workunit(&self.id)
        }
        /// The size of `pages`.
        pub fn bytes(&self) -> u64 {
            self.pages.iter().map(|p| p.job.scan.bytes).sum()
        }
        pub fn validate(&self) -> anyhow::Result<()> {
            ensure!(self.v == VERSION, "unsupported OCR bundle version");
            ensure!(
                (2..=MAX_PAGES).contains(&self.pages.len()),
                "a bundle holds 2 to {MAX_PAGES} pages"
            );
            let ids: Vec<&str> = self.pages.iter().map(|p| p.job.id.as_str()).collect();
            ensure!(
                is_sha256(&self.id) && self.id == identity(&ids)?,
                "bundle identity mismatch"
            );
            let first = &self.pages[0].job;
            let mut seen = std::collections::BTreeSet::new();
            let mut offset = 0u64;
            for page in &self.pages {
                page.job.validate()?;
                ensure!(seen.insert(&page.job.id), "a page is in the bundle twice");
                ensure!(
                    page.job.engine.kind == crate::routing::EngineKind::Tesseract,
                    "bundles are Tesseract pages only"
                );
                ensure!(
                    page.job.engine.image == first.engine.image
                        && page.job.engine.version == first.engine.version
                        && page.job.engine.timeout_secs == first.engine.timeout_secs
                        && page.job.message.reason == first.message.reason,
                    "a bundle's pages share one image, engine version, deadline and queue"
                );
                ensure!(
                    page.offset == offset,
                    "bundle scan offsets are not contiguous"
                );
                offset += page.job.scan.bytes;
            }
            ensure!(
                offset <= MAX_PAGES_BYTES as u64,
                "bundle scans over size limit"
            );
            Ok(())
        }
        /// Page `index`'s scan out of `pages`, checked against its job.
        pub fn scan<'a>(&self, index: usize, pages: &'a [u8]) -> anyhow::Result<&'a [u8]> {
            let page = self.pages.get(index).context("no such bundle page")?;
            let slice = Slice {
                offset: page.offset,
                bytes: page.job.scan.bytes,
            }
            .of(pages)?;
            ensure!(
                sha256(slice) == page.job.scan.sha256,
                "input scan checksum mismatch"
            );
            Ok(slice)
        }
    }

    /// A byte range of `texts`, `layouts` or `pages`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Slice {
        pub offset: u64,
        pub bytes: u64,
    }
    impl Slice {
        pub fn of<'a>(&self, container: &'a [u8]) -> anyhow::Result<&'a [u8]> {
            let start = usize::try_from(self.offset)?;
            let end = start
                .checked_add(usize::try_from(self.bytes)?)
                .ok_or_else(|| anyhow::anyhow!("slice range overflow"))?;
            anyhow::ensure!(end <= container.len(), "slice out of bounds");
            Ok(&container[start..end])
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Outcome {
        Ok,
        Error,
    }

    /// One page's entry in `results.json`.
    #[cfg(feature = "pipeline")]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PageResult {
        pub job_id: String,
        pub outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub error: Option<String>,
        /// The page's own result manifest, as a single-page unit returns it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub result: Option<ResultManifest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub text: Option<Slice>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub alto: Option<Slice>,
    }

    #[cfg(feature = "pipeline")]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Results {
        pub v: u32,
        pub bundle_id: String,
        pub pages: Vec<PageResult>,
    }

    /// What a bundle result says about one page.
    #[cfg(feature = "pipeline")]
    #[derive(Debug, Clone, PartialEq)]
    pub enum PageOutput {
        /// The page's `result.json`, `text.txt` and `layout.xml`, checked against its job.
        Ok {
            result: Vec<u8>,
            text: Vec<u8>,
            alto: Vec<u8>,
        },
        /// The page failed in the bundle; requeue it alone.
        Failed(String),
    }

    #[cfg(feature = "pipeline")]
    impl Results {
        /// The outputs for `job` (a page of `bundle`), or why it has none. An error here means
        /// the files themselves are unusable, which fails every page.
        pub fn page(
            &self,
            bundle: &Bundle,
            job: &Job,
            texts: &[u8],
            layouts: &[u8],
        ) -> anyhow::Result<PageOutput> {
            self.page_by(
                bundle,
                job,
                |s| Ok(s.of(texts)?.to_vec()),
                |s| Ok(s.of(layouts)?.to_vec()),
            )
        }

        /// [`Results::page`], reading only the page's own ranges of `texts` and `layouts`
        /// through `text` and `alto`, so one page's spool-out needn't load the whole bundle's.
        /// A range over the per-page limit is refused before it is read.
        pub fn page_by(
            &self,
            bundle: &Bundle,
            job: &Job,
            mut text: impl FnMut(Slice) -> anyhow::Result<Vec<u8>>,
            mut alto: impl FnMut(Slice) -> anyhow::Result<Vec<u8>>,
        ) -> anyhow::Result<PageOutput> {
            ensure!(
                self.v == VERSION && self.bundle_id == bundle.id,
                "bundle result is for another bundle"
            );
            ensure!(
                self.pages.len() <= bundle.pages.len(),
                "bundle result has more pages than its bundle"
            );
            ensure!(
                bundle.pages.iter().any(|p| p.job == *job),
                "job is not in this bundle"
            );
            let Some(entry) = self.pages.iter().find(|p| p.job_id == job.id) else {
                return Ok(PageOutput::Failed("no entry in the bundle result".into()));
            };
            if entry.outcome == Outcome::Error {
                let mut why = entry.error.clone().unwrap_or_else(|| "error".into());
                // Volunteer text: cut at a character boundary, never inside one.
                if why.len() > MAX_ERROR {
                    let mut end = MAX_ERROR;
                    while !why.is_char_boundary(end) {
                        end -= 1;
                    }
                    why.truncate(end);
                }
                return Ok(PageOutput::Failed(why));
            }
            let mut checked = || -> anyhow::Result<(ResultManifest, Vec<u8>, Vec<u8>)> {
                let result = entry.result.clone().context("ok entry without a result")?;
                let (t, a) = (
                    entry.text.context("ok entry without text")?,
                    entry.alto.context("ok entry without ALTO")?,
                );
                ensure!(
                    t.bytes <= MAX_TEXT as u64 && a.bytes <= MAX_ALTO as u64,
                    "result over size limit"
                );
                let (t, a) = (text(t)?, alto(a)?);
                result.matches(job, &t, &a)?;
                Ok((result, t, a))
            };
            match checked() {
                Ok((result, text, alto)) => Ok(PageOutput::Ok {
                    result: serde_json::to_vec_pretty(&result)?,
                    text,
                    alto,
                }),
                Err(e) => Ok(PageOutput::Failed(format!("returned page rejected: {e}"))),
            }
        }
    }

    #[cfg(all(test, feature = "pipeline"))]
    mod tests {
        use super::*;

        pub(crate) fn job(n: u8) -> Job {
            let mut job: Job =
                serde_json::from_str(include_str!("fixtures/boinc-tesseract.json")).unwrap();
            let scan = vec![n; 10 + n as usize];
            job.message.page = format!("sn12345678/1900-01-01/ed-1/seq-{}", n + 1);
            job.scan.bytes = scan.len() as u64;
            job.scan.sha256 = sha256(&scan);
            job.id = job.identity().unwrap();
            job.validate().unwrap();
            job
        }

        fn scans(b: &Bundle) -> Vec<u8> {
            b.pages
                .iter()
                .flat_map(|p| {
                    let n = (p.job.scan.bytes - 10) as u8;
                    vec![n; p.job.scan.bytes as usize]
                })
                .collect()
        }

        #[test]
        fn bundles_lay_scans_out_back_to_back_and_check_them() {
            let b = Bundle::new(vec![job(1), job(2), job(3)]).unwrap();
            assert_eq!(
                b.pages.iter().map(|p| p.offset).collect::<Vec<_>>(),
                [0, 11, 23]
            );
            assert_eq!(b.bytes(), 36);
            assert!(b.workunit().starts_with("usnm_ocrb_"));
            let pages = scans(&b);
            assert_eq!(b.scan(1, &pages).unwrap(), &[2u8; 12][..]);
            let mut bad = pages.clone();
            bad[12] ^= 1;
            assert!(b.scan(1, &bad).is_err());
            // Order is identity; a single page is not a bundle; a page only once.
            let other = Bundle::new(vec![job(2), job(1), job(3)]).unwrap();
            assert_ne!(other.id, b.id);
            assert!(Bundle::new(vec![job(1)]).is_err());
            assert!(Bundle::new(vec![job(1), job(1)]).is_err());
            let mut mixed = job(4);
            mixed.message.reason = crate::message::Reason::User;
            mixed.id = mixed.identity().unwrap();
            assert!(Bundle::new(vec![job(1), mixed]).is_err());
            let mut tampered = b.clone();
            tampered.pages[1].offset += 1;
            assert!(tampered.validate().is_err());
        }

        #[test]
        fn results_give_each_page_its_own_outputs_or_a_reason() {
            let b = Bundle::new(vec![job(1), job(2)]).unwrap();
            let (j1, j2) = (&b.pages[0].job, &b.pages[1].job);
            let manifest = |j: &Job, text: &[u8], alto: &[u8]| ResultManifest {
                v: super::super::VERSION,
                job_id: j.id.clone(),
                scan_sha256: j.scan.sha256.clone(),
                engine: j.engine.clone(),
                text_sha256: sha256(text),
                alto_sha256: sha256(alto),
                elapsed_ms: 5,
                completed_at: "2026-10-24T12:00:00Z".into(),
            };
            let results = Results {
                v: VERSION,
                bundle_id: b.id.clone(),
                pages: vec![
                    PageResult {
                        job_id: j1.id.clone(),
                        outcome: Outcome::Ok,
                        error: None,
                        result: Some(manifest(j1, b"one", b"<a/>")),
                        text: Some(Slice {
                            offset: 0,
                            bytes: 3,
                        }),
                        alto: Some(Slice {
                            offset: 0,
                            bytes: 4,
                        }),
                    },
                    PageResult {
                        job_id: j2.id.clone(),
                        outcome: Outcome::Error,
                        error: Some("tesseract failed".into()),
                        result: None,
                        text: None,
                        alto: None,
                    },
                ],
            };
            let out = results.page(&b, j1, b"one", b"<a/>").unwrap();
            let PageOutput::Ok { result, text, alto } = out else {
                panic!("expected ok")
            };
            assert_eq!(
                (text.as_slice(), alto.as_slice()),
                (&b"one"[..], &b"<a/>"[..])
            );
            let back: ResultManifest = serde_json::from_slice(&result).unwrap();
            back.matches(j1, b"one", b"<a/>").unwrap();
            assert_eq!(
                results.page(&b, j2, b"one", b"<a/>").unwrap(),
                PageOutput::Failed("tesseract failed".into())
            );
            // A long volunteer error with a multi-byte character at the cut is cut before it.
            let mut long = results.clone();
            long.pages[1].error = Some(format!("{}é and more", "x".repeat(MAX_ERROR - 1)));
            assert_eq!(
                long.page(&b, j2, b"one", b"<a/>").unwrap(),
                PageOutput::Failed("x".repeat(MAX_ERROR - 1))
            );
            // Bytes that don't match the manifest fail that page only.
            assert!(matches!(
                results.page(&b, j1, b"two", b"<a/>").unwrap(),
                PageOutput::Failed(_)
            ));
            // A job outside the bundle, or another bundle's results, is an error.
            assert!(results.page(&b, &job(5), b"", b"").is_err());
            let mut other = results.clone();
            other.bundle_id = "0".repeat(64);
            assert!(other.page(&b, j1, b"one", b"<a/>").is_err());
        }
    }
}

#[cfg(all(test, feature = "pipeline"))]
mod routing_tests {
    use super::*;
    use crate::routing::EngineKind;

    #[test]
    fn legacy_tesseract_job_is_byte_identical() {
        let legacy = include_str!("fixtures/boinc-tesseract.json").trim_end();
        let job: Job = serde_json::from_str(legacy).unwrap();
        assert_eq!(job.engine.kind, EngineKind::Tesseract);
        assert_eq!(serde_json::to_string(&job).unwrap(), legacy);
        assert_eq!(
            job.identity().unwrap(),
            "05a1b237ca490126d365648bed704bd0e04c72979922ef98d86514e9b79de777"
        );
        job.validate().unwrap();
        assert_eq!(job.engine_tag().unwrap().to_string(),
            "tesseract/5.5.0 profile=eng image=sha256:aaaaaaaaaaaa models=sha256:0123456789ab route=boinc@r1 contract=0");
    }

    #[test]
    fn engine_kind_is_bound_to_identity_validation_results_and_tag() {
        let mut job: Job =
            serde_json::from_str(include_str!("fixtures/boinc-tesseract.json")).unwrap();
        job.message.language = "jpn".into();
        job.id = job.identity().unwrap();
        assert!(job.validate().is_err());
        let tesseract_id = job.id.clone();
        job.engine.kind = EngineKind::NdlocrLite;
        assert_ne!(job.identity().unwrap(), tesseract_id);
        job.engine.version = "1.0.0".into();
        job.engine.profile = "default".into();
        assert!(job.validate().is_err()); // stale identity
        job.id = job.identity().unwrap();
        job.validate().unwrap();
        let wire = serde_json::to_string(&job).unwrap();
        assert!(wire.contains("\"kind\":\"ndlocr-lite\""));
        assert_eq!(serde_json::from_str::<Job>(&wire).unwrap(), job);
        assert!(job
            .engine_tag()
            .unwrap()
            .to_string()
            .starts_with("ndlocr-lite/1.0.0 profile=default "));
        let result = ResultManifest {
            v: VERSION,
            job_id: job.id.clone(),
            scan_sha256: job.scan.sha256.clone(),
            engine: job.engine.clone(),
            text_sha256: sha256(b"text"),
            alto_sha256: sha256(b"alto"),
            elapsed_ms: 1,
            completed_at: "2026-10-10T12:00:00Z".into(),
        };
        result.matches(&job, b"text", b"alto").unwrap();
        let mut wrong = result;
        wrong.engine.kind = EngineKind::Tesseract;
        assert!(wrong.matches(&job, b"text", b"alto").is_err());
        job.message.language = "eng".into();
        job.id = job.identity().unwrap();
        assert!(job.validate().is_err());
    }
}
