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
//! were on disk. When LoC answers 404 for a file under a guessed URL,
//! Chronicling America's redirect is asked where the batch is (as for a
//! manifest LoC doesn't have at the guessed path), and the batch's URL is
//! moved there and kept as checked before anything goes to the item API.
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use opentelemetry::KeyValue;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::time::Instant;
use usnm_core::ids::PageKey;

use crate::batch::{self, BagIndex, Md5Sum};
use crate::telemetry::Metrics;

pub mod bucket;
pub mod global;
pub mod mirror;
mod pacer;
mod types;

pub use pacer::Pacer;
pub use types::{
    iiif_base, image_url, issue_id, page_images, FetchError, ImageSize, LocConfig, Scan,
    ScanFormat, Server, IIIF_ROOT, MAX_IMAGE_BYTES, STORAGE_SERVICE_ROOT,
};

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

/// Beside a batch's kept manifest: its directory on the storage service, as
/// LoC served the manifest there or Chronicling America's redirect named it.
const URL_FILE: &str = "url";
/// Beside a manifest from LoC's bucket instead: only the guess the batch's
/// name gives ([`Bag::guessed`]).
const GUESSED_URL_FILE: &str = "url.guessed";
/// A guess LoC's checked URL later replaced: the batch's earlier sidecars,
/// fetched under the guess, name it.
const WRONG_URL_FILE: &str = "url.wrong";

type Pages = Arc<BTreeMap<u16, String>>;

/// A batch's bag manifest, and where its files are.
#[derive(Debug)]
pub struct Bag {
    /// `<batch>_ver<NN>`.
    pub dir: String,
    /// The batch's directory on the storage service, ending in `/`.
    pub url: String,
    /// Whether `url` is only the guess the batch's name gives
    /// (`<aw>/batch_<dir>/`), under which a manifest from LoC's bucket was
    /// kept, with nothing from LoC to confirm it yet.
    pub guessed: bool,
    pub index: BagIndex,
}

/// A page found through its batch.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchPage {
    /// The JP2 on the storage service.
    pub url: String,
    pub iiif_base: String,
    /// The bag manifest's md5 for the JP2.
    pub md5: Md5Sum,
}

/// What a server's answers say about telling an item error from a hold.
#[derive(Debug, Default)]
struct ApiHealth {
    /// The last small file (at most [`PROBE_MAX_BYTES`]) that answered
    /// 2xx, or one set by [`LocClient::set_known_good`]: what a check asks
    /// for again.
    known_good: Option<String>,
    /// When anything on the server last answered 2xx.
    last_ok: Option<Instant>,
    /// Item errors lately: when, and the title.
    item_errors: std::collections::VecDeque<(Instant, String)>,
}

/// Why a known-good item cannot establish that the server is serving.
enum ProbeFailure {
    /// It failed too: a hold, for at least this long if it said so.
    Failed(Option<Duration>),
    /// A strict block already held and counted; stop the original fetch.
    Blocked(FetchError),
}

/// A successful GET.
#[derive(Debug)]
struct Got {
    body: Bytes,
    /// Sending to the end of the body.
    took: Duration,
}

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

/// A `Retry-After` value (seconds or an HTTP date), capped at an hour, and
/// whether the cap cut it.
fn retry_after(resp: &reqwest::Response) -> Option<(Duration, bool)> {
    let v = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    let d = usnm_core::retry_after::parse(v, std::time::SystemTime::now())?;
    Some((d.min(MAX_RETRY_AFTER), d > MAX_RETRY_AFTER))
}

/// Write `body` to `path` through a `.part` file and a rename.
async fn write_file(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    tokio::fs::write(&part, body).await?;
    tokio::fs::rename(&part, path).await
}

/// Keep `url` as the checked URL of the batch kept in `dir`, in place of
/// any guess. A guess that was right goes; a wrong one is kept as
/// `url.wrong`, the record of which sidecars name it.
async fn write_checked_url(dir: &Path, url: &str) -> std::io::Result<()> {
    write_file(&dir.join(URL_FILE), url.as_bytes()).await?;
    let guessed = dir.join(GUESSED_URL_FILE);
    match tokio::fs::read_to_string(&guessed).await {
        Ok(g) if g.trim() != url => {
            tracing::warn!(
                dir = %dir.display(),
                guess = g.trim(),
                url,
                "a batch's guessed URL was wrong: the sidecars fetched under it name the guess"
            );
            tokio::fs::rename(&guessed, dir.join(WRONG_URL_FILE)).await
        }
        Ok(_) => tokio::fs::remove_file(&guessed).await,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
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
            health: Mutex::new(ApiHealth::default()),
            storage_health: Mutex::new(ApiHealth::default()),
            bucket: cfg.bucket.clone().map(bucket::Bucket::new).transpose()?,
            cfg,
            http,
            http_here,
            metrics: Metrics::global(),
        })
    }

    /// LoC pushed back with a 429, a 403 or a challenge page: hold the
    /// host's whole budget for `d`, and drop it to the slow rate for good
    /// (the maintainer's rule of 24 October 2026: 45 a minute, 10 if LoC
    /// objects). A 503 or an unreadable body holds only its server.
    fn push_back(&self, status: StatusCode, challenged: bool, d: Duration) {
        if let Some(g) = &self.global {
            if challenged
                || matches!(
                    status,
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN
                )
            {
                g.hold(d);
                g.slow_down(&format!(
                    "HTTP {}{}",
                    status.as_u16(),
                    if challenged { " (challenge page)" } else { "" }
                ));
            }
        }
    }

    /// The main client follows same-host redirects on its own; each hop it
    /// followed is another LoC request, so it is charged to the global budget
    /// after the fact (the next slots move on).
    fn charged(
        &self,
        res: reqwest::Result<reqwest::Response>,
        asked: &str,
    ) -> reqwest::Result<reqwest::Response> {
        if let (Ok(r), Some(g)) = (&res, &self.global) {
            if r.url().as_str() != asked {
                g.charge(1);
            }
        }
        res
    }

    fn pacer(&self, server: Server) -> &Pacer {
        match server {
            Server::Api => &self.api,
            Server::Tile => &self.tile,
            Server::Storage => &self.storage,
        }
    }

    /// The item-error record of a server that has items (not the image
    /// server, whose 503s always hold).
    fn health(&self, server: Server) -> Option<&Mutex<ApiHealth>> {
        match server {
            Server::Api => Some(&self.health),
            Server::Storage => Some(&self.storage_health),
            Server::Tile => None,
        }
    }

    /// The item JSON URL of `key`'s issue.
    pub fn item_url(&self, key: &PageKey) -> String {
        format!("{}/item/{}/?fo=json", self.cfg.api_root, issue_id(key))
    }

    /// An item known to have answered 2xx before (from an earlier run),
    /// to check against when an item answers 503 before any item has
    /// answered in this process. One answered since takes its place.
    pub fn set_known_good(&self, url: String) {
        self.set_known_good_on(Server::Api, url);
    }

    /// The same, for `server`: a small file there, such as an issue METS.
    pub fn set_known_good_on(&self, server: Server, url: String) {
        if let Some(h) = self.health(server) {
            let mut h = h.lock().expect("health lock");
            if h.known_good.is_none() {
                h.known_good = Some(url);
            }
        }
    }

    /// Whether a fast, bare 503 for item `url` of `title` on `server` is
    /// an item error rather than a hold (the module notes), and if not, how
    /// long a failed check asked to wait. Records it if it is.
    async fn item_error(&self, server: Server, url: &str, title: &str) -> Result<(), ProbeFailure> {
        /// Whether item errors from too many titles, with this one, are
        /// inside the window.
        fn too_many_titles(h: &mut ApiHealth, title: &str) -> bool {
            let now = Instant::now();
            while h
                .item_errors
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > ITEM_ERROR_WINDOW)
            {
                h.item_errors.pop_front();
            }
            let mut titles: std::collections::BTreeSet<&str> =
                h.item_errors.iter().map(|(_, t)| t.as_str()).collect();
            titles.insert(title);
            if titles.len() >= ITEM_ERROR_TITLES {
                tracing::warn!(
                    titles = titles.len(),
                    "item errors from several titles: holding instead"
                );
                return true;
            }
            false
        }
        let Some(health) = self.health(server) else {
            return Err(ProbeFailure::Failed(None));
        };
        let check = {
            let mut h = health.lock().expect("health lock");
            if too_many_titles(&mut h, title) {
                return Err(ProbeFailure::Failed(None));
            }
            let now = Instant::now();
            if h.known_good.as_deref() == Some(url) {
                return Err(ProbeFailure::Failed(None));
            }
            match (&h.known_good, h.last_ok) {
                (_, Some(at)) if now.duration_since(at) <= ITEM_ERROR_OK_WITHIN => None,
                (Some(good), _) => Some(good.clone()),
                (None, _) => return Err(ProbeFailure::Failed(None)),
            }
        };
        if let Some(good) = check {
            self.probe(server, &good).await?;
        }
        // Checked again with the record, under one lock: callers that
        // waited on probes together can't all pass the titles rule.
        let mut h = health.lock().expect("health lock");
        if too_many_titles(&mut h, title) {
            return Err(ProbeFailure::Failed(None));
        }
        h.item_errors.push_back((Instant::now(), title.to_owned()));
        Ok(())
    }

    /// One paced request for the known-good item, outside the caches.
    async fn probe(&self, server: Server, url: &str) -> Result<(), ProbeFailure> {
        self.pacer(server).wait().await;
        match self.charged(self.http.get(url).send().await, url) {
            Ok(resp) if self.cfg.html_is_block && html_page(&resp).is_some() => {
                self.block_hold(server, resp.status(), "html_challenge", retry_after(&resp));
                Err(ProbeFailure::Blocked(FetchError::Transient(format!(
                    "{url}: known-good item answered HTTP {} with {}; a bot challenge or block page; holding",
                    resp.status().as_u16(),
                    html_page(&resp).expect("checked HTML response")
                ))))
            }
            Ok(resp) if resp.status().is_success() => {
                let status = resp.status();
                let asked = retry_after(&resp);
                match read_limited(resp, MAX_ITEM_BYTES).await {
                    Ok(_) => {
                        if let Some(h) = self.health(server) {
                            let mut h = h.lock().expect("health lock");
                            h.known_good = Some(url.to_owned());
                            h.last_ok = Some(Instant::now());
                        }
                        Ok(())
                    }
                    Err(e) if self.cfg.html_is_block => {
                        self.block_hold(server, status, "unreadable_body", asked);
                        Err(ProbeFailure::Blocked(FetchError::Transient(format!(
                            "{url}: known-good item's body could not be read ({e}); a possible block or challenge; holding"
                        ))))
                    }
                    Err(_) => Err(ProbeFailure::Failed(None)),
                }
            }
            Ok(resp) => {
                tracing::warn!(
                    status = resp.status().as_u16(),
                    server = server.name(),
                    "the known-good item failed too"
                );
                // A 429 or 403 here is LoC pushing back, as on any request.
                if matches!(
                    resp.status(),
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN
                ) {
                    let d = retry_after(&resp)
                        .map_or(self.cfg.backoff, |(d, _)| d.max(self.cfg.backoff));
                    self.pacer(server).hold(d);
                    self.push_back(resp.status(), false, d);
                }
                Err(ProbeFailure::Failed(retry_after(&resp).map(|(d, _)| d)))
            }
            Err(e) => {
                tracing::warn!(error = %e.without_url(), server = server.name(), "the known-good item failed too");
                Err(ProbeFailure::Failed(None))
            }
        }
    }

    /// A 2xx from `server` for an item of a title: it is serving. Only a
    /// small answer becomes the known-good item, so a check never asks for
    /// megabytes.
    fn answered(&self, server: Server, url: &str, len: usize) {
        if let Some(h) = self.health(server) {
            let mut h = h.lock().expect("health lock");
            h.last_ok = Some(Instant::now());
            if len <= PROBE_MAX_BYTES {
                h.known_good = Some(url.to_owned());
            }
        }
    }

    /// GET `url`, paced by `server`'s pacer, retrying transient failures.
    /// `title` is set for an item (an item JSON, a METS, a JP2), which can
    /// end in [`FetchError::Item`].
    async fn get(
        &self,
        url: &str,
        server: Server,
        max: usize,
        title: Option<&str>,
    ) -> Result<Got, FetchError> {
        self.fetch(url, server, max, title, false)
            .await
            .map(|g| g.expect("a 404 is an error unless asked for"))
    }

    fn block_hold(
        &self,
        server: Server,
        status: StatusCode,
        reason: &'static str,
        asked: Option<(Duration, bool)>,
    ) {
        let d = asked.map_or(self.cfg.backoff, |(d, _)| d.max(self.cfg.backoff));
        self.pacer(server).hold(d);
        if reason == "html_challenge" {
            self.push_back(status, true, d);
        }
        self.metrics.loc_slowdowns.add(
            1,
            &[
                KeyValue::new("status", i64::from(status.as_u16())),
                KeyValue::new("server", server.name()),
                KeyValue::new("reason", reason),
                KeyValue::new("retry_after", asked.is_some()),
                KeyValue::new("capped", asked.is_some_and(|(_, capped)| capped)),
            ],
        );
    }

    /// [`Self::get`], with `missing_ok` answering `None` for a 404 or 410.
    async fn fetch(
        &self,
        url: &str,
        server: Server,
        max: usize,
        title: Option<&str>,
        missing_ok: bool,
    ) -> Result<Option<Got>, FetchError> {
        let pacer = self.pacer(server);
        let mut last = String::new();
        // The last answer for this file was a fast, bare 503.
        let mut bare_before = false;
        for attempt in 1..=self.cfg.attempts.max(1) {
            // Whether the answer just before this one was a fast, bare
            // 503; any other answer in between clears it.
            let prev_bare = std::mem::take(&mut bare_before);
            pacer.wait().await;
            let wait = self.cfg.backoff * attempt;
            let sent = Instant::now();
            match self.charged(self.http.get(url).send().await, url) {
                Ok(resp) if resp.status().is_success() => {
                    if self.cfg.html_is_block {
                        if let Some(why) = html_page(&resp) {
                            self.block_hold(
                                server,
                                resp.status(),
                                "html_challenge",
                                retry_after(&resp),
                            );
                            tracing::warn!(
                                url,
                                server = server.name(),
                                "an HTML page instead of the file: holding"
                            );
                            return Err(FetchError::Transient(format!(
                                "{url} answered HTTP {} with {why} instead of the file: a bot challenge or block page; holding",
                                resp.status().as_u16()
                            )));
                        }
                    }
                    let status = resp.status();
                    let asked = retry_after(&resp);
                    match read_limited(resp, max).await {
                        Ok(b) => {
                            if title.is_some() {
                                self.answered(server, url, b.len());
                            }
                            return Ok(Some(Got {
                                body: b,
                                took: sent.elapsed(),
                            }));
                        }
                        Err(e) if self.cfg.html_is_block => {
                            self.block_hold(server, status, "unreadable_body", asked);
                            return Err(FetchError::Transient(format!(
                            "{url}: the answer's body could not be read ({e}), which is how a block or challenge page can look; holding"
                        )));
                        }
                        Err(e) => last = e,
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    if missing_ok && matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
                        return Ok(None);
                    }
                    // A 403 is how a block looks: hold the server as for a
                    // 429, rather than dead-letter pages at the paced rate.
                    if matches!(
                        status,
                        StatusCode::TOO_MANY_REQUESTS
                            | StatusCode::SERVICE_UNAVAILABLE
                            | StatusCode::FORBIDDEN
                    ) {
                        let asked = retry_after(&resp);
                        let mut checked = None;
                        // Any Retry-After, even one that can't be read, is
                        // a hold.
                        let bare = status == StatusCode::SERVICE_UNAVAILABLE
                            && !resp.headers().contains_key(reqwest::header::RETRY_AFTER)
                            && !(self.cfg.html_is_block && challenge(&resp).is_some())
                            && sent.elapsed() < ITEM_ERROR_FAST;
                        // On the storage service, where 503s under load
                        // look the same, only a second fast, bare 503 for
                        // the same file in a row, after the first held, can
                        // be an item error.
                        let may_be_item =
                            self.cfg.item_probes && (server == Server::Api || prev_bare);
                        bare_before = bare;
                        if let (Some(title), Some(_), true, true) =
                            (title, self.health(server), may_be_item, bare)
                        {
                            match self.item_error(server, url, title).await {
                                Ok(()) => {
                                    tracing::warn!(
                                        url,
                                        title,
                                        server = server.name(),
                                        "loc.gov answered 503 at once for this item while serving others: an item error, not holding"
                                    );
                                    return Err(FetchError::Item(format!(
                                        "{url} returned HTTP 503 at once while other items answered"
                                    )));
                                }
                                Err(ProbeFailure::Failed(d)) => checked = d,
                                Err(ProbeFailure::Blocked(e)) => return Err(e),
                            }
                        }
                        let capped = asked.is_some_and(|(_, c)| c);
                        let d = asked.map_or(wait, |(r, _)| r.max(wait));
                        let d = checked.map_or(d, |c| c.min(MAX_RETRY_AFTER).max(d));
                        // Everyone waits, not just this request.
                        pacer.hold(d);
                        self.push_back(
                            status,
                            self.cfg.html_is_block && challenge(&resp).is_some(),
                            d,
                        );
                        self.metrics.loc_slowdowns.add(
                            1,
                            &[
                                KeyValue::new("status", i64::from(status.as_u16())),
                                KeyValue::new("server", server.name()),
                                KeyValue::new(
                                    "reason",
                                    if self.cfg.html_is_block && challenge(&resp).is_some() {
                                        "html_challenge"
                                    } else {
                                        "http_status"
                                    },
                                ),
                                KeyValue::new("retry_after", asked.is_some()),
                                // Retry-After over an hour, cut to one.
                                KeyValue::new("capped", capped),
                            ],
                        );
                        tracing::warn!(
                            status = status.as_u16(),
                            server = server.name(),
                            wait_secs = d.as_secs(),
                            capped,
                            "loc.gov asked us to slow down"
                        );
                        last = format!("HTTP {}", status.as_u16());
                        if self.cfg.html_is_block {
                            if let Some(why) = challenge(&resp) {
                                last = format!("{last} ({why}): blocked");
                            }
                        }
                        continue;
                    }
                    // 408 is a timeout, worth retrying like a 5xx.
                    if status.is_client_error() && status != StatusCode::REQUEST_TIMEOUT {
                        return Err(FetchError::Permanent(format!(
                            "{url} returned HTTP {}",
                            status.as_u16()
                        )));
                    }
                    last = format!("HTTP {}", status.as_u16());
                }
                Err(e) => last = e.without_url().to_string(),
            }
            if attempt < self.cfg.attempts {
                tracing::warn!(url, attempt, error = %last, wait_secs = wait.as_secs(), "retrying");
                tokio::time::sleep(wait).await;
            }
        }
        Err(FetchError::Transient(format!("giving up on {url}: {last}")))
    }

    /// Until when loc.gov's API, image server or storage service holds this
    /// node's requests.
    pub fn held_until(&self) -> Option<Instant> {
        [
            self.api.held_until(),
            self.tile.held_until(),
            self.storage.held_until(),
        ]
        .into_iter()
        .flatten()
        .max()
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

    /// Where the batch's files go in the disk cache.
    fn cache_dir(&self, dir: &str) -> Option<PathBuf> {
        self.cfg.batch_cache.as_ref().map(|c| c.join(dir))
    }

    /// The batch's bag manifest: from the disk cache, or fetched once (the
    /// guessed storage path, then Chronicling America's redirect). `None`
    /// when LoC has no such batch.
    pub async fn bag(&self, batch: &str, version: u16) -> Result<Option<Arc<Bag>>, FetchError> {
        let Some(dir) = batch::batch_dir(batch, version) else {
            return Ok(None);
        };
        self.bags
            .try_get_with(dir.clone(), async { self.load_bag(&dir).await })
            .await
            .map_err(|e: Arc<FetchError>| e.cloned())
    }

    /// Where `url` redirects, without following it: one paced request on
    /// the storage service's pacer. `None` if it answers anything but a
    /// redirect (a 404, or the file itself, which isn't the storage
    /// service). A 429, 503 or 403 holds the storage service as any of its
    /// answers would.
    async fn locate(&self, url: &str) -> Result<Option<String>, FetchError> {
        let mut last = "held".to_owned();
        for attempt in 1..=self.cfg.attempts.max(1) {
            self.storage.wait().await;
            let resp = match self.http_here.get(url).send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt == self.cfg.attempts.max(1) {
                        return Err(FetchError::Transient(format!(
                            "giving up on {url}: {}",
                            e.without_url()
                        )));
                    }
                    tokio::time::sleep(self.cfg.backoff * attempt).await;
                    continue;
                }
            };
            let status = resp.status();
            if status.is_redirection() {
                let to = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| resp.url().join(l).ok());
                return Ok(to.map(|u| u.to_string()));
            }
            if matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::FORBIDDEN
            ) {
                let wait = self.cfg.backoff * attempt;
                let d = retry_after(&resp).map_or(wait, |(r, _)| r.max(wait));
                self.storage.hold(d);
                let why = self.cfg.html_is_block.then(|| challenge(&resp)).flatten();
                self.push_back(status, why.is_some(), d);
                self.metrics.loc_slowdowns.add(
                    1,
                    &[
                        KeyValue::new("status", i64::from(status.as_u16())),
                        KeyValue::new("server", Server::Storage.name()),
                        KeyValue::new(
                            "reason",
                            if why.is_some() {
                                "html_challenge"
                            } else {
                                "http_status"
                            },
                        ),
                        KeyValue::new(
                            "retry_after",
                            resp.headers().contains_key(reqwest::header::RETRY_AFTER),
                        ),
                        KeyValue::new("capped", retry_after(&resp).is_some_and(|(_, c)| c)),
                    ],
                );
                last = format!("HTTP {}", status.as_u16());
                if let Some(why) = why {
                    last = format!("{last} ({why}): blocked");
                }
                tracing::warn!(
                    status = status.as_u16(),
                    server = "storage",
                    wait_secs = d.as_secs(),
                    "loc.gov asked us to slow down"
                );
                continue;
            }
            if self.cfg.html_is_block {
                // Only a 404 or 410 is an absent batch; a challenge
                // page or a server error is a block, never "not there".
                if status.is_success() {
                    if let Some(why) = html_page(&resp) {
                        self.block_hold(
                            Server::Storage,
                            status,
                            "html_challenge",
                            retry_after(&resp),
                        );
                        return Err(FetchError::Transient(format!(
                            "{url} answered HTTP {} with {why} instead of a redirect: a bot challenge or block page; holding",
                            status.as_u16()
                        )));
                    }
                }
                if status.is_server_error() {
                    self.storage.hold(self.cfg.backoff);
                    return Err(FetchError::Transient(format!(
                        "{url} returned HTTP {}; holding",
                        status.as_u16()
                    )));
                }
            }
            return Ok(None);
        }
        Err(FetchError::Transient(format!("giving up on {url}: {last}")))
    }

    /// Seed the storage service's known-good item from the last JP2 an
    /// earlier run fetched: its issue's METS, as kept in the batch cache
    /// (the plain or the signed one, whichever is there), so a check never
    /// asks for a file that isn't on LoC.
    pub fn seed_storage_known_good(&self, jp2_url: &str) {
        let Some(rel) = jp2_url.strip_prefix(&self.cfg.storage_root) else {
            return;
        };
        // <inst>/batch_<dir>/data/<lccn>/<reel>/<issue>/<frame>.jp2
        let segs: Vec<&str> = rel.split('/').collect();
        let [_, bdir, "data", lccn, _, issue, _] = segs[..] else {
            return;
        };
        let (Some(bdir), Some(cache), Some(base)) = (
            bdir.strip_prefix("batch_"),
            self.cfg.batch_cache.as_ref(),
            jp2_url.rsplit_once('/').map(|(b, _)| b),
        ) else {
            return;
        };
        // Only from the batch's checked URL: a JP2 fetched under a guess
        // (its manifest came from LoC's bucket), right or wrong, may name a
        // METS LoC doesn't have there.
        let kept = cache.join(bdir);
        match std::fs::read_to_string(kept.join(URL_FILE)) {
            Ok(u) if !jp2_url.starts_with(u.trim()) => return,
            Ok(_) => {}
            Err(_) if kept.join(GUESSED_URL_FILE).is_file() => return,
            Err(_) => {}
        }
        for name in [format!("{issue}.xml"), format!("{issue}_1.xml")] {
            if cache
                .join(bdir)
                .join("mets")
                .join(lccn)
                .join(&name)
                .is_file()
            {
                self.set_known_good_on(Server::Storage, format!("{base}/{name}"));
                return;
            }
        }
    }

    /// A batch URL kept in `path`, if it is one under the storage root.
    async fn url_on_disk(&self, path: &Path) -> Option<String> {
        let url = tokio::fs::read_to_string(path).await.ok()?;
        let url = url.trim();
        (url.starts_with(&self.cfg.storage_root) && url.ends_with('/')).then(|| url.to_owned())
    }

    async fn load_bag(&self, dir: &str) -> Result<Option<Arc<Bag>>, FetchError> {
        let cache = self.cache_dir(dir);
        // A checked URL on disk, if there is one.
        let mut checked = None;
        if let Some(c) = &cache {
            checked = self.url_on_disk(&c.join(URL_FILE)).await;
            // A guess is used only while LoC's bucket is: without it, the
            // batch is looked up on LoC as if nothing were on disk.
            let guessed = match (&checked, &self.bucket) {
                (None, Some(_)) => self.url_on_disk(&c.join(GUESSED_URL_FILE)).await,
                _ => None,
            };
            let url = checked
                .clone()
                .map(|u| (u, false))
                .or(guessed.map(|u| (u, true)));
            if let Some((url, guessed)) = url {
                if let Ok(text) = tokio::fs::read_to_string(c.join("manifest-md5.txt")).await {
                    return Ok(Some(Arc::new(Bag {
                        dir: dir.to_owned(),
                        url,
                        guessed,
                        index: BagIndex::parse(&text),
                    })));
                }
            }
        }
        let guess = batch::storage_batch_url(&self.cfg.storage_root, dir);
        // LoC's bucket first, for a batch it holds: the same file. Its keys
        // don't name the awardee, so unless a checked URL is on disk, the
        // batch's URL on LoC is only the guess its name gives, and is kept
        // as one.
        let mut found = match &self.bucket {
            Some(b) => b.manifest(dir).await.map(|body| match &checked {
                Some(url) => (url.clone(), body, bucket::NAME, false),
                None => (guess.clone(), body, bucket::NAME, true),
            }),
            None => None,
        };
        if found.is_none() {
            found = self
                .fetch(
                    &format!("{guess}manifest-md5.txt"),
                    Server::Storage,
                    MAX_BAG_BYTES,
                    None,
                    true,
                )
                .await?
                .map(|g| (guess.clone(), g.body, Server::Storage.name(), false));
        }
        if found.is_none() {
            // Chronicling America knows where the batch is: follow it, but
            // only onto the storage service.
            let via = format!("{}{dir}/manifest-md5.txt", self.cfg.batches_root);
            if let Some(to) = self.locate(&via).await? {
                match to
                    .strip_suffix("manifest-md5.txt")
                    .filter(|u| u.starts_with(&self.cfg.storage_root))
                {
                    Some(u) => {
                        found = self
                            .fetch(&to, Server::Storage, MAX_BAG_BYTES, None, true)
                            .await?
                            .map(|g| (u.to_owned(), g.body, Server::Storage.name(), false));
                    }
                    None => {
                        tracing::warn!(
                            batch = dir,
                            to,
                            "the batch's redirect leaves the storage service; not following it"
                        );
                    }
                }
            }
        }
        let Some((url, body, from, guessed)) = found else {
            tracing::warn!(
                batch = dir,
                "LoC has no manifest for this batch; its pages go through the item API"
            );
            return Ok(None);
        };
        let text = String::from_utf8_lossy(&body).into_owned();
        let index = BagIndex::parse(&text);
        tracing::info!(
            batch = dir,
            url,
            url_guessed = guessed,
            from,
            issues = index.issues(),
            jp2s = index.jp2s,
            conflicts = index.conflicts,
            "batch manifest read"
        );
        if let Some(c) = &cache {
            let w = async {
                write_file(&c.join("manifest-md5.txt"), text.as_bytes()).await?;
                if guessed {
                    write_file(&c.join(GUESSED_URL_FILE), url.as_bytes()).await
                } else {
                    write_checked_url(c, &url).await
                }
            };
            if let Err(e) = w.await {
                tracing::warn!(batch = dir, error = %e, "can't keep the batch manifest on disk");
            }
        }
        Ok(Some(Arc::new(Bag {
            dir: dir.to_owned(),
            url,
            guessed,
            index,
        })))
    }

    /// Where batch `bag` is, when its URL is only a guess and LoC answered
    /// 404 for a file under it: Chronicling America's redirect
    /// ([`Self::locate`], one paced request, as for a manifest LoC doesn't
    /// have at the guessed path), asked once per batch at a time and
    /// remembered for an hour. Its answer becomes the batch's checked URL,
    /// on disk and in memory. `Some` only when the batch is elsewhere; when
    /// the redirect confirms the guess, or names nowhere on the storage
    /// service, the 404 stands.
    async fn relocate(&self, bag: &Bag) -> Result<Option<Arc<Bag>>, FetchError> {
        if !bag.guessed {
            return Ok(None);
        }
        // Done already, for a caller that still has the guess.
        if let Some(Some(now)) = self.bags.get(&bag.dir).await {
            if !now.guessed {
                return Ok((now.url != bag.url).then_some(now));
            }
        }
        let dir = bag.dir.as_str();
        let to = self
            .located
            .try_get_with(dir.to_owned(), async {
                let via = format!("{}{dir}/manifest-md5.txt", self.cfg.batches_root);
                let to = self.locate(&via).await?.and_then(|to| {
                    to.strip_suffix("manifest-md5.txt")
                        .filter(|u| u.starts_with(&self.cfg.storage_root))
                        .map(str::to_owned)
                });
                if to.is_none() {
                    tracing::warn!(
                        batch = dir,
                        url = bag.url,
                        "LoC answered 404 under the guessed URL of a batch whose manifest came from LoC's bucket, and Chronicling America names no other place on the storage service; keeping the guess"
                    );
                }
                Ok(to)
            })
            .await
            .map_err(|e: Arc<FetchError>| e.cloned())?;
        let Some(url) = to else {
            return Ok(None);
        };
        let moved = url != bag.url;
        if moved {
            tracing::warn!(
                batch = dir,
                guessed = bag.url,
                url,
                "a batch whose manifest came from LoC's bucket isn't where its name says: moved to where Chronicling America's redirect says (pages of it already fetched keep the guessed URL in their sidecars)"
            );
        } else {
            tracing::info!(
                batch = dir,
                url,
                "Chronicling America confirms a batch's guessed URL"
            );
        }
        if let Some(c) = self.cache_dir(dir) {
            if let Err(e) = write_checked_url(&c, &url).await {
                tracing::warn!(batch = dir, error = %e, "can't keep the batch's URL on disk");
            }
        }
        let checked = Arc::new(Bag {
            dir: bag.dir.clone(),
            url,
            guessed: false,
            index: bag.index.clone(),
        });
        self.bags
            .insert(bag.dir.clone(), Some(Arc::clone(&checked)))
            .await;
        Ok(moved.then_some(checked))
    }

    /// `page` where its batch is, when it was placed under its batch's
    /// guessed URL, LoC answered 404 for it there, and the batch is
    /// elsewhere ([`Self::relocate`]). Only with LoC's bucket, the one
    /// source of guesses.
    async fn moved(&self, page: &BatchPage) -> Result<Option<BatchPage>, FetchError> {
        if self.bucket.is_none() {
            return Ok(None);
        }
        let Some((dir, _)) = bucket::key(&self.cfg.storage_root, &page.url) else {
            return Ok(None);
        };
        let guess = batch::storage_batch_url(&self.cfg.storage_root, dir);
        let Some(rest) = page.url.strip_prefix(&guess) else {
            return Ok(None);
        };
        let Some(bag) = self
            .bags
            .try_get_with(dir.to_owned(), async { self.load_bag(dir).await })
            .await
            .map_err(|e: Arc<FetchError>| e.cloned())?
        else {
            return Ok(None);
        };
        let to = if bag.guessed {
            self.relocate(&bag).await?
        } else {
            // Moved already, after this page was placed under the guess.
            (bag.url != guess).then_some(bag)
        };
        let Some(to) = to else {
            return Ok(None);
        };
        let url = format!("{}{rest}", to.url);
        Ok(
            iiif_base(&self.cfg.iiif_root, &url).map(|iiif_base| BatchPage {
                url,
                iiif_base,
                md5: page.md5,
            }),
        )
    }

    /// seq → frame for an issue of `bag`, from its METS (checked against
    /// the bag's md5), kept on disk and in memory. `None` if the METS isn't
    /// there; one that doesn't parse is an error.
    async fn issue_frames(
        &self,
        bag: &Bag,
        lccn: &str,
        dir: &str,
    ) -> Result<Option<Pages>, FetchError> {
        let Some(files) = bag.index.issue(lccn, dir) else {
            return Ok(None);
        };
        let Some((name, md5)) = files.mets_file(dir) else {
            return Ok(None);
        };
        let key = format!("{}/{lccn}/{dir}", bag.dir);
        let disk = self
            .cache_dir(&bag.dir)
            .map(|c| c.join("mets").join(lccn).join(&name));
        let url = batch::file_url(&bag.url, lccn, &files.reel, dir, &name);
        // Its path in the bag, for LoC's bucket.
        let rel = batch::file_url("", lccn, &files.reel, dir, &name);
        self.mets
            .try_get_with(key, async {
                let mut url = url;
                let mut body = None;
                if let Some(p) = &disk {
                    if let Ok(b) = tokio::fs::read(p).await {
                        if batch::md5(&b) == md5 {
                            body = Some(Bytes::from(b));
                        }
                    }
                }
                let body = match body {
                    Some(b) => b,
                    None => {
                        // LoC's bucket first, checked against the bag's md5
                        // the same way.
                        let from_bucket = match &self.bucket {
                            Some(b) => b.mets(&bag.dir, &rel, &md5).await,
                            None => None,
                        };
                        let body = match from_bucket {
                            Some(b) => b,
                            None => {
                                let mut got = self
                                    .fetch(&url, Server::Storage, MAX_METS_BYTES, Some(lccn), true)
                                    .await?;
                                // Not under a guessed URL: the batch may
                                // be elsewhere.
                                if got.is_none() {
                                    if let Some(moved) = self.relocate(bag).await? {
                                        url = batch::file_url(
                                            &moved.url,
                                            lccn,
                                            &files.reel,
                                            dir,
                                            &name,
                                        );
                                        got = self
                                            .fetch(
                                                &url,
                                                Server::Storage,
                                                MAX_METS_BYTES,
                                                Some(lccn),
                                                true,
                                            )
                                            .await?;
                                    }
                                }
                                let Some(got) = got else {
                                    tracing::warn!(url, "the issue's METS isn't there");
                                    return Ok(None);
                                };
                                if batch::md5(&got.body) != md5 {
                                    return Err(FetchError::Transient(format!(
                                        "{url}: md5 {} isn't the bag's {}",
                                        batch::md5_hex(&got.body),
                                        batch::hex(&md5)
                                    )));
                                }
                                got.body
                            }
                        };
                        if let Some(p) = &disk {
                            if let Err(e) = write_file(p, &body).await {
                                tracing::warn!(url, error = %e, "can't keep the METS on disk");
                            }
                        }
                        body
                    }
                };
                match batch::mets_pages(&body) {
                    Ok(p) => Ok(Some(Arc::new(p))),
                    Err(e) => {
                        // Not a page the batch route can't place: the METS
                        // is the bag's, so something is wrong; fail it
                        // rather than go round through the item API.
                        Err(FetchError::Transient(format!(
                            "{url}: can't read the issue's METS: {e}"
                        )))
                    }
                }
            })
            .await
            .map_err(|e: Arc<FetchError>| e.cloned())
    }

    /// Where `key`'s JP2 is, through its batch (`batch`, `version`), and
    /// its IIIF base and md5. `None` when the batch route can't place it:
    /// no such batch, the issue or its METS isn't in the bag, or the METS
    /// lists no JP2 for the seq. A hold or a failure is an error, never
    /// `None`, so it is never sent round the item API.
    pub async fn resolve(
        &self,
        key: &PageKey,
        batch: &str,
        version: u16,
    ) -> Result<Option<BatchPage>, FetchError> {
        let Some(mut bag) = self.bag(batch, version).await? else {
            return Ok(None);
        };
        let dir = batch::issue_dir(key.date, key.edition);
        let Some(frames) = self.issue_frames(&bag, &key.lccn, &dir).await? else {
            return Ok(None);
        };
        // Its METS may have shown a guessed URL to be wrong.
        if bag.guessed {
            if let Some(Some(now)) = self.bags.get(&bag.dir).await {
                bag = now;
            }
        }
        let Some(files) = bag.index.issue(&key.lccn, &dir) else {
            return Ok(None);
        };
        let Some(frame) = frames.get(&key.seq) else {
            return Ok(None);
        };
        let Some(md5) = files.jp2.get(frame) else {
            return Ok(None);
        };
        let url = batch::file_url(
            &bag.url,
            &key.lccn,
            &files.reel,
            &dir,
            &format!("{frame}.jp2"),
        );
        let Some(base) = iiif_base(&self.cfg.iiif_root, &url) else {
            return Ok(None);
        };
        Ok(Some(BatchPage {
            url,
            iiif_base: base,
            md5: *md5,
        }))
    }

    /// The page's JP2 from LoC's bucket, when it is configured and holds
    /// the page's batch, matching the bag's md5, with LoC's URL and IIIF
    /// base, and where the bytes came from. `None` sends the caller on, to
    /// a mirror or [`Self::scan_resolved`]. Only a full-size scan is a JP2.
    pub async fn bucket_jp2(&self, page: &BatchPage) -> Option<(Scan, mirror::Hit)> {
        self.bucket
            .as_ref()?
            .jp2(page, &self.cfg.storage_root)
            .await
    }

    /// Whether LoC's bucket is configured.
    pub fn has_bucket(&self) -> bool {
        self.bucket.is_some()
    }

    /// LoC's bucket's counts and hold, when it is configured.
    pub fn bucket_status(&self) -> Option<bucket::BucketStatus> {
        self.bucket.as_ref().map(bucket::Bucket::status)
    }

    /// The page `resolve` found: the JP2 from LoC's storage service, checked
    /// against the bag's md5, at full size; the IIIF JPEG of its base when
    /// scaled. `None` if the JP2 answers 404 though the bag lists it. A
    /// page placed under a guessed URL ([`Bag::guessed`]) that answers 404
    /// is asked again where Chronicling America says its batch is, and the
    /// scan carries that URL and IIIF base.
    pub async fn scan_resolved(
        &self,
        key: &PageKey,
        page: &BatchPage,
        size: ImageSize,
    ) -> Result<Option<Scan>, FetchError> {
        if let ImageSize::Pct(_) = size {
            if self.bucket.is_none() {
                return self.iiif_scan(page.iiif_base.clone(), size).await.map(Some);
            }
            // With LoC's bucket, the base may be under a guessed URL.
            let url = image_url(&page.iiif_base, size);
            if let Some(got) = self
                .fetch(&url, Server::Tile, MAX_IMAGE_BYTES, None, true)
                .await?
            {
                return Ok(Some(jpeg(page.iiif_base.clone(), url, got)));
            }
            return match self.moved(page).await? {
                Some(p) => self.iiif_scan(p.iiif_base, size).await.map(Some),
                None => Err(FetchError::Permanent(format!(
                    "{url} returned HTTP 404 or 410"
                ))),
            };
        }
        let jp2 = |url: String| async move {
            self.fetch(
                &url,
                Server::Storage,
                MAX_IMAGE_BYTES,
                Some(&key.lccn),
                true,
            )
            .await
        };
        let (page, got) = match jp2(page.url.clone()).await? {
            Some(got) => (page.clone(), got),
            None => {
                let moved = self.moved(page).await?;
                let got = match &moved {
                    Some(p) => jp2(p.url.clone()).await?,
                    None => None,
                };
                let (Some(p), Some(got)) = (moved, got) else {
                    tracing::warn!(url = page.url, "a JP2 the bag lists isn't there");
                    return Ok(None);
                };
                (p, got)
            }
        };
        let md5 = batch::md5(&got.body);
        if md5 != page.md5 {
            return Err(FetchError::Transient(format!(
                "{}: md5 {} isn't the bag's {}",
                page.url,
                batch::hex(&md5),
                batch::hex(&page.md5)
            )));
        }
        Ok(Some(Scan {
            iiif_base: page.iiif_base.clone(),
            url: page.url.clone(),
            format: ScanFormat::Jp2,
            bytes: got.body,
            md5: Some(batch::hex(&md5)),
            download: got.took,
            from_cache: false,
            evict: None,
        }))
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

/// Why a 2xx answer is a web page rather than a file: a challenge header
/// or HTML content type. None for images, JSON, XML and text manifests.
fn html_page(resp: &reqwest::Response) -> Option<String> {
    if resp
        .headers()
        .get("cf-mitigated")
        .is_some_and(|v| v == "challenge")
    {
        return challenge(resp);
    }
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)?
        .to_str()
        .ok()?
        .to_ascii_lowercase();
    (ct.starts_with("text/html") || ct.starts_with("application/xhtml")).then(|| {
        format!(
            "an HTML page ({})",
            challenge(resp).unwrap_or_else(|| ct.clone())
        )
    })
}

/// Whether a refusal is a bot challenge, by the headers Cloudflare sends
/// with one (`cf-mitigated: challenge`) or an HTML body from Cloudflare.
fn challenge(resp: &reqwest::Response) -> Option<String> {
    let h = resp.headers();
    let get = |k: &str| {
        h.get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_ascii_lowercase)
    };
    if get("cf-mitigated").is_some_and(|v| v.contains("challenge")) {
        return Some("a Cloudflare challenge".into());
    }
    let html = get("content-type").is_some_and(|c| c.starts_with("text/html"));
    (html && get("server").is_some_and(|s| s.contains("cloudflare")))
        .then(|| "an HTML page from Cloudflare".into())
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
        .map_err(|e| e.without_url().to_string())?
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
