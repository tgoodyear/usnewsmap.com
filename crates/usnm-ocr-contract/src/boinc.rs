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
pub const RETRY_VERSION: u32 = 2;
/// A line job's first attempt ([`LineRead`], #1621). Workers built before line jobs know
/// versions 1 and 2 only, so they refuse one.
pub const LINES_VERSION: u32 = 3;
/// A line job's retry.
pub const LINES_RETRY_VERSION: u32 = 4;
/// The page segmentation mode of a line job: one raw line, no layout analysis.
pub const LINE_PSM: u8 = 13;
/// The most lines in one line job.
pub const MAX_LINES: usize = 2000;
/// The longest line id.
pub const MAX_LINE_ID: usize = 32;
/// The largest box coordinate, in scan pixels.
pub const MAX_LINE_COORD: u32 = 100_000;
/// The languages a line job can read: their line route is a model trained on the print.
pub const LINE_LANGUAGES: &[&str] = &["chr"];
/// `result.json`, error markers, and a page job's `request.json`.
pub const MAX_JSON: usize = 16 * 1024;
/// A `request.json` of any kind: a line job's carries its line boxes.
pub const MAX_REQUEST: usize = 512 * 1024;
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

/// One line of a line job: an id unique in the job and its box `[x0, y0, x1, y1]` in pixels
/// of the scan, without padding (the worker pads it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineBox {
    pub id: String,
    #[serde(rename = "box")]
    pub rect: [u32; 4],
}

/// The lines of a page a line job reads one at a time, each crop at [`LINE_PSM`], instead of
/// the whole page (#1621). `language` routes the job's profile, whatever the page's message
/// language is: a Cherokee Phoenix page is mostly English.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineRead {
    pub language: String,
    pub psm: u8,
    pub lines: Vec<LineBox>,
}

impl LineRead {
    pub fn validate(&self) -> anyhow::Result<()> {
        use anyhow::ensure;
        ensure!(
            LINE_LANGUAGES.contains(&self.language.as_str()),
            "no line route for language {:?}",
            self.language
        );
        ensure!(self.psm == LINE_PSM, "line jobs use PSM {LINE_PSM}");
        ensure!(
            (1..=MAX_LINES).contains(&self.lines.len()),
            "a line job reads 1 to {MAX_LINES} lines"
        );
        let mut seen = std::collections::BTreeSet::new();
        for line in &self.lines {
            ensure!(
                !line.id.is_empty()
                    && line.id.len() <= MAX_LINE_ID
                    && line
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
                "invalid line id"
            );
            ensure!(seen.insert(line.id.as_str()), "duplicate line id {}", line.id);
            let [x0, y0, x1, y1] = line.rect;
            ensure!(
                x0 < x1 && y0 < y1 && x1 <= MAX_LINE_COORD && y1 <= MAX_LINE_COORD,
                "invalid box for line {}",
                line.id
            );
        }
        Ok(())
    }
}

#[cfg(feature = "pipeline")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub v: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "is_first_attempt")]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    pub message: OcrMessage,
    pub batch_version: u16,
    pub scan: Scan,
    pub engine: Engine,
    /// A line job's lines (#1621); none for a page job, which serializes as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<LineRead>,
}
#[cfg(feature = "pipeline")]
fn is_first_attempt(attempt: &u32) -> bool {
    *attempt == 0
}
#[cfg(feature = "pipeline")]
impl Job {
    pub fn identity(&self) -> anyhow::Result<String> {
        let legacy = (
            self.v,
            &self.message.page,
            &self.message.batch,
            &self.message.language,
            self.message.reason,
            self.message.score,
            self.batch_version,
            &self.scan,
            &self.engine,
        );
        // A page job hashes exactly as before line jobs; a line job adds its lines.
        match (self.attempt == 0, &self.lines) {
            (true, None) => Ok(sha256(&serde_json::to_vec(&legacy)?)),
            (false, None) => Ok(sha256(&serde_json::to_vec(&(
                legacy,
                self.attempt,
                &self.retry_of,
            ))?)),
            (true, Some(lines)) => Ok(sha256(&serde_json::to_vec(&(legacy, lines))?)),
            (false, Some(lines)) => Ok(sha256(&serde_json::to_vec(&(
                (legacy, self.attempt, &self.retry_of),
                lines,
            ))?)),
        }
    }
    pub fn workunit(&self) -> String {
        format!("usnm_{}", self.id)
    }
    /// The version a job of this kind has: a first attempt or a retry, of a page or of lines.
    pub fn version_for(retry: bool, lines: bool) -> u32 {
        match (retry, lines) {
            (false, false) => VERSION,
            (true, false) => RETRY_VERSION,
            (false, true) => LINES_VERSION,
            (true, true) => LINES_RETRY_VERSION,
        }
    }
    /// The language the job's engine profile is routed on: a line job's line language, else
    /// the page's.
    pub fn route_language(&self) -> &str {
        match &self.lines {
            Some(lines) => &lines.language,
            None => &self.message.language,
        }
    }
    /// The model's reading of each line, in the job's order, from a result's `text.txt`: one
    /// line of text per line box, each ending in a newline. None for a page job.
    pub fn line_texts<'a>(&'a self, text: &'a str) -> anyhow::Result<Option<Vec<(&'a str, &'a str)>>> {
        let Some(lines) = &self.lines else {
            return Ok(None);
        };
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| anyhow::anyhow!("line result text does not end in a newline"))?;
        let texts: Vec<&str> = body.split('\n').collect();
        ensure!(
            texts.len() == lines.lines.len() && !text.contains('\r'),
            "line result has {} lines for {} line boxes",
            texts.len(),
            lines.lines.len()
        );
        Ok(Some(
            lines
                .lines
                .iter()
                .map(|l| l.id.as_str())
                .zip(texts)
                .collect(),
        ))
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        let first = self.attempt == 0 && self.retry_of.is_none();
        let retry = (1..i32::MAX as u32).contains(&self.attempt)
            && self
                .retry_of
                .as_deref()
                .is_some_and(|id| is_sha256(id) && id != self.id);
        let lines = self.lines.is_some();
        ensure!(
            (self.v == Self::version_for(false, lines) && first)
                || (self.v == Self::version_for(true, lines) && retry),
            "unsupported BOINC job version or attempt"
        );
        self.message.validate()?;
        ensure!(
            self.engine.kind == crate::routing::EngineKind::for_language(self.route_language()),
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
        match &self.lines {
            None => ensure!(
                self.engine.psm == 3 && self.engine.dpi == 300,
                "pilot uses PSM 3 and DPI 300"
            ),
            Some(lines) => {
                lines.validate()?;
                ensure!(
                    self.engine.psm == LINE_PSM && self.engine.dpi == 300,
                    "line jobs use PSM {LINE_PSM} and DPI 300"
                );
                ensure!(
                    self.engine.kind == crate::routing::EngineKind::Tesseract,
                    "line jobs are Tesseract jobs"
                );
            }
        }
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
            self.v == job.v
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
        let Ok(text) = std::str::from_utf8(text) else {
            bail!("text is not UTF-8");
        };
        job.line_texts(text)?;
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
                ensure!(
                    page.job.attempt == 0,
                    "retry jobs use pinned single-page workunits"
                );
                ensure!(
                    page.job.lines.is_none(),
                    "line jobs use pinned single-page workunits"
                );
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

        #[test]
        fn retries_preserve_legacy_identity_but_have_new_versioned_workunits() {
            let original: Job =
                serde_json::from_str(include_str!("fixtures/boinc-tesseract.json")).unwrap();
            original.validate().unwrap();
            assert_eq!(
                original.id,
                "05a1b237ca490126d365648bed704bd0e04c72979922ef98d86514e9b79de777"
            );
            assert!(serde_json::to_value(&original)
                .unwrap()
                .get("attempt")
                .is_none());
            let mut retry = original.clone();
            retry.v = super::super::RETRY_VERSION;
            retry.attempt = 1;
            retry.retry_of = Some(original.id.clone());
            retry.id = retry.identity().unwrap();
            retry.validate().unwrap();
            assert_ne!(retry.workunit(), original.workunit());
            let id = retry.id.clone();
            retry.message.queued_at = "2026-10-28T00:00:00Z".into();
            assert_eq!(retry.identity().unwrap(), id);
            retry.attempt = 2;
            retry.id = retry.identity().unwrap();
            retry.validate().unwrap();
            assert_ne!(retry.id, id);
            assert!(Bundle::new(vec![retry.clone(), job(2)]).is_err());
            retry.v = super::super::VERSION;
            assert!(retry.validate().is_err());
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

#[cfg(all(test, feature = "pipeline"))]
mod line_tests {
    use super::*;

    const LEGACY: &str = include_str!("fixtures/boinc-tesseract.json");

    fn line_job(n: usize) -> Job {
        let mut job: Job = serde_json::from_str(LEGACY).unwrap();
        job.v = LINES_VERSION;
        job.engine.profile = "chr_phoenix".into();
        job.engine.psm = LINE_PSM;
        job.lines = Some(LineRead {
            language: "chr".into(),
            psm: LINE_PSM,
            lines: (0..n)
                .map(|i| LineBox {
                    id: format!("b0l{i}"),
                    rect: [10, 20 + 40 * i as u32, 900, 50 + 40 * i as u32],
                })
                .collect(),
        });
        job.id = job.identity().unwrap();
        job
    }

    #[test]
    fn page_jobs_serialize_and_hash_as_before() {
        let job: Job = serde_json::from_str(LEGACY.trim_end()).unwrap();
        assert_eq!(job.lines, None);
        assert_eq!(serde_json::to_string(&job).unwrap(), LEGACY.trim_end());
        assert_eq!(
            job.identity().unwrap(),
            "05a1b237ca490126d365648bed704bd0e04c72979922ef98d86514e9b79de777"
        );
        job.validate().unwrap();
        assert_eq!(job.route_language(), "eng");
        assert_eq!(job.line_texts("anything").unwrap(), None);
    }

    #[test]
    fn line_jobs_have_their_own_version_and_identity() {
        let job = line_job(3);
        job.validate().unwrap();
        assert_eq!(job.route_language(), "chr");
        assert_eq!(job.message.language, "eng");
        // A line job can't pass as a page job, nor a page job as a line job.
        let mut old = job.clone();
        old.v = VERSION;
        old.id = old.identity().unwrap();
        assert!(old.validate().is_err());
        let mut page: Job = serde_json::from_str(LEGACY).unwrap();
        page.v = LINES_VERSION;
        page.id = page.identity().unwrap();
        assert!(page.validate().is_err());
        // The lines are in the identity.
        let mut moved = job.clone();
        moved.lines.as_mut().unwrap().lines[1].rect[0] += 1;
        assert_ne!(moved.identity().unwrap(), job.id);
        assert!(moved.validate().is_err());
        // A worker that predates line jobs refuses the request outright.
        let wire = serde_json::to_string(&job).unwrap();
        assert!(wire.contains("\"lines\":{\"language\":\"chr\",\"psm\":13,"));
        assert_eq!(serde_json::from_str::<Job>(&wire).unwrap(), job);
    }

    #[test]
    fn line_jobs_retry_with_the_line_retry_version() {
        let first = line_job(2);
        let mut retry = first.clone();
        retry.v = Job::version_for(true, true);
        retry.attempt = 1;
        retry.retry_of = Some(first.id.clone());
        retry.id = retry.identity().unwrap();
        retry.validate().unwrap();
        assert_ne!(retry.id, first.id);
        retry.v = RETRY_VERSION;
        retry.id = retry.identity().unwrap();
        assert!(retry.validate().is_err());
        assert_eq!(
            [
                Job::version_for(false, false),
                Job::version_for(true, false),
                Job::version_for(false, true),
                Job::version_for(true, true)
            ],
            [VERSION, RETRY_VERSION, LINES_VERSION, LINES_RETRY_VERSION]
        );
    }

    fn rejected(edit: impl FnOnce(&mut Job)) -> bool {
        let mut job = line_job(3);
        edit(&mut job);
        job.id = job.identity().unwrap();
        job.validate().is_err()
    }

    #[test]
    fn line_jobs_read_at_psm_13_only() {
        assert!(rejected(|j| j.engine.psm = 3));
        assert!(rejected(|j| j.lines.as_mut().unwrap().psm = 3));
        assert!(rejected(|j| j.engine.dpi = 400));
        // And page jobs stay at PSM 3.
        let mut page: Job = serde_json::from_str(LEGACY).unwrap();
        page.engine.psm = LINE_PSM;
        page.id = page.identity().unwrap();
        assert!(page.validate().is_err());
    }

    #[test]
    fn line_jobs_have_a_sane_line_count() {
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines.clear()));
        let mut most = line_job(MAX_LINES);
        most.validate().unwrap();
        most.lines.as_mut().unwrap().lines.push(LineBox {
            id: "extra".into(),
            rect: [1, 1, 2, 2],
        });
        most.id = most.identity().unwrap();
        assert!(most.validate().is_err());
        // The most lines still fit a request.
        assert!(serde_json::to_vec_pretty(&line_job(MAX_LINES)).unwrap().len() <= MAX_REQUEST);
    }

    #[test]
    fn line_ids_are_unique_short_ascii() {
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[2].id = "b0l0".into()));
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].id = String::new()));
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].id =
            "x".repeat(MAX_LINE_ID + 1)));
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].id = "b0 l0".into()));
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].id = "Ꭰ".into()));
        assert!(!rejected(|j| j.lines.as_mut().unwrap().lines[0].id =
            "x".repeat(MAX_LINE_ID)));
    }

    #[test]
    fn line_boxes_are_non_empty_and_in_bounds() {
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].rect = [5, 5, 5, 9]));
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].rect = [5, 9, 9, 9]));
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].rect = [9, 5, 5, 9]));
        assert!(rejected(|j| j.lines.as_mut().unwrap().lines[0].rect =
            [0, 0, MAX_LINE_COORD + 1, 9]));
        assert!(!rejected(|j| j.lines.as_mut().unwrap().lines[0].rect =
            [0, 0, MAX_LINE_COORD, MAX_LINE_COORD]));
    }

    #[test]
    fn line_jobs_name_a_line_language() {
        assert!(rejected(|j| j.lines.as_mut().unwrap().language = "eng".into()));
        assert!(rejected(|j| j.lines.as_mut().unwrap().language = "jpn".into()));
    }

    #[test]
    fn line_jobs_never_go_in_bundles() {
        let mut other = line_job(1);
        other.message.page = "sn00000001/1900-01-01/ed-1/seq-2".into();
        other.id = other.identity().unwrap();
        assert!(bundle::Bundle::new(vec![line_job(1), other]).is_err());
    }

    #[test]
    fn line_results_give_one_text_line_per_box() {
        let job = line_job(3);
        let text = "ᎠᏍᎦᏯ\n\nᏣᎳᎩ ᎦᏬᏂᎯᏍᏗ\n";
        assert_eq!(
            job.line_texts(text).unwrap().unwrap(),
            [("b0l0", "ᎠᏍᎦᏯ"), ("b0l1", ""), ("b0l2", "ᏣᎳᎩ ᎦᏬᏂᎯᏍᏗ")]
        );
        assert!(job.line_texts("a\nb\n").is_err());
        assert!(job.line_texts("a\nb\nc\nd\n").is_err());
        assert!(job.line_texts("a\nb\nc").is_err());
        assert!(job.line_texts("a\r\nb\nc\n").is_err());
        let result = ResultManifest {
            v: job.v,
            job_id: job.id.clone(),
            scan_sha256: job.scan.sha256.clone(),
            engine: job.engine.clone(),
            text_sha256: sha256(text.as_bytes()),
            alto_sha256: sha256(b"alto"),
            elapsed_ms: 1,
            completed_at: "2026-11-09T12:00:00Z".into(),
        };
        result.matches(&job, text.as_bytes(), b"alto").unwrap();
        let short = "a\nb\n";
        let mut wrong = result;
        wrong.text_sha256 = sha256(short.as_bytes());
        assert!(wrong.matches(&job, short.as_bytes(), b"alto").is_err());
    }
}
