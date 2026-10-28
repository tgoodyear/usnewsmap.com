//! Page scans from the Library of Congress.
//!
//! **The batch route** (#368, the fetcher's first choice): a page's original
//! JP2 from its batch's directory on `tile.loc.gov`'s storage service
//! ([`crate::batch`]). One `manifest-md5.txt` per batch (kept on disk)
//! names every file and its md5; one METS per issue (kept on disk too)
//! maps seq to the JP2's frame number; the JP2 itself is checked against
//! the bag's md5. Its IIIF base is built from the same path. Nothing goes
//! to www.loc.gov. [`LocClient::resolve`] answers `None` when the batch,
//! the issue, its METS or the page isn't there, and only then does the
//! caller go to the item API.
//!
//! **LoC's bucket** ([`bucket`], off unless [`LocConfig::bucket`] names
//! it): LoC's own S3 copy of the batches. When it is on, the bag manifest
//! and each issue METS are asked of it first, for the batches it holds, and
//! [`LocClient::bucket_jp2`] gives a page's JP2 from it. Each is checked as
//! LoC's would be; the bucket is paced and held on its own, never in LoC's
//! pacers or budget, and anything it can't serve (a private batch, a
//! missing file, a mismatch, a hold) goes to LoC as before. The bucket's
//! keys don't name the awardee, so a manifest from it is kept under the
//! storage URL the batch's name gives as a guess ([`Bag::guessed`], the
//! `url.guessed` file beside the manifest, not `url`), used only while the
//! bucket is on: without it, the batch is looked up on LoC as if nothing
//! were on disk. A METS or JP2 LoC serves under a guessed URL, matching
//! the bag's md5, confirms it: the guess is kept as checked. When LoC
//! answers 404 for a file under a guessed URL, Chronicling America's
//! redirect is asked where the batch is (as for a manifest LoC doesn't
//! have at the guessed path), and the batch's URL is moved there and kept
//! as checked before anything goes to the item API.
//!
//! **The item API route** (the worker's, and the fetcher's fallback): for
//! each issue, one request to loc.gov's item JSON
//! (`https://www.loc.gov/item/{lccn}/{date}/ed-{n}/?fo=json`) gives every
//! page's JP2 file; its path names the page's IIIF service
//! (`https://tile.loc.gov/image-services/iiif/service:…`), and the scan is
//! that base plus `/full/full/0/default.jpg`. The item JSON is cached (LRU),
//! so the pages of one issue cost one API request.
//!
//! **Pacing.** loc.gov's API, its image server and its storage service are
//! each paced by a gap between requests shared by every task on the node
//! (the storage service shares the image server's pacer unless told it has
//! a budget of its own: LoC publishes a limit for each, but not whether
//! they are counted together). `Retry-After` is honoured on 429 and 503, a
//! 403 holds the server the same way (a block isn't the page's fault), and
//! every request names the site in its User-Agent. A hold is never worked
//! around: a page whose batch route is held fails as transient and doesn't
//! go to the item API.
//!
//! **Item errors** (#414). Some items answer 503 at once, for hours, while
//! the rest of loc.gov answers normally: a fault in that item on LoC's
//! side, not a hold. An item JSON 503, or a METS or JP2 503 on the storage
//! service, counts as an item error, with nothing held, only when all of
//! these are true:
//! - on the storage service, it is the second 503 for that file, after the
//!   first held the server as usual (503s under load look the same there);
//! - it came back within [`ITEM_ERROR_FAST`], with no `Retry-After`;
//! - another request to that server answered 2xx within
//!   [`ITEM_ERROR_OK_WITHIN`], or one paced request for the last small file
//!   that answered 2xx there (the *known-good* item) answers 2xx now;
//! - fewer than [`ITEM_ERROR_TITLES`] different titles, this one included,
//!   had item errors on that server in the last [`ITEM_ERROR_WINDOW`].
//!
//! Anything else (a 429, a 403, a `Retry-After`, a slow 503, a 503 on the
//! image server or for a batch manifest, a failed check, or item errors
//! spread over several titles) holds the server as before. When unsure, it
//! holds.

use std::collections::BTreeMap;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
#[cfg(test)]
use opentelemetry::KeyValue;
#[cfg(test)]
use reqwest::StatusCode;
use serde_json::Value;
#[cfg(test)]
use tokio::time::Instant;
use usnm_core::ids::PageKey;

use crate::telemetry::Metrics;

mod batch_route;
pub mod bucket;
mod fetch;
pub mod global;
pub mod mirror;
mod pacer;
mod types;

pub use batch_route::{Bag, BatchPage};
pub use pacer::Pacer;
pub use types::{
    iiif_base, image_url, issue_id, page_images, FetchError, ImageSize, LocConfig, Scan,
    ScanFormat, Server, IIIF_ROOT, MAX_IMAGE_BYTES, STORAGE_SERVICE_ROOT,
};

use fetch::{challenge, html_page, retry_after, ApiHealth, Got};

pub const USER_AGENT: &str = concat!(
    "usnewsmap-ocr-worker/",
    env!("CARGO_PKG_VERSION"),
    " (+https://usnewsmap.com)"
);

/// The largest item JSON read.
const MAX_ITEM_BYTES: usize = 16 * 1024 * 1024;
/// The largest batch manifest read (a 10,000-page batch's is about 3 MB).
const MAX_BAG_BYTES: usize = 128 * 1024 * 1024;
/// The largest issue METS read (about 16 KB; a signed one 55 KB).
const MAX_METS_BYTES: usize = 4 * 1024 * 1024;
/// A file at most this big may be asked for again to check that its
/// server is answering (the known-good item).
const PROBE_MAX_BYTES: usize = 1024 * 1024;
/// The longest `Retry-After` honoured.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);
/// An item JSON 503 slower than this is never an item error.
pub const ITEM_ERROR_FAST: Duration = Duration::from_secs(2);
/// Another item's 2xx this recent shows loc.gov is serving.
pub const ITEM_ERROR_OK_WITHIN: Duration = Duration::from_secs(60);
/// Item errors are remembered this long, by title.
pub const ITEM_ERROR_WINDOW: Duration = Duration::from_secs(30 * 60);
/// Item errors from this many titles inside the window look like loc.gov,
/// not the items: the next one holds the server.
pub const ITEM_ERROR_TITLES: usize = 3;

type Pages = Arc<BTreeMap<u16, String>>;

pub struct LocClient {
    cfg: LocConfig,
    http: reqwest::Client,
    /// The same, following no redirect, for [`LocClient::locate`].
    http_here: reqwest::Client,
    api: Pacer,
    tile: Arc<Pacer>,
    storage: Arc<Pacer>,
    /// The host's budget (#962), also held and slowed by LoC's push-backs.
    global: Option<Arc<global::GlobalPacer>>,
    items: moka::future::Cache<String, Pages>,
    /// By batch directory; `None` for a batch LoC doesn't have.
    bags: moka::future::Cache<String, Option<Arc<Bag>>>,
    /// By `<batch dir>/<lccn>/<issue dir>`; `None` for an issue whose METS
    /// isn't there.
    mets: moka::future::Cache<String, Option<Pages>>,
    /// LoC's bucket, when configured.
    bucket: Option<bucket::Bucket>,
    /// By batch directory: where Chronicling America's redirect put a
    /// batch whose URL was a guess ([`LocClient::relocate`]); `None` where
    /// it named nowhere on the storage service.
    located: moka::future::Cache<String, Option<String>>,
    /// Batches whose guessed URL is being kept as checked right now
    /// ([`LocClient::confirm`]).
    confirming: Mutex<std::collections::HashSet<String>>,
    health: Mutex<ApiHealth>,
    storage_health: Mutex<ApiHealth>,
    metrics: Metrics,
}

impl std::fmt::Debug for LocClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocClient")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl LocClient {
    pub fn new(cfg: LocConfig) -> anyhow::Result<Self> {
        let http_here = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(180))
            // Redirects within a host only. Chronicling America's batch
            // redirect to the storage service is followed by hand
            // ([`LocClient::locate`]), once checked and paced.
            .redirect(reqwest::redirect::Policy::custom(|a| {
                let same = a.previous().last().is_some_and(|p| {
                    p.host_str() == a.url().host_str() && p.scheme() == a.url().scheme()
                });
                if a.previous().len() >= 5 || !same {
                    a.stop()
                } else {
                    a.follow()
                }
            }))
            .build()?;
        let global = cfg
            .global_pacer
            .clone()
            .map(|p| Arc::new(global::GlobalPacer::new(p)));
        let (tile, storage) = if cfg.storage_own_budget {
            (
                Arc::new(Pacer::with_global(cfg.tile_gap, global.clone())),
                Arc::new(Pacer::with_global(cfg.storage_gap, global.clone())),
            )
        } else {
            let shared = Arc::new(Pacer::with_global(
                cfg.tile_gap.max(cfg.storage_gap),
                global.clone(),
            ));
            (Arc::clone(&shared), shared)
        };
        Ok(Self {
            api: Pacer::with_global(cfg.api_gap, global.clone()),
            global,
            tile,
            storage,
            items: moka::future::Cache::builder()
                .max_capacity(256)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            bags: moka::future::Cache::builder()
                .max_capacity(32)
                .time_to_live(Duration::from_secs(6 * 3600))
                .build(),
            mets: moka::future::Cache::builder()
                .max_capacity(4096)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            located: moka::future::Cache::builder()
                .max_capacity(4096)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            confirming: Mutex::new(std::collections::HashSet::new()),
            health: Mutex::new(ApiHealth::default()),
            storage_health: Mutex::new(ApiHealth::default()),
            bucket: cfg.bucket.clone().map(bucket::Bucket::new).transpose()?,
            cfg,
            http,
            http_here,
            metrics: Metrics::global(),
        })
    }

    /// The item JSON URL of `key`'s issue.
    pub fn item_url(&self, key: &PageKey) -> String {
        format!("{}/item/{}/?fo=json", self.cfg.api_root, issue_id(key))
    }

    /// seq → IIIF base for `key`'s issue, from the cache or one API request.
    /// Only answers are cached: a 4xx on the issue fails this page and is
    /// asked again for the next, so a refusal can't empty a whole issue.
    async fn issue_pages(&self, key: &PageKey) -> Result<Pages, FetchError> {
        let issue = issue_id(key);
        let url = self.item_url(key);
        self.items
            .try_get_with(issue, async {
                let got = self
                    .get(&url, Server::Api, MAX_ITEM_BYTES, Some(&key.lccn))
                    .await?;
                let item: Value = serde_json::from_slice(&got.body)
                    .map_err(|e| FetchError::Transient(format!("item JSON for {url}: {e}")))?;
                Ok(Arc::new(page_images(&self.cfg.iiif_root, &item)))
            })
            .await
            .map_err(|e: Arc<FetchError>| e.cloned())
    }

    /// The page's full-size JPEG, through the item API.
    pub async fn scan(&self, key: &PageKey) -> Result<Scan, FetchError> {
        self.scan_at(key, ImageSize::Full).await
    }

    /// The page's JPEG at `size` ([`image_url`]), through the item API.
    pub async fn scan_at(&self, key: &PageKey, size: ImageSize) -> Result<Scan, FetchError> {
        let pages = self.issue_pages(key).await?;
        let base = pages
            .get(&key.seq)
            .cloned()
            .ok_or_else(|| FetchError::Permanent(format!("loc.gov lists no image for {key}")))?;
        self.iiif_scan(base, size).await
    }

    /// The page's IIIF service base, through the item API (one request per
    /// issue), so a caller can pace the image request itself.
    pub async fn iiif_base_of(&self, key: &PageKey) -> Result<String, FetchError> {
        let pages = self.issue_pages(key).await?;
        pages
            .get(&key.seq)
            .cloned()
            .ok_or_else(|| FetchError::Permanent(format!("loc.gov lists no image for {key}")))
    }

    /// A file on the storage service (an ALTO next to its JP2), paced like
    /// the JP2s; `None` for a 404 or 410.
    pub async fn storage_file(
        &self,
        url: &str,
        title: &str,
        max: usize,
    ) -> Result<Option<Bytes>, FetchError> {
        Ok(self
            .fetch(url, Server::Storage, max, Some(title), true)
            .await?
            .map(|g| g.body))
    }

    /// The JPEG at `size` of the page with IIIF base `base`.
    pub async fn iiif_scan(&self, base: String, size: ImageSize) -> Result<Scan, FetchError> {
        let url = image_url(&base, size);
        let got = self.get(&url, Server::Tile, MAX_IMAGE_BYTES, None).await?;
        Ok(jpeg(base, url, got))
    }
}

/// The IIIF JPEG at `url` of the page with IIIF base `base`.
fn jpeg(base: String, url: String, got: Got) -> Scan {
    Scan {
        iiif_base: base,
        url,
        format: ScanFormat::Jpeg,
        bytes: got.body,
        md5: None,
        download: got.took,
        from_cache: false,
        evict: None,
    }
}

/// An error and its causes, for a log line: reqwest's own message
/// ("error sending request") says nothing of why.
pub(crate) fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cause = c.source();
    }
    out
}

pub(crate) async fn read_limited(mut resp: reqwest::Response, max: usize) -> Result<Bytes, String> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(format!("response over {max} bytes"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| error_chain(&e.without_url()))?
    {
        if body.len() + chunk.len() > max {
            return Err(format!("response over {max} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

#[cfg(test)]
mod tests;
