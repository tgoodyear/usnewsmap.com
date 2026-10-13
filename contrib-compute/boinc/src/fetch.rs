//! The "fetch-and-ocr" workunit type: the workunit carries page identifiers and source URLs,
//! and the client downloads the scan itself, reads it, and uploads only the text.
//!
//! What is built here: the manifest type and its checks, slicing and workunit naming, the
//! `create_work` arguments, the request budget the generator enforces, and the client's
//! per-page decision logic (download the pinned URL, verify, read), all
//! against traits so it runs offline in tests.
//!
//! What is not built: the real HTTP client (`Fetcher`) and the call to Tesseract (`Ocr`) in
//! `usnm-ocr-run`. `UnimplementedFetcher` marks the spot. See docs/design.md, "Job
//! types", for the rate-limit approach. This type is off by default.

use crate::result::{sha256_hex, SourceInfo};
use crate::workgen::{manifest_file_name, stage_args, JobParams, StateEntry, Submitter};
use md5::{Digest as _, Md5};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeSet;

/// Manifest `kind` of this workunit type.
pub const KIND: &str = "fetch-and-ocr";
/// The only hosts a fetch-and-ocr workunit may name. Anything else is refused at generation
/// time and again by the client.
pub const ALLOWED_HOSTS: [&str; 1] = ["tile.loc.gov"];
/// Smallest gap the client leaves between two requests to the source, in milliseconds. This
/// matches the 1 s gap the maintainer's own nodes keep between image requests.
pub const MIN_GAP_MS: u64 = 1000;
/// Largest scan the client downloads, in bytes.
pub const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;

/// One page of a fetch-and-ocr workunit. Fields are in alphabetical order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchPage {
    /// Base batch name, for example `nbu_gage`. Carried for provenance.
    pub batch: String,
    /// Expected size in bytes, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// `jp2` or `jpeg`. Pinned in the workunit: the client requests this one URL and no other.
    pub format: String,
    pub language: String,
    /// MD5 of the file. Required for a JP2 (the batch's bag lists it); a IIIF JPEG has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    pub page_id: String,
    pub profile: String,
    pub url: String,
}

fn is_md5(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The host and path of an `https://host/path` URL with no userinfo, port, query or fragment.
fn split_url(url: &str) -> Result<(&str, &str), String> {
    let rest = url.strip_prefix("https://").ok_or("url must be https://")?;
    if url.contains(char::is_whitespace) || url.contains(['?', '#', '@']) {
        return Err("url has whitespace, a query, a fragment or userinfo".into());
    }
    let (host, path) = rest.split_once('/').ok_or("url has no path")?;
    if host.contains(':') {
        return Err("url has a port".into());
    }
    if !ALLOWED_HOSTS.contains(&host) {
        return Err(format!("host {host:?} is not allowed"));
    }
    if path.split('/').any(|s| s == ".." || s == ".") {
        return Err("url path has dot segments".into());
    }
    Ok((host, path))
}

fn check_jp2_url(url: &str) -> Result<(), String> {
    let (_, path) = split_url(url)?;
    if !path.starts_with("storage-services/") || !path.ends_with(".jp2") {
        return Err("a jp2 url must be a storage-services path ending in .jp2".into());
    }
    Ok(())
}

fn check_jpeg_url(url: &str) -> Result<(), String> {
    let (_, path) = split_url(url)?;
    if !path.starts_with("image-services/iiif/") || !path.ends_with("/full/full/0/default.jpg") {
        return Err("a jpeg url must be a full-size IIIF default.jpg".into());
    }
    Ok(())
}

/// Check one page. Called by the generator and again by the client.
pub fn check_page(p: &FetchPage) -> Result<(), String> {
    if p.page_id.is_empty() || p.page_id.len() > 200 || p.page_id.chars().any(char::is_control) {
        return Err("page_id is empty, too long or has control characters".into());
    }
    if p.batch.is_empty()
        || !p
            .batch
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("batch must be lower-case letters, digits and underscores".into());
    }
    if p.language.is_empty() || p.profile.is_empty() {
        return Err("language and profile are required".into());
    }
    if let Some(b) = p.bytes {
        if b == 0 || b > MAX_SCAN_BYTES {
            return Err(format!("bytes must be 1..={MAX_SCAN_BYTES}"));
        }
    }
    match p.format.as_str() {
        "jp2" => {
            check_jp2_url(&p.url)?;
            match &p.md5 {
                Some(m) if is_md5(m) => {}
                _ => return Err("a jp2 page needs the bag's md5 (32 lowercase hex)".into()),
            }
        }
        "jpeg" => {
            check_jpeg_url(&p.url)?;
            if let Some(m) = &p.md5 {
                if !is_md5(m) {
                    return Err("md5 must be 32 lowercase hex".into());
                }
            }
        }
        other => return Err(format!("format {other:?} must be jp2 or jpeg")),
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct FetchManifestError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for FetchManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "fetch manifest line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for FetchManifestError {}

/// Parse a JSON-lines list of pages to fetch and read. Duplicate page ids are an error.
pub fn parse(text: &str) -> Result<Vec<FetchPage>, FetchManifestError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let page: FetchPage = serde_json::from_str(line).map_err(|e| FetchManifestError {
            line: n,
            message: e.to_string(),
        })?;
        check_page(&page).map_err(|message| FetchManifestError { line: n, message })?;
        if !seen.insert(page.page_id.clone()) {
            return Err(FetchManifestError {
                line: n,
                message: format!("duplicate page_id {:?}", page.page_id),
            });
        }
        out.push(page);
    }
    Ok(out)
}

/// A workunit's worth of pages to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchSlice {
    pub name: String,
    pub pages: Vec<FetchPage>,
}

/// `usnm_ocr_f_` plus 16 hex characters of the SHA-256 of the kind and the page ids, so the
/// same pages as a staged workunit get a different name.
pub fn slice_name(pages: &[FetchPage]) -> String {
    let mut h = Sha256::new();
    h.update(KIND.as_bytes());
    h.update(b"\n");
    for p in pages {
        h.update(p.page_id.as_bytes());
        h.update(b"\n");
    }
    let hex: String = h
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{}_f_{hex}", crate::APP_NAME)
}

/// Cut pages into workunits of `pages_per_wu`, never mixing language or profile, in order.
/// A short final group is kept only with `allow_partial`.
pub fn slice(pages: &[FetchPage], pages_per_wu: usize, allow_partial: bool) -> Vec<FetchSlice> {
    assert!(pages_per_wu > 0, "pages_per_wu must be positive");
    let mut keys: Vec<(&str, &str)> = Vec::new();
    for p in pages {
        let k = (p.language.as_str(), p.profile.as_str());
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut out = Vec::new();
    for (lang, prof) in keys {
        let group: Vec<&FetchPage> = pages
            .iter()
            .filter(|p| p.language == lang && p.profile == prof)
            .collect();
        for chunk in group.chunks(pages_per_wu) {
            if chunk.len() < pages_per_wu && !allow_partial {
                continue;
            }
            let pages: Vec<FetchPage> = chunk.iter().map(|p| (*p).clone()).collect();
            out.push(FetchSlice {
                name: slice_name(&pages),
                pages,
            });
        }
    }
    out
}

/// The manifest file a fetch-and-ocr workunit carries as its only input (`manifest.json`).
/// Fields are in alphabetical order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchManifest {
    pub kind: String,
    /// Smallest gap between two requests to the source, in milliseconds.
    pub min_gap_ms: u64,
    pub pages: Vec<FetchPage>,
    pub schema: u32,
    pub wu: String,
}

pub fn fetch_manifest(slice: &FetchSlice, min_gap_ms: u64) -> FetchManifest {
    FetchManifest {
        kind: KIND.into(),
        min_gap_ms: min_gap_ms.max(MIN_GAP_MS),
        pages: slice.pages.clone(),
        schema: 2,
        wu: slice.name.clone(),
    }
}

/// Client-side check of a manifest it was given: kind, schema, every page.
pub fn check_manifest(m: &FetchManifest) -> Result<(), String> {
    if m.kind != KIND || m.schema != 2 {
        return Err("not a fetch-and-ocr manifest".into());
    }
    if m.min_gap_ms < MIN_GAP_MS {
        return Err(format!("min_gap_ms below {MIN_GAP_MS}"));
    }
    if m.pages.is_empty() || m.pages.len() > 16 {
        return Err("a manifest holds 1 to 16 pages".into());
    }
    m.pages.iter().try_for_each(check_page)
}

/// `create_work` arguments. The manifest is the only input file; there are no remote files.
pub fn create_work_args(slice: &FetchSlice, params: &JobParams) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |k: &str, v: String| {
        a.push(format!("--{k}"));
        a.push(v);
    };
    push("appname", crate::APP_NAME.into());
    push("wu_name", slice.name.clone());
    push(
        "wu_template",
        format!("templates/{}_fetch_in", crate::APP_NAME),
    );
    push(
        "result_template",
        format!("templates/{}_out", crate::APP_NAME),
    );
    push("rsc_fpops_est", format!("{:.0}", params.rsc_fpops_est));
    push("rsc_fpops_bound", format!("{:.0}", params.rsc_fpops_bound));
    push("rsc_memory_bound", params.rsc_memory_bound.to_string());
    // Room for the downloaded scans.
    push(
        "rsc_disk_bound",
        (params.rsc_disk_bound + 64 * 1024 * 1024 * slice.pages.len() as u64).to_string(),
    );
    push("delay_bound", params.delay_bound_secs.to_string());
    push("min_quorum", params.min_quorum.to_string());
    push("target_nresults", params.target_nresults.to_string());
    push("max_error_results", params.max_error_results.to_string());
    push("max_total_results", params.max_total_results.to_string());
    a.push(manifest_file_name(&slice.name));
    a
}

/// Request budget for the generator. A workunit is a promise of `pages_per_wu` requests to the
/// source, so the budget is counted in workunits created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub max_per_day: usize,
    pub max_per_hour: usize,
    pub max_per_run: usize,
}

impl Budget {
    /// Workunits per hour for a global ceiling of `requests_per_min` to the source, if every
    /// page costs `requests_per_page` requests (replication factor, plus any pages the generator
    /// reissues in the other format).
    pub fn from_requests(
        requests_per_min: f64,
        requests_per_page: f64,
        pages_per_wu: usize,
    ) -> Budget {
        let per_hour =
            (requests_per_min * 60.0 / requests_per_page / pages_per_wu as f64).floor() as usize;
        Budget {
            max_per_day: per_hour * 24,
            max_per_hour: per_hour,
            max_per_run: per_hour.clamp(1, 50),
        }
    }
}

/// Pick the slices to create now. Nothing is created while `hold_until` (seconds since the
/// epoch) is in the future: that is the circuit breaker the operator trips when the source
/// answers 429, 403 or 503 to any client. A workunit already in the state is skipped.
pub fn select<'a>(
    slices: &'a [FetchSlice],
    state: &[StateEntry],
    now: u64,
    budget: &Budget,
    hold_until: u64,
) -> Vec<&'a FetchSlice> {
    if now < hold_until {
        return Vec::new();
    }
    let hour = state.iter().filter(|e| e.created_at + 3_600 > now).count();
    let day = state.iter().filter(|e| e.created_at + 86_400 > now).count();
    let room = budget
        .max_per_hour
        .saturating_sub(hour)
        .min(budget.max_per_day.saturating_sub(day))
        .min(budget.max_per_run);
    slices
        .iter()
        .filter(|s| !state.iter().any(|e| e.wu == s.name))
        .take(room)
        .collect()
}

/// Stage the manifest and create the workunit.
pub fn submit(
    slice: &FetchSlice,
    params: &JobParams,
    sub: &mut dyn Submitter,
) -> Result<(), String> {
    let json =
        serde_json::to_string(&fetch_manifest(slice, MIN_GAP_MS)).map_err(|e| e.to_string())?;
    let local = sub.write_manifest(&manifest_file_name(&slice.name), &json)?;
    sub.stage_file(&stage_args(&local))?;
    sub.create_work(&create_work_args(slice, params))
}

// ---- client side -------------------------------------------------------------------------

/// Why a download did not produce bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum FetchError {
    /// 429, 403 or 503 (or a Retry-After): the source is holding this client. Never retried
    /// and never worked around; the task ends.
    Held {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    NotFound,
    TooBig,
    Transient(String),
}

/// The HTTP client. It names the project in its User-Agent, leaves `min_gap_ms` between
/// requests, sends one request at a time and follows no redirect to another host. Not built
/// here; `UnimplementedFetcher` stands in.
pub trait Fetcher {
    fn get(&mut self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError>;
}

pub struct UnimplementedFetcher;

impl Fetcher for UnimplementedFetcher {
    fn get(&mut self, _url: &str, _max_bytes: u64) -> Result<Vec<u8>, FetchError> {
        // TODO: blocking HTTPS client with the pacing and hold rules above. Needs the
        // low per-client rate; see design.md "Job types".
        Err(FetchError::Transient("fetcher not implemented".into()))
    }
}

/// An engine failure. `exit_code` is the process status if it exited.
#[derive(Debug, PartialEq, Eq)]
pub struct OcrError {
    pub exit_code: Option<i32>,
    pub message: String,
}

/// Reads one image and returns its text. `usnm-ocr-run` implements it by running Tesseract.
pub trait Ocr {
    fn ocr(&mut self, image: &[u8], format: &str, page: &FetchPage) -> Result<String, OcrError>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum PageFailure {
    Held {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    Mismatch {
        expected: String,
        got: String,
    },
    SizeMismatch {
        expected: u64,
        got: u64,
    },
    NotFound,
    TooBig,
    Transient(String),
    Engine(String),
}

impl PageFailure {
    /// Exit status for `usnm-ocr-run`, so the project logs say why a task failed.
    pub fn exit_code(&self) -> i32 {
        match self {
            PageFailure::Held { .. } => 33,
            PageFailure::Mismatch { .. } | PageFailure::SizeMismatch { .. } => 34,
            PageFailure::NotFound => 35,
            PageFailure::TooBig => 36,
            PageFailure::Transient(_) => 37,
            PageFailure::Engine(_) => 38,
        }
    }
}

impl From<FetchError> for PageFailure {
    fn from(e: FetchError) -> Self {
        match e {
            FetchError::Held {
                status,
                retry_after_secs,
            } => PageFailure::Held {
                status,
                retry_after_secs,
            },
            FetchError::NotFound => PageFailure::NotFound,
            FetchError::TooBig => PageFailure::TooBig,
            FetchError::Transient(m) => PageFailure::Transient(m),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PageOutcome {
    pub input_md5: String,
    pub source: SourceInfo,
    pub text: String,
}

pub fn md5_hex(bytes: &[u8]) -> String {
    Md5::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn verify(p: &FetchPage, bytes: &[u8]) -> Result<(), PageFailure> {
    if let Some(n) = p.bytes {
        if bytes.len() as u64 != n {
            return Err(PageFailure::SizeMismatch {
                expected: n,
                got: bytes.len() as u64,
            });
        }
    }
    if let Some(m) = &p.md5 {
        let got = md5_hex(bytes);
        if &got != m {
            return Err(PageFailure::Mismatch {
                expected: m.clone(),
                got,
            });
        }
    }
    Ok(())
}

/// Download, verify and read one page. The client requests the one URL the workunit pins and
/// never switches format on its own: an engine crash is a failed result, and the generator
/// may reissue that page in the other format as a new workunit. A checksum mismatch is a
/// download failure. A hold from the source ends the page at once.
pub fn fetch_and_ocr_page(
    p: &FetchPage,
    fetcher: &mut dyn Fetcher,
    engine: &mut dyn Ocr,
) -> Result<PageOutcome, PageFailure> {
    check_page(p).map_err(PageFailure::Transient)?;
    let bytes = fetcher.get(
        &p.url,
        p.bytes.unwrap_or(MAX_SCAN_BYTES).min(MAX_SCAN_BYTES),
    )?;
    verify(p, &bytes)?;
    let text = engine
        .ocr(&bytes, &p.format, p)
        .map_err(|e| PageFailure::Engine(e.message))?;
    let (data, format) = (bytes, p.format.clone());
    Ok(PageOutcome {
        input_md5: md5_hex(&data),
        source: SourceInfo {
            bytes: data.len() as u64,
            format,
            sha256: sha256_hex(&data),
        },
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const JP2: &str = "https://tile.loc.gov/storage-services/service/ndnp/nbu/batch_nbu_gage_ver01/data/sn1/reel/1890050301/0007.jp2";
    const JPG: &str = "https://tile.loc.gov/image-services/iiif/service:ndnp:nbu:batch_nbu_gage_ver01:data:sn1:reel:1890050301:0007/full/full/0/default.jpg";

    fn page(id: &str) -> FetchPage {
        FetchPage {
            batch: "nbu_gage".into(),
            bytes: None,
            format: "jp2".into(),
            language: "deu".into(),
            md5: Some(md5_hex(b"jp2 bytes")),
            page_id: id.into(),
            profile: "frak2021".into(),
            url: JP2.into(),
        }
    }

    #[test]
    fn md5_matches_known_value() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn accepts_a_good_page_and_refuses_bad_urls() {
        assert_eq!(check_page(&page("a")), Ok(()));
        let bad_urls = [
            "http://tile.loc.gov/storage-services/x.jp2",
            "https://evil.example/storage-services/x.jp2",
            "https://tile.loc.gov:8443/storage-services/x.jp2",
            "https://tile.loc.gov/storage-services/x.jp2?token=1",
            "https://user@tile.loc.gov/storage-services/x.jp2",
            "https://tile.loc.gov/storage-services/../x.jp2",
            "https://tile.loc.gov/other/x.jp2",
            "https://tile.loc.gov/storage-services/x.jpg",
        ];
        for u in bad_urls {
            let mut p = page("a");
            p.url = u.into();
            assert!(check_page(&p).is_err(), "{u}");
        }
    }

    #[test]
    fn a_jp2_needs_an_md5() {
        let mut p = page("a");
        p.md5 = None;
        assert!(check_page(&p).is_err());
        let mut p = page("a");
        p.md5 = Some("ABC".into());
        assert!(check_page(&p).is_err());
        let mut j = page("a");
        j.format = "jpeg".into();
        j.url = JPG.into();
        j.md5 = None;
        assert_eq!(check_page(&j), Ok(()));
    }

    #[test]
    fn parse_reports_the_line_and_duplicates() {
        let good = serde_json::to_string(&page("a")).unwrap();
        let dup = format!("{good}\n\n{good}\n");
        let e = parse(&dup).unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.message.contains("duplicate"));
        let e = parse("{\"nope\":1}").unwrap_err();
        assert_eq!(e.line, 1);
        assert_eq!(parse(&good).unwrap().len(), 1);
    }

    #[test]
    fn manifest_keys_are_sorted_and_round_trip() {
        let pages: Vec<_> = (0..4).map(|i| page(&format!("p{i}"))).collect();
        let s = slice(&pages, 4, false).remove(0);
        let m = fetch_manifest(&s, 250);
        assert_eq!(
            m.min_gap_ms, MIN_GAP_MS,
            "the gap never goes below the floor"
        );
        let json = serde_json::to_string(&m).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        let back: FetchManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        assert_eq!(check_manifest(&back), Ok(()));
        let mut low = back.clone();
        low.min_gap_ms = 10;
        assert!(check_manifest(&low).is_err());
    }

    #[test]
    fn slicing_names_and_groups() {
        let mut pages: Vec<_> = (0..5).map(|i| page(&format!("p{i}"))).collect();
        pages[2].language = "eng".into();
        pages[2].profile = "eng".into();
        let full = slice(&pages, 2, false);
        assert_eq!(
            full.len(),
            2,
            "deu: p0,p1 and p3,p4; eng has one page and waits"
        );
        let all = slice(&pages, 2, true);
        assert_eq!(all.len(), 3);
        assert!(full[0].name.starts_with("usnm_ocr_f_"));
        assert_eq!(full[0].name.len(), "usnm_ocr_f_".len() + 16);
        assert_eq!(
            slice(&pages, 2, false)[0].name,
            full[0].name,
            "deterministic"
        );
    }

    #[test]
    fn create_work_has_one_input_and_no_remote_files() {
        let pages: Vec<_> = (0..4).map(|i| page(&format!("p{i}"))).collect();
        let s = slice(&pages, 4, false).remove(0);
        let args = create_work_args(&s, &JobParams::for_pages(4));
        assert!(!args.contains(&"--remote_file".to_string()));
        assert_eq!(args.last().unwrap(), &format!("{}_manifest.json", s.name));
        let i = args.iter().position(|a| a == "--wu_template").unwrap();
        assert_eq!(args[i + 1], "templates/usnm_ocr_fetch_in");
    }

    fn state(now: u64, ages: &[u64]) -> Vec<StateEntry> {
        ages.iter()
            .enumerate()
            .map(|(i, a)| StateEntry {
                created_at: now - a,
                wu: format!("old{i}"),
            })
            .collect()
    }

    #[test]
    fn budget_comes_from_the_request_ceiling() {
        // 60 requests a minute, 1.25 requests per page (replication), 4 pages each.
        let b = Budget::from_requests(60.0, 1.25, 4);
        assert_eq!(b.max_per_hour, 720);
        assert_eq!(b.max_per_day, 720 * 24);
    }

    #[test]
    fn select_honours_hour_day_run_and_the_hold() {
        let pages: Vec<_> = (0..40).map(|i| page(&format!("p{i}"))).collect();
        let slices = slice(&pages, 4, false);
        assert_eq!(slices.len(), 10);
        let now = 1_000_000;
        let b = Budget {
            max_per_day: 8,
            max_per_hour: 5,
            max_per_run: 50,
        };
        assert_eq!(select(&slices, &[], now, &b, 0).len(), 5);
        // Two made in the last hour, three earlier today: hour room 3, day room 3.
        let st = state(now, &[10, 20, 4_000, 5_000, 6_000]);
        assert_eq!(select(&slices, &st, now, &b, 0).len(), 3);
        // Day cap binds.
        let st = state(now, &[10, 4_000, 5_000, 6_000, 7_000, 8_000, 9_000]);
        assert_eq!(select(&slices, &st, now, &b, 0).len(), 1);
        // The hold creates nothing, and ends on time.
        assert!(select(&slices, &[], now, &b, now + 1).is_empty());
        assert_eq!(select(&slices, &[], now, &b, now).len(), 5);
        // A workunit already made is skipped.
        let st = vec![StateEntry {
            created_at: now.saturating_sub(900_000),
            wu: slices[0].name.clone(),
        }];
        assert!(select(&slices, &st, now, &b, 0)
            .iter()
            .all(|s| s.name != slices[0].name));
    }

    #[derive(Default)]
    struct Rec(Vec<String>);
    impl Submitter for Rec {
        fn write_manifest(&mut self, name: &str, json: &str) -> Result<String, String> {
            let m: FetchManifest = serde_json::from_str(json).map_err(|e| e.to_string())?;
            check_manifest(&m)?;
            self.0.push(format!("write {name}"));
            Ok(format!("/tmp/{name}"))
        }
        fn stage_file(&mut self, args: &[String]) -> Result<(), String> {
            self.0.push(format!("stage {}", args.join(" ")));
            Ok(())
        }
        fn create_work(&mut self, _args: &[String]) -> Result<(), String> {
            self.0.push("create".into());
            Ok(())
        }
    }

    #[test]
    fn submit_stages_a_checked_manifest_then_creates() {
        let pages: Vec<_> = (0..4).map(|i| page(&format!("p{i}"))).collect();
        let s = slice(&pages, 4, false).remove(0);
        let mut r = Rec::default();
        submit(&s, &JobParams::for_pages(4), &mut r).unwrap();
        assert_eq!(r.0.len(), 3);
        assert!(r.0[0].starts_with("write usnm_ocr_f_"));
        assert_eq!(r.0[2], "create");
    }

    struct Web {
        files: BTreeMap<String, Result<Vec<u8>, FetchError>>,
        calls: Vec<String>,
    }
    impl Fetcher for Web {
        fn get(&mut self, url: &str, _max: u64) -> Result<Vec<u8>, FetchError> {
            self.calls.push(url.into());
            match self.files.remove(url) {
                Some(r) => r,
                None => Err(FetchError::NotFound),
            }
        }
    }
    struct Eng {
        crash_jp2: bool,
        seen: Vec<String>,
    }
    impl Ocr for Eng {
        fn ocr(&mut self, _image: &[u8], format: &str, _p: &FetchPage) -> Result<String, OcrError> {
            self.seen.push(format.into());
            if format == "jp2" && self.crash_jp2 {
                return Err(OcrError {
                    exit_code: Some(136),
                    message: "signal".into(),
                });
            }
            Ok(format!("text from {format}"))
        }
    }

    fn web(jp2: Result<Vec<u8>, FetchError>, jpg: Option<Vec<u8>>) -> Web {
        let mut files = BTreeMap::new();
        files.insert(JP2.to_string(), jp2);
        if let Some(j) = jpg {
            files.insert(JPG.to_string(), Ok(j));
        }
        Web {
            files,
            calls: vec![],
        }
    }

    #[test]
    fn reads_the_jp2_and_records_what_it_read() {
        let mut w = web(Ok(b"jp2 bytes".to_vec()), None);
        let mut e = Eng {
            crash_jp2: false,
            seen: vec![],
        };
        let out = fetch_and_ocr_page(&page("a"), &mut w, &mut e).unwrap();
        assert_eq!(out.text, "text from jp2");
        assert_eq!(out.input_md5, md5_hex(b"jp2 bytes"));
        assert_eq!(out.source.format, "jp2");
        assert_eq!(w.calls, vec![JP2.to_string()]);
    }

    #[test]
    fn a_wrong_md5_stops_before_the_engine_runs() {
        let mut w = web(Ok(b"tampered".to_vec()), None);
        let mut e = Eng {
            crash_jp2: false,
            seen: vec![],
        };
        let err = fetch_and_ocr_page(&page("a"), &mut w, &mut e).unwrap_err();
        assert!(matches!(err, PageFailure::Mismatch { .. }));
        assert_eq!(err.exit_code(), 34);
        assert!(e.seen.is_empty());
    }

    #[test]
    fn an_engine_crash_is_a_failed_result_and_never_switches_format() {
        let mut w = web(Ok(b"jp2 bytes".to_vec()), Some(b"jpeg bytes".to_vec()));
        let mut e = Eng {
            crash_jp2: true,
            seen: vec![],
        };
        let err = fetch_and_ocr_page(&page("a"), &mut w, &mut e).unwrap_err();
        assert!(matches!(err, PageFailure::Engine(_)));
        assert_eq!(e.seen, vec!["jp2"]);
        assert_eq!(
            w.calls,
            vec![JP2.to_string()],
            "only the pinned URL is requested"
        );
    }

    #[test]
    fn a_hold_ends_the_page() {
        let mut w = web(
            Err(FetchError::Held {
                status: 429,
                retry_after_secs: Some(3600),
            }),
            Some(b"jpeg bytes".to_vec()),
        );
        let mut e = Eng {
            crash_jp2: false,
            seen: vec![],
        };
        let err = fetch_and_ocr_page(&page("a"), &mut w, &mut e).unwrap_err();
        assert_eq!(
            err,
            PageFailure::Held {
                status: 429,
                retry_after_secs: Some(3600)
            }
        );
        assert_eq!(err.exit_code(), 33);
        assert_eq!(
            w.calls.len(),
            1,
            "the other URL on the same host is not tried"
        );
    }

    #[test]
    fn a_size_mismatch_is_caught() {
        let mut p = page("a");
        p.bytes = Some(3);
        let mut w = web(Ok(b"jp2 bytes".to_vec()), None);
        let mut e = Eng {
            crash_jp2: false,
            seen: vec![],
        };
        assert!(matches!(
            fetch_and_ocr_page(&p, &mut w, &mut e),
            Err(PageFailure::SizeMismatch {
                expected: 3,
                got: 9
            })
        ));
    }

    #[test]
    fn the_stub_fetcher_fails_closed() {
        let mut e = Eng {
            crash_jp2: false,
            seen: vec![],
        };
        let r = fetch_and_ocr_page(&page("a"), &mut UnimplementedFetcher, &mut e);
        assert!(matches!(r, Err(PageFailure::Transient(_))));
    }
}
