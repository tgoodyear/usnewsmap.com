//! Second sources for a page's JP2: copies of LoC's own NDNP files kept
//! outside `tile.loc.gov`.
//!
//! - **State partners.** Four NDNP awardees serve their own batches with
//!   LoC's Chronicling America software (chronam or Open ONI), under LoC's
//!   page URLs: `/lccn/{lccn}/{date}/ed-{n}/seq-{n}.jp2`. Nebraska (`nbu`),
//!   Pennsylvania (`pst`), Oregon (`oru`) and South Carolina (`scu`). Each
//!   serves only its own awardee's batches. A partner may hold more pages
//!   in a batch than LoC lists, so its seq can drift from LoC's: where its
//!   page JSON names the file (the IIIF id), that name must be LoC's.
//! - **The Wayback Machine.** Captures of the JP2s on LoC's storage
//!   service, a few percent of each batch. One CDX lookup per issue lists
//!   the captured files (pages of an issue asked at once share it); a
//!   capture is fetched as it was (`id_`), the largest first, since some
//!   captures were cut short.
//!
//! The page itself is always placed by LoC: its frame and md5 come from
//! LoC's METS and bag manifest ([`super::LocClient::resolve`]), on disk
//! once fetched. Only the JP2's bytes come from a mirror, and only if they
//! match the bag's md5. The scan keeps LoC's URL and IIIF base, as an
//! archived copy does (ADR-0018), so nothing downstream sees a difference;
//! where the bytes came from is the caller's to record.
//!
//! **Pacing.** Each mirror host takes one request at a time, the next at
//! least its gap after the last one ended, never through the host's LoC
//! budget. A page waits at most [`MirrorConfig::max_wait`] for its turn and
//! then goes on without the mirror. A 429 or 503 holds the mirror for
//! `Retry-After` (at least [`MirrorConfig::backoff`], at most an hour), a
//! 403, a challenge or an HTML page for [`MirrorConfig::block_hold`], and
//! a held mirror is skipped, never waited out. Nothing a mirror answers
//! holds or slows LoC. A redirect is followed only within the mirror's
//! host, by hand, as a request of its own (a mirror sending us to loc.gov
//! would be a LoC request outside its pacing). Wayback's Save Page Now is
//! never used: it would make the Internet Archive fetch from LoC for us.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::Instant;
use usnm_core::ids::PageKey;

use super::{
    challenge, html_page, issue_id, read_limited, retry_after, BatchPage, Scan, ScanFormat,
    MAX_IMAGE_BYTES, USER_AGENT,
};
use crate::batch;

/// The largest page JSON or CDX answer read.
const MAX_META_BYTES: usize = 4 * 1024 * 1024;
/// The most rows one CDX answer lists (an issue has tens of files, each
/// captured a few times).
const CDX_LIMIT: usize = 2000;
/// The most captures of one file tried.
const CAPTURES_TRIED: usize = 2;
/// The most redirects within a mirror's host followed for one request.
const MAX_REDIRECTS: usize = 2;
/// How long a page JSON or a CDX lookup may take.
const META_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a JP2 may take.
const IMAGE_TIMEOUT: Duration = Duration::from_secs(120);

/// A state partner: its awardee code, its name in counts and logs, its
/// site, and whether its page JSON names the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partner {
    pub awardee: &'static str,
    pub name: &'static str,
    pub root: String,
    pub json_names_file: bool,
}

/// The partners known to serve LoC's NDNP JP2s byte for byte, checked on
/// 24 October 2026. South Carolina's page JSON doesn't name the file.
pub fn partners() -> Vec<Partner> {
    [
        (
            "nbu",
            "nebnewspapers",
            "https://nebnewspapers.unl.edu",
            true,
        ),
        (
            "pst",
            "panewsarchive",
            "https://panewsarchive.psu.edu",
            true,
        ),
        ("oru", "oregonnews", "https://oregonnews.uoregon.edu", true),
        (
            "scu",
            "historicnewspapers-sc",
            "https://historicnewspapers.sc.edu",
            false,
        ),
    ]
    .into_iter()
    .map(|(awardee, name, root, json_names_file)| Partner {
        awardee,
        name,
        root: root.into(),
        json_names_file,
    })
    .collect()
}

/// The Wayback Machine's name in counts and logs.
pub const WAYBACK: &str = "wayback";
pub const WAYBACK_ROOT: &str = "https://web.archive.org";

#[derive(Debug, Clone)]
pub struct MirrorConfig {
    /// The partners asked; empty for none.
    pub partners: Vec<Partner>,
    /// Whether the Wayback Machine is asked; its site.
    pub wayback: Option<String>,
    /// From the end of one request to a partner to the start of the next
    /// (South Carolina's `Crawl-delay` is 3 s; the others publish none).
    pub partner_gap: Duration,
    /// The same for the Wayback Machine.
    pub wayback_gap: Duration,
    /// A 5xx, a timeout or a network error holds the mirror this long, and
    /// a 429 or 503 at least this long.
    pub backoff: Duration,
    /// A 403, a challenge or an HTML page holds the mirror this long.
    pub block_hold: Duration,
    /// The longest a page waits for a mirror's turn before going on
    /// without it.
    pub max_wait: Duration,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            partners: Vec::new(),
            wayback: None,
            partner_gap: Duration::from_secs(5),
            wayback_gap: Duration::from_secs(5),
            backoff: Duration::from_secs(60),
            block_hold: Duration::from_secs(3600),
            max_wait: Duration::from_secs(30),
        }
    }
}

impl MirrorConfig {
    /// The config for a list of names: awardee codes (`nbu`, `pst`, `oru`,
    /// `scu`) and `wayback`.
    pub fn named(names: &[String]) -> anyhow::Result<Self> {
        let known = partners();
        let mut cfg = Self::default();
        for n in names {
            let n = n.trim();
            if n.is_empty() {
                continue;
            }
            if n == WAYBACK {
                cfg.wayback = Some(WAYBACK_ROOT.into());
            } else if let Some(p) = known.iter().find(|p| p.awardee == n) {
                if !cfg.partners.iter().any(|q| q.awardee == p.awardee) {
                    cfg.partners.push(p.clone());
                }
            } else {
                anyhow::bail!(
                    "unknown mirror {n:?}: the mirrors are {} and {WAYBACK}",
                    known
                        .iter()
                        .map(|p| p.awardee)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Ok(cfg)
    }

    pub fn is_empty(&self) -> bool {
        self.partners.is_empty() && self.wayback.is_none()
    }
}

/// Where a mirrored JP2 came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The mirror's name ([`Partner::name`] or [`WAYBACK`]).
    pub source: &'static str,
    /// The URL the bytes were read from.
    pub url: String,
}

/// One mirror's counts, for the fetcher's status.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MirrorStatus {
    /// Requests sent (redirects followed each count).
    pub requests: u64,
    /// JP2s that matched the bag's md5.
    pub hits: u64,
    /// Pages it doesn't have (404, not captured, refused).
    pub misses: u64,
    /// Pages whose file isn't LoC's for that seq (the page JSON named
    /// another frame).
    pub seq_drift: u64,
    /// JP2s that didn't match the bag's md5.
    pub md5_mismatches: u64,
    /// Pages it was skipped for: held, or busy past the wait.
    pub skipped_held: u64,
    /// Holds it set (429, 503, 403, a challenge, an error).
    pub holds: u64,
    /// Seconds it is still held for, if it is.
    pub held_secs: Option<u64>,
}

#[derive(Debug, Default)]
struct Counts {
    requests: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    seq_drift: AtomicU64,
    md5_mismatches: AtomicU64,
    skipped_held: AtomicU64,
    holds: AtomicU64,
}

/// One request at a time to a mirror host, each at least `gap` after the
/// last one ended, and the host's hold. LoC's bucket ([`super::bucket`])
/// has one too.
#[derive(Debug)]
pub(super) struct Gate {
    gap: Duration,
    one: Semaphore,
    /// No request before this: the gap after the last one, or a hold.
    next: Mutex<Instant>,
    held: Mutex<Option<Instant>>,
}

impl Gate {
    pub(super) fn new(gap: Duration) -> Self {
        Self {
            gap,
            one: Semaphore::new(1),
            next: Mutex::new(Instant::now()),
            held: Mutex::new(None),
        }
    }

    pub(super) fn held_until(&self) -> Option<Instant> {
        let held = *self.held.lock().expect("gate lock");
        held.filter(|h| *h > Instant::now())
    }

    pub(super) fn hold(&self, d: Duration) {
        let until = Instant::now() + d;
        let mut held = self.held.lock().expect("gate lock");
        *held = Some(held.map_or(until, |h| h.max(until)));
        let mut next = self.next.lock().expect("gate lock");
        *next = (*next).max(until);
    }

    /// The caller's turn, if it comes by `deadline` and the host isn't
    /// held: the host is then the caller's alone until the permit drops,
    /// and [`Gate::done`] must be called when its request ends. Nothing is
    /// reserved for a caller who doesn't get its turn.
    pub(super) async fn turn(&self, deadline: Instant) -> Option<SemaphorePermit<'_>> {
        if self.held_until().is_some() {
            return None;
        }
        let permit = tokio::time::timeout_at(deadline, self.one.acquire())
            .await
            .ok()?
            .ok()?;
        let at = *self.next.lock().expect("gate lock");
        if self.held_until().is_some() || at > deadline {
            return None;
        }
        tokio::time::sleep_until(at).await;
        // A hold set while this caller slept (by a request that just
        // ended) still holds.
        if self.held_until().is_some() {
            return None;
        }
        Some(permit)
    }

    /// A request ended: the next waits the gap.
    pub(super) fn done(&self) {
        let mut next = self.next.lock().expect("gate lock");
        *next = (*next).max(Instant::now() + self.gap);
    }
}

#[derive(Debug)]
struct Host {
    name: &'static str,
    gate: Gate,
    counts: Counts,
}

/// What one request got.
enum Tried {
    /// The body, how long it took, and the URL that sent it (after any
    /// redirect).
    Body(Bytes, Duration, String),
    /// 404 or 410: it isn't there.
    Missing,
    /// Not asked: the mirror is held, or busy past the wait.
    Skipped,
    /// Anything else: a hold, a refusal or an error, already counted.
    Failed,
}

/// What a JP2 from a mirror came to.
enum Checked {
    Hit(Scan, Hit),
    Mismatch,
    /// Missing, skipped or failed (already counted).
    No,
}

/// A captured file: when, its URL as captured, and the record's length.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Capture {
    ts: String,
    original: String,
    length: u64,
}

/// An issue's captured JP2s, by file name, each file's distinct captures
/// largest first.
type Captures = BTreeMap<String, Vec<Capture>>;

/// The mirrors a fetcher asks, each paced and held on its own.
pub struct Mirrors {
    cfg: MirrorConfig,
    http: reqwest::Client,
    /// By awardee code.
    partners: BTreeMap<&'static str, (Partner, Host)>,
    wayback: Option<(String, Host)>,
    /// By issue directory URL. Lookups that found nothing are kept too;
    /// ones that couldn't be made aren't.
    cdx: moka::future::Cache<String, Arc<Captures>>,
}

impl std::fmt::Debug for Mirrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mirrors")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl Mirrors {
    pub fn new(cfg: MirrorConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            // No idle connections kept: requests are seconds apart, and a
            // connection the server closed meanwhile fails the next request
            // ("error sending request"), which would hold the host.
            .pool_max_idle_per_host(0)
            // Followed by hand, within the host, each paced and counted.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let host = |name, gap| Host {
            name,
            gate: Gate::new(gap),
            counts: Counts::default(),
        };
        let partners = cfg
            .partners
            .iter()
            .map(|p| (p.awardee, (p.clone(), host(p.name, cfg.partner_gap))))
            .collect();
        let wayback = cfg
            .wayback
            .clone()
            .map(|root| (root, host(WAYBACK, cfg.wayback_gap)));
        Ok(Self {
            http,
            partners,
            wayback,
            cdx: moka::future::Cache::builder()
                .max_capacity(4096)
                .time_to_live(Duration::from_secs(6 * 3600))
                .build(),
            cfg,
        })
    }

    /// The page's JP2 from a mirror, matching the bag's md5, with LoC's
    /// URL and IIIF base; `None` when no mirror has it (or none could be
    /// asked), for the caller to fetch it from LoC.
    pub async fn jp2(&self, batch: &str, key: &PageKey, page: &BatchPage) -> Option<(Scan, Hit)> {
        let awardee = batch.split('_').next().unwrap_or_default();
        if let Some((p, host)) = self.partners.get(awardee) {
            if let Some(got) = self.ask_partner(p, host, key, page).await {
                return Some(got);
            }
        }
        if let Some((root, host)) = &self.wayback {
            if let Some(got) = self.ask_wayback(root, host, page).await {
                return Some(got);
            }
        }
        None
    }

    /// Each mirror's counts, by name.
    pub fn status(&self) -> BTreeMap<String, MirrorStatus> {
        let now = Instant::now();
        self.partners
            .values()
            .map(|(_, h)| h)
            .chain(self.wayback.iter().map(|(_, h)| h))
            .map(|h| {
                let c = &h.counts;
                let get = |a: &AtomicU64| a.load(Ordering::SeqCst);
                (
                    h.name.to_owned(),
                    MirrorStatus {
                        requests: get(&c.requests),
                        hits: get(&c.hits),
                        misses: get(&c.misses),
                        seq_drift: get(&c.seq_drift),
                        md5_mismatches: get(&c.md5_mismatches),
                        skipped_held: get(&c.skipped_held),
                        holds: get(&c.holds),
                        held_secs: h
                            .gate
                            .held_until()
                            .map(|t| t.saturating_duration_since(now).as_secs()),
                    },
                )
            })
            .collect()
    }

    async fn ask_partner(
        &self,
        p: &Partner,
        host: &Host,
        key: &PageKey,
        page: &BatchPage,
    ) -> Option<(Scan, Hit)> {
        let base = format!(
            "{}/lccn/{}/seq-{}",
            p.root.trim_end_matches('/'),
            issue_id(key),
            key.seq
        );
        if p.json_names_file {
            match self
                .get(host, &format!("{base}.json"), MAX_META_BYTES, META_TIMEOUT)
                .await
            {
                Tried::Body(body, _, _) => match partner_file(&body) {
                    Some(file) if Some(file.as_str()) != data_path(&page.url) => {
                        host.counts.seq_drift.fetch_add(1, Ordering::SeqCst);
                        tracing::debug!(
                            mirror = host.name,
                            page = page.url,
                            theirs = file,
                            "the mirror's page is another file"
                        );
                        return None;
                    }
                    // LoC's file, or a page JSON that doesn't say: the md5
                    // decides.
                    _ => {}
                },
                Tried::Missing => {
                    host.counts.misses.fetch_add(1, Ordering::SeqCst);
                    return None;
                }
                Tried::Skipped | Tried::Failed => return None,
            }
        }
        match self.checked(host, &format!("{base}.jp2"), page).await {
            Checked::Hit(scan, hit) => Some((scan, hit)),
            Checked::Mismatch | Checked::No => None,
        }
    }

    async fn ask_wayback(&self, root: &str, host: &Host, page: &BatchPage) -> Option<(Scan, Hit)> {
        let (dir, file) = page.url.rsplit_once('/')?;
        let dir = format!("{dir}/");
        // Pages of the issue asked at once share one lookup; one that
        // couldn't be made isn't kept.
        let captures = self
            .cdx
            .optionally_get_with(dir.clone(), async {
                self.captures(root, host, &dir).await.map(Arc::new)
            })
            .await?;
        let Some(tries) = captures.get(file) else {
            host.counts.misses.fetch_add(1, Ordering::SeqCst);
            return None;
        };
        for c in tries.iter().take(CAPTURES_TRIED) {
            let url = format!(
                "{}/web/{}id_/{}",
                root.trim_end_matches('/'),
                c.ts,
                c.original
            );
            match self.checked(host, &url, page).await {
                Checked::Hit(scan, hit) => return Some((scan, hit)),
                // A capture cut short: the next.
                Checked::Mismatch => continue,
                Checked::No => return None,
            }
        }
        None
    }

    /// The JP2s the Wayback Machine has captured in an issue's directory;
    /// `None` if it couldn't be asked (held, busy or an error) or its
    /// answer couldn't be read, so the lookup isn't kept.
    async fn captures(&self, root: &str, host: &Host, dir: &str) -> Option<Captures> {
        let target = dir
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        let url = reqwest::Url::parse_with_params(
            &format!("{}/cdx/search/cdx", root.trim_end_matches('/')),
            &[
                ("url", target),
                ("matchType", "prefix"),
                ("filter", "statuscode:200"),
                ("filter", "mimetype:image/jp2"),
                // Every distinct capture, not only each file's first: an
                // early capture may be cut short.
                ("collapse", "digest"),
                ("fl", "timestamp,original,length"),
                ("output", "json"),
                ("limit", &CDX_LIMIT.to_string()),
            ],
        )
        .ok()?;
        let body = match self
            .get(host, url.as_str(), MAX_META_BYTES, META_TIMEOUT)
            .await
        {
            Tried::Body(b, _, _) => b,
            // The CDX answers 404 for nothing captured on some prefixes.
            Tried::Missing => return Some(BTreeMap::new()),
            Tried::Skipped | Tried::Failed => return None,
        };
        let path = dir_path(dir)?;
        let got = cdx_files(&body, &path);
        if got.is_none() {
            tracing::warn!(mirror = host.name, dir, "a CDX answer that doesn't read");
            host.gate.hold(self.cfg.backoff);
            host.counts.holds.fetch_add(1, Ordering::SeqCst);
        }
        got
    }

    /// `url`'s body, if it matches the bag's md5, as the page's scan.
    async fn checked(&self, host: &Host, url: &str, page: &BatchPage) -> Checked {
        let (body, took, from) = match self.get(host, url, MAX_IMAGE_BYTES, IMAGE_TIMEOUT).await {
            Tried::Body(b, t, from) => (b, t, from),
            Tried::Missing => {
                host.counts.misses.fetch_add(1, Ordering::SeqCst);
                return Checked::No;
            }
            Tried::Skipped | Tried::Failed => return Checked::No,
        };
        let md5 = batch::md5(&body);
        if md5 != page.md5 {
            host.counts.md5_mismatches.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                mirror = host.name,
                url,
                bytes = body.len(),
                md5 = batch::hex(&md5),
                bag = batch::hex(&page.md5),
                "a mirror's JP2 isn't the bag's"
            );
            return Checked::Mismatch;
        }
        host.counts.hits.fetch_add(1, Ordering::SeqCst);
        Checked::Hit(
            Scan {
                iiif_base: page.iiif_base.clone(),
                url: page.url.clone(),
                format: ScanFormat::Jp2,
                bytes: body,
                md5: Some(batch::hex(&md5)),
                download: took,
                from_cache: false,
                evict: None,
            },
            Hit {
                source: host.name,
                url: from,
            },
        )
    }

    /// One GET from a mirror in its turn, with no retry: a held or busy
    /// mirror isn't asked. Redirects within its host are followed, each in
    /// its own turn after the gap.
    async fn get(&self, host: &Host, url: &str, max: usize, timeout: Duration) -> Tried {
        let c = &host.counts;
        let Some(_turn) = host.gate.turn(Instant::now() + self.cfg.max_wait).await else {
            c.skipped_held.fetch_add(1, Ordering::SeqCst);
            return Tried::Skipped;
        };
        let tried = self.send(host, url, max, timeout).await;
        host.gate.done();
        tried
    }

    async fn send(&self, host: &Host, first: &str, max: usize, timeout: Duration) -> Tried {
        let c = &host.counts;
        let hold = |d: Duration, url: &str, why: String| {
            host.gate.hold(d);
            c.holds.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                mirror = host.name,
                url,
                why,
                secs = d.as_secs(),
                "holding a mirror"
            );
            Tried::Failed
        };
        let mut url = first.to_owned();
        for hop in 0..=MAX_REDIRECTS {
            if hop > 0 {
                // Its turn is still ours; the gap holds between hops too.
                tokio::time::sleep(host.gate.gap).await;
            }
            c.requests.fetch_add(1, Ordering::SeqCst);
            let sent = Instant::now();
            let resp = match self.http.get(&url).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    return hold(self.cfg.backoff, &url, super::error_chain(&e.without_url()))
                }
            };
            let status = resp.status();
            if status.is_success() {
                if let Some(why) = html_page(&resp) {
                    return hold(self.cfg.block_hold, &url, why);
                }
                return match read_limited(resp, max).await {
                    Ok(body) => Tried::Body(body, sent.elapsed(), url),
                    Err(e) => hold(self.cfg.backoff, &url, e),
                };
            }
            // A challenge is a block whatever its status.
            if let Some(why) = challenge(&resp) {
                return hold(self.cfg.block_hold, &url, format!("HTTP {status}: {why}"));
            }
            match status.as_u16() {
                404 | 410 => return Tried::Missing,
                429 | 503 => {
                    let d = retry_after(&resp)
                        .map_or(self.cfg.backoff, |(d, _)| d.max(self.cfg.backoff));
                    return hold(d, &url, format!("HTTP {status}"));
                }
                403 => return hold(self.cfg.block_hold, &url, format!("HTTP {status}")),
                301 | 302 | 303 | 307 | 308 => match same_host_location(&url, &resp) {
                    Some(to) if hop < MAX_REDIRECTS => url = to,
                    _ => {
                        tracing::debug!(mirror = host.name, url, %status, "a redirect not followed");
                        c.misses.fetch_add(1, Ordering::SeqCst);
                        return Tried::Failed;
                    }
                },
                300..=499 => {
                    tracing::debug!(mirror = host.name, url, %status, "a mirror refused");
                    c.misses.fetch_add(1, Ordering::SeqCst);
                    return Tried::Failed;
                }
                _ => return hold(self.cfg.backoff, &url, format!("HTTP {status}")),
            }
        }
        Tried::Failed
    }
}

/// A redirect's target, if it stays on `from`'s scheme, host and port.
fn same_host_location(from: &str, resp: &reqwest::Response) -> Option<String> {
    let from = reqwest::Url::parse(from).ok()?;
    let loc = resp
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?;
    let to = from.join(loc).ok()?;
    (to.scheme() == from.scheme()
        && to.host_str() == from.host_str()
        && to.port_or_known_default() == from.port_or_known_default())
    .then(|| to.to_string())
}

/// The part of a storage-service URL from the batch's `data/` on:
/// `data/<lccn>/<reel>/<issue>/<frame>.jp2`.
fn data_path(url: &str) -> Option<&str> {
    url.find("/data/").map(|i| &url[i + 1..])
}

/// The file a partner's page JSON (a IIIF canvas) names, from the
/// `batch/data/...` path in its image id: `data/<lccn>/<reel>/<issue>/<frame>.jp2`.
fn partner_file(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let id = v
        .pointer("/images/0/resource/service/@id")
        .or_else(|| v.pointer("/images/0/resource/@id"))
        .or_else(|| v.get("@id"))?
        .as_str()?;
    let decoded = percent_decode(id);
    let i = decoded.find("/data/")?;
    let file = &decoded[i + 1..];
    file.ends_with(".jp2").then(|| file.to_owned())
}

/// `%XX` escapes decoded (the IIIF ids escape `/`).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A URL's path, for comparing captures whatever their scheme and port.
fn dir_path(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok().map(|u| u.path().to_owned())
}

/// The JP2s directly in `dir` (a path ending in `/`) in a CDX JSON answer
/// (`[["timestamp","original","length"], [ts, url, length], ...]`), by
/// file name, each file's captures largest first, then latest; `None` if
/// the answer isn't a JSON array (an empty one is an answer: nothing
/// captured).
fn cdx_files(body: &[u8], dir: &str) -> Option<Captures> {
    let body = std::str::from_utf8(body).ok()?.trim();
    // The CDX answers an empty body for nothing captured.
    if body.is_empty() {
        return Some(BTreeMap::new());
    }
    let Value::Array(rows) = serde_json::from_str::<Value>(body).ok()? else {
        return None;
    };
    let mut out: Captures = BTreeMap::new();
    for row in rows.iter().skip(1) {
        let field = |i: usize| row.get(i).and_then(Value::as_str);
        let (Some(ts), Some(orig)) = (field(0), field(1)) else {
            continue;
        };
        let length = field(2).and_then(|l| l.parse().ok()).unwrap_or(0);
        let Some(path) = dir_path(orig) else { continue };
        let Some(file) = path.strip_prefix(dir) else {
            continue;
        };
        if file.contains('/')
            || !file.ends_with(".jp2")
            || ts.is_empty()
            || !ts.bytes().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        out.entry(file.to_owned()).or_default().push(Capture {
            ts: ts.to_owned(),
            original: orig.to_owned(),
            length,
        });
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| b.length.cmp(&a.length).then_with(|| b.ts.cmp(&a.ts)));
    }
    Some(out)
}

#[cfg(test)]
mod tests;
