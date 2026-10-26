//! The plain types, constants and URL helpers of the `loc` module: errors, sizes,
//! formats, servers, scans and the client's configuration.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;
use usnm_core::ids::PageKey;

/// The largest scan read (full-size JPEGs and JP2s are a few MB).
pub const MAX_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// Where IIIF bases point.
pub const IIIF_ROOT: &str = "https://tile.loc.gov/image-services/iiif/";
/// The storage service, under which every batch's files are.
pub const STORAGE_SERVICE_ROOT: &str = "https://tile.loc.gov/storage-services/service/";

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The page or issue isn't there, or LoC refused the request: retrying won't help.
    #[error("{0}")]
    Permanent(String),
    #[error("{0}")]
    Transient(String),
    /// loc.gov answered 503 at once for this item while it served others
    /// (the module notes): the item is broken on LoC's side. Nothing was
    /// held; ask again later, not now.
    #[error("{0}")]
    Item(String),
}

impl FetchError {
    pub(super) fn cloned(&self) -> Self {
        match self {
            Self::Permanent(m) => Self::Permanent(m.clone()),
            Self::Transient(m) => Self::Transient(m.clone()),
            Self::Item(m) => Self::Item(m.clone()),
        }
    }
}

/// The size asked of LoC's IIIF image server: the full scan, or scaled
/// to a percentage of it (IIIF's `pct:n` size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSize {
    Full,
    Pct(u8),
}

impl ImageSize {
    /// The IIIF size parameter.
    pub fn iiif(self) -> String {
        match self {
            Self::Full => "full".into(),
            Self::Pct(p) => format!("pct:{p}"),
        }
    }
}

impl std::fmt::Display for ImageSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => f.write_str("full"),
            Self::Pct(p) => write!(f, "scaled:{p}"),
        }
    }
}

impl std::str::FromStr for ImageSize {
    type Err = String;
    /// `full`, or `scaled:<1-99>`.
    fn from_str(s: &str) -> Result<Self, String> {
        if s == "full" {
            return Ok(Self::Full);
        }
        s.strip_prefix("scaled:")
            .and_then(|p| p.parse::<u8>().ok())
            .filter(|p| (1..100).contains(p))
            .map(Self::Pct)
            .ok_or_else(|| format!("{s:?} isn't `full` or `scaled:<1-99>`"))
    }
}

/// What a scan's bytes are: LoC's IIIF JPEG, or the batch's original JP2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanFormat {
    #[default]
    Jpeg,
    Jp2,
}

impl ScanFormat {
    /// The file extension.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Jp2 => "jp2",
        }
    }

    /// The name in sidecars: `jpeg` or `jp2`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Jp2 => "jp2",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "jpeg" => Some(Self::Jpeg),
            "jp2" => Some(Self::Jp2),
            _ => None,
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Jp2 => "image/jp2",
        }
    }
}

/// Which of LoC's servers a request goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    /// www.loc.gov's JSON API.
    Api,
    /// tile.loc.gov's IIIF image server.
    Tile,
    /// tile.loc.gov's storage service (batch files).
    Storage,
}

impl Server {
    pub fn name(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Tile => "tile",
            Self::Storage => "storage",
        }
    }
}

/// A page's JPEG on the IIIF server, from its service base.
pub fn image_url(iiif_base: &str, size: ImageSize) -> String {
    format!("{iiif_base}/full/{}/0/default.jpg", size.iiif())
}

/// What's fetched for a page.
#[derive(Debug)]
pub struct Scan {
    /// The page's IIIF service base: its id, whatever the bytes are.
    pub iiif_base: String,
    /// The URL the bytes came from, on tile.loc.gov: the IIIF JPEG, or the
    /// JP2 on the storage service.
    pub url: String,
    pub format: ScanFormat,
    pub bytes: Bytes,
    /// For a JP2, the md5 the batch's bag manifest lists, which the bytes
    /// were checked against.
    pub md5: Option<String>,
    /// How long the image request took (sent to last byte); zero from the cache.
    pub download: Duration,
    /// Read from the prefetch cache, not fetched.
    pub from_cache: bool,
    /// A buffer-set scan's URL on the scan server, to delete once the
    /// page's output is uploaded.
    pub evict: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocConfig {
    /// Between loc.gov API requests: 5 s, www.loc.gov's `Crawl-Delay`.
    pub api_gap: Duration,
    pub tile_gap: Duration,
    /// Between storage-service requests (batch manifests, METS, JP2s).
    pub storage_gap: Duration,
    /// The storage service gets a pacer of its own. Otherwise it shares the
    /// image server's, at the longer of the two gaps, and a hold on either
    /// holds both.
    pub storage_own_budget: bool,
    pub attempts: u32,
    /// The wait after a failed attempt, times the attempt number.
    pub backoff: Duration,
    /// `https://www.loc.gov` (a test server in tests).
    pub api_root: String,
    /// Where IIIF bases point (`https://tile.loc.gov/image-services/iiif/`).
    pub iiif_root: String,
    /// NDNP's batches on the storage service
    /// (`https://tile.loc.gov/storage-services/service/ndnp/`).
    pub storage_root: String,
    /// Chronicling America's batch directories, which redirect to the
    /// storage service (`https://chroniclingamerica.loc.gov/data/batches/`).
    pub batches_root: String,
    /// Where batch manifests and issue METS are kept, so each is fetched
    /// once. None: memory only.
    pub batch_cache: Option<PathBuf>,
    /// On a fast, bare 503, ask another item to tell an item error from a
    /// server problem. Off for callers that must send nothing after a 5xx.
    pub item_probes: bool,
    /// Treat an answer that is not the file asked for as a block: a 2xx
    /// HTML page (a bot challenge such as Cloudflare's "Just a moment...")
    /// or a body that can't be read holds the server and fails at once,
    /// with no retry. Enabled by the worker, prefetch and BOINC staging;
    /// none of their LoC routes expects HTML.
    pub html_is_block: bool,
    /// The host's global LoC budget (#962): a state file every LoC client on
    /// the host shares ([`super::global::GlobalPacer`]). None: this process's
    /// own pacers only.
    pub global_pacer: Option<PathBuf>,
    /// LoC's own S3 bucket of NDNP batches ([`super::bucket`]), asked
    /// before the storage service for the bag manifest, the METS and
    /// ([`super::LocClient::bucket_jp2`]) the JP2 of a batch it holds,
    /// paced and held on its own, outside the host's LoC budget. None: off.
    pub bucket: Option<super::bucket::BucketConfig>,
}

impl Default for LocConfig {
    fn default() -> Self {
        Self {
            api_gap: Duration::from_millis(5000),
            tile_gap: Duration::from_millis(1000),
            storage_gap: Duration::from_millis(1000),
            storage_own_budget: false,
            attempts: 5,
            backoff: Duration::from_secs(60),
            api_root: "https://www.loc.gov".into(),
            iiif_root: IIIF_ROOT.into(),
            storage_root: format!("{STORAGE_SERVICE_ROOT}ndnp/"),
            batches_root: "https://chroniclingamerica.loc.gov/data/batches/".into(),
            batch_cache: None,
            item_probes: true,
            html_is_block: false,
            global_pacer: None,
            bucket: None,
        }
    }
}

/// The IIIF service base for a JP2 URL on LoC's storage service, as
/// `jaocr.py iiif_base` builds it.
pub fn iiif_base(iiif_root: &str, jp2_url: &str) -> Option<String> {
    let path = jp2_url
        .split_once("/storage-services/service/")?
        .1
        .strip_suffix(".jp2")?;
    if path.is_empty()
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.'))
        || path.contains("..")
    {
        return None;
    }
    Some(format!("{iiif_root}service:{}", path.replace('/', ":")))
}

/// seq → IIIF base from loc.gov's issue JSON. `resources[0].files` is in
/// page (seq) order, which isn't the order of the image file names.
pub fn page_images(iiif_root: &str, item: &Value) -> BTreeMap<u16, String> {
    let files = item["resources"][0]["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut out = BTreeMap::new();
    for (i, page) in files.iter().enumerate() {
        let Ok(seq) = u16::try_from(i + 1) else { break };
        let jp2 = page.as_array().and_then(|fs| {
            fs.iter()
                .find(|f| f["mimetype"] == "image/jp2")
                .and_then(|f| f["url"].as_str())
        });
        if let Some(base) = jp2.and_then(|u| iiif_base(iiif_root, u)) {
            out.insert(seq, base);
        }
    }
    out
}

/// `<lccn>/<date>/ed-<n>`, an issue as loc.gov's item URLs name it.
pub fn issue_id(key: &PageKey) -> String {
    format!(
        "{}/{}/ed-{}",
        key.lccn,
        key.date.format("%Y-%m-%d"),
        key.edition
    )
}
