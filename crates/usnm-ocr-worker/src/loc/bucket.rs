//! LoC's own S3 bucket of NDNP batches, `ndnp-batches`
//! (`https://ndnp-batches.s3.amazonaws.com/`, us-east-1, anonymous GET,
//! not behind Cloudflare). The maintainer's decision of 24 October 2026:
//! the scan fetcher may use it for everything on the batches it holds, the
//! bag manifest, the issue METS and the JP2s, at a slow pace, so those
//! batches need nothing from LoC's Cloudflare-fronted hosts. Off unless
//! configured ([`super::LocConfig::bucket`]).
//!
//! **Keys.** The bucket's keys are LoC's storage paths without the
//! awardee and the `batch_` prefix: LoC's
//! `…/storage-services/service/ndnp/<aw>/batch_<b>_verNN/<path>` is
//! `<root><b>_verNN/<path>`. On 24 October 1,074 of the 3,010 batches were
//! public (38.1% of the pages); every object of the others answers 403
//! AccessDenied.
//!
//! **What is taken.** A bag manifest is taken only if its body matches the
//! object's `x-amz-meta-md5` (the uploader's md5) or, without that header,
//! its ETag when that is a single-part one (32 hex digits, which S3 makes
//! the md5); one with neither is refused and counted. A METS or JP2 must
//! match the bag's md5, as LoC's must. A scan keeps LoC's URL and IIIF
//! base. The bucket's keys don't name the awardee, so the URL a bucket
//! manifest is kept under on LoC is only the guess the batch's name gives
//! (unless a URL LoC served is already on disk): the caller marks it as a
//! guess and checks it with LoC when LoC answers 404 under it
//! ([`super::LocClient`]). Where a JP2's bytes came from is the caller's to
//! record ([`NAME`] and the bucket's URL).
//!
//! **Which batches.** The first answer for a batch says whether the bucket
//! serves it: a file it sends makes the batch public; a 403, or a 404 for
//! its manifest, while nothing is known of the batch makes it private, and
//! nothing more of that batch is asked of the bucket in this process. Once
//! a batch is public, a 403 or 404 for one of its files is that file only
//! (a restrictive ACL on one object, or S3's 403 for a missing key where
//! listing isn't allowed): it counts as a miss and the batch stays public.
//!
//! **Pacing.** As for a mirror ([`super::mirror`]): one request at a time,
//! the next at least [`BucketConfig::gap`] (6 s, never less from the flag:
//! [`MIN_GAP`]) after the last one ended, never through LoC's pacers or the
//! host's LoC budget. A request waits at most [`BucketConfig::max_wait`]
//! for its turn and then goes to LoC. A 429 or 503 (S3's SlowDown) holds
//! the bucket for `Retry-After` (at least [`BucketConfig::backoff`], at
//! most an hour); a challenge, an HTML page or a redirect for
//! [`BucketConfig::block_hold`]; and a 5xx, a timeout, a network error or
//! a body that can't be read for the backoff. A held bucket is skipped,
//! never waited out, and nothing the bucket answers holds or slows LoC. A
//! 403 holds nothing: it means the batch is private. No redirect is
//! followed (S3 sends one only towards another endpoint, which is another
//! host, so a redirect means the root is wrong). The flag takes only an
//! `amazonaws.com` host, so it can't point the bucket's client at a LoC
//! host outside LoC's pacers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context as _;
use base64::Engine as _;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use super::mirror::{Gate, Hit};
use super::{
    challenge, html_page, read_limited, retry_after, BatchPage, Scan, ScanFormat, MAX_BAG_BYTES,
    MAX_IMAGE_BYTES, MAX_METS_BYTES, USER_AGENT,
};
use crate::batch::{self, Md5Sum};

/// The bucket's name in counts, logs and `sources-<tag>.jsonl`.
pub const NAME: &str = "ndnp-batches";
/// The bucket's root.
pub const ROOT: &str = "https://ndnp-batches.s3.amazonaws.com/";

/// The longest an issue METS may take (a manifest or a JP2 may take
/// [`BucketConfig::timeout`]).
const METS_TIMEOUT: Duration = Duration::from_secs(30);
/// The uploader's md5 of an object.
const META_MD5: &str = "x-amz-meta-md5";
/// The shortest gap between requests to the bucket `--loc-bucket-gap-ms`
/// may set: 6 s, the maintainer's of 24 October 2026.
pub const MIN_GAP: Duration = Duration::from_secs(6);

#[derive(Debug, Clone)]
pub struct BucketConfig {
    /// The bucket's root, ending in `/` ([`ROOT`]; a stand-in in tests).
    pub root: String,
    /// From the end of one request to the start of the next (6 s, the
    /// maintainer's; [`BucketConfig::at`] takes nothing under [`MIN_GAP`]).
    pub gap: Duration,
    /// A 5xx, a timeout, a network error or a body that can't be read
    /// holds the bucket this long, and a 429 or 503 at least this long.
    pub backoff: Duration,
    /// A challenge, an HTML page or a redirect holds the bucket this long.
    pub block_hold: Duration,
    /// The longest a request waits for the bucket's turn before going to
    /// LoC without it.
    pub max_wait: Duration,
    /// The longest one request may take (120 s: a 10,000-page batch's
    /// manifest is about 3 MB); an issue METS at most 30 s.
    pub timeout: Duration,
}

impl Default for BucketConfig {
    fn default() -> Self {
        Self {
            root: ROOT.into(),
            gap: MIN_GAP,
            backoff: Duration::from_secs(60),
            block_hold: Duration::from_secs(3600),
            max_wait: Duration::from_secs(30),
            timeout: Duration::from_secs(120),
        }
    }
}

impl BucketConfig {
    /// The bucket at `root` (`--loc-bucket`), `gap` apart
    /// (`--loc-bucket-gap-ms`, at least [`MIN_GAP`]), with the default
    /// holds; `None` for an empty `root`: off. Only an `amazonaws.com` host
    /// is taken: a loc.gov one would be asked outside LoC's pacers and the
    /// host's LoC budget.
    pub fn at(root: &str, gap: Duration) -> anyhow::Result<Option<Self>> {
        let root = root.trim();
        if root.is_empty() {
            return Ok(None);
        }
        let u = reqwest::Url::parse(root).with_context(|| format!("--loc-bucket {root:?}"))?;
        anyhow::ensure!(
            matches!(u.scheme(), "https" | "http")
                && u.host_str().is_some()
                && u.query().is_none()
                && u.fragment().is_none()
                && u.username().is_empty()
                && u.password().is_none(),
            "--loc-bucket {root:?} isn't a plain http(s) URL"
        );
        let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
        anyhow::ensure!(
            host.ends_with(".amazonaws.com"),
            "--loc-bucket {root:?}: LoC's bucket is on S3, an amazonaws.com host; {host} isn't"
        );
        anyhow::ensure!(
            gap >= MIN_GAP,
            "--loc-bucket-gap-ms must be at least {}",
            MIN_GAP.as_millis()
        );
        let mut root = u.to_string();
        if !root.ends_with('/') {
            root.push('/');
        }
        Ok(Some(Self {
            root,
            gap,
            ..Self::default()
        }))
    }
}

/// The bucket's counts, for the fetcher's status.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BucketStatus {
    /// Requests sent.
    pub requests: u64,
    /// Bag manifests, issue METS and JP2s taken from it.
    pub manifests: u64,
    pub mets: u64,
    pub jp2s: u64,
    /// Batches it serves, and batches it doesn't (a 403, or no manifest),
    /// as learnt in this process.
    pub public_batches: u64,
    pub private_batches: u64,
    /// Files of a public batch it doesn't have or refuses (404, 403), and
    /// other refusals.
    pub misses: u64,
    /// Files that didn't match their md5 (the bag's, or a manifest's
    /// `x-amz-meta-md5` or ETag).
    pub md5_mismatches: u64,
    /// Manifests refused for want of an md5 to check them against: no
    /// `x-amz-meta-md5`, and no ETag or a multipart one.
    pub no_md5: u64,
    /// Requests it was skipped for: held, or busy past the wait.
    pub skipped_held: u64,
    /// Holds it set (429, 503, a challenge, a redirect, an error).
    pub holds: u64,
    /// Seconds it is still held for, if it is.
    pub held_secs: Option<u64>,
}

#[derive(Debug, Default)]
struct Counts {
    requests: AtomicU64,
    manifests: AtomicU64,
    mets: AtomicU64,
    jp2s: AtomicU64,
    misses: AtomicU64,
    md5_mismatches: AtomicU64,
    no_md5: AtomicU64,
    skipped_held: AtomicU64,
    holds: AtomicU64,
}

/// What an object's headers say its md5 is.
#[derive(Debug, Clone, PartialEq)]
enum Declared {
    /// `x-amz-meta-md5`, the uploader's, as sent.
    Meta(String),
    /// A single-part ETag (no `x-amz-meta-md5`), which S3 makes the md5.
    ETag(Md5Sum),
    /// Neither: no `x-amz-meta-md5`, and no ETag or a multipart one.
    Unknown,
}

impl Declared {
    fn of(resp: &reqwest::Response) -> Self {
        let h = resp.headers();
        if let Some(v) = h.get(META_MD5) {
            return Self::Meta(v.to_str().unwrap_or_default().to_owned());
        }
        h.get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .and_then(etag_md5)
            .map_or(Self::Unknown, Self::ETag)
    }
}

/// What one request got.
enum Tried {
    /// The body, how long it took, and what the headers say its md5 is.
    Body(Bytes, Duration, Declared),
    /// 403: the batch isn't public.
    Private,
    /// 404 or 410: it isn't there.
    Missing,
    /// Not asked: the bucket is held or busy past the wait, or the batch
    /// is known not to be there.
    Skipped,
    /// Anything else: a hold, a refusal or an error, already counted.
    Failed,
}

/// LoC's bucket, paced and held on its own; one per [`super::LocClient`].
#[derive(Debug)]
pub(super) struct Bucket {
    cfg: BucketConfig,
    http: reqwest::Client,
    /// The bucket's one gate, for manifests, METS and JP2s alike.
    gate: Gate,
    counts: Counts,
    /// By batch directory: whether the bucket serves it.
    batches: Mutex<HashMap<String, bool>>,
}

impl Bucket {
    pub(super) fn new(cfg: BucketConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            gate: Gate::new(cfg.gap),
            counts: Counts::default(),
            batches: Mutex::new(HashMap::new()),
            cfg,
        })
    }

    /// Whether the bucket serves batch `dir`, if that is known.
    fn serves(&self, dir: &str) -> Option<bool> {
        self.batches.lock().expect("bucket lock").get(dir).copied()
    }

    fn learn(&self, dir: &str, public: bool) {
        let before = self
            .batches
            .lock()
            .expect("bucket lock")
            .insert(dir.to_owned(), public);
        if before != Some(public) {
            tracing::info!(
                bucket = NAME,
                batch = dir,
                public,
                "whether LoC's bucket serves this batch"
            );
        }
    }

    /// A 403 or 404 for a file of batch `dir`: of a batch it has served,
    /// that file only (a miss); otherwise the batch isn't public.
    fn refused(&self, dir: &str, url: &str, why: &str) {
        if self.serves(dir) == Some(true) {
            self.counts.misses.fetch_add(1, Ordering::SeqCst);
            tracing::debug!(
                bucket = NAME,
                url,
                why,
                "LoC's bucket refused one file of a batch it serves"
            );
        } else {
            self.learn(dir, false);
        }
    }

    /// Batch `dir`'s bag manifest, if the bucket serves it and the body
    /// matches the object's `x-amz-meta-md5` or, without one, its
    /// single-part ETag; `None` sends the caller to LoC.
    pub(super) async fn manifest(&self, dir: &str) -> Option<Bytes> {
        let url = format!("{}{dir}/manifest-md5.txt", self.cfg.root);
        match self
            .get(dir, &url, MAX_BAG_BYTES, self.cfg.timeout, true)
            .await
        {
            Tried::Body(body, _, declared) => {
                let got = batch::md5(&body);
                let want = match &declared {
                    Declared::Meta(v) => meta_md5(v),
                    Declared::ETag(m) => Some(*m),
                    Declared::Unknown => {
                        self.counts.no_md5.fetch_add(1, Ordering::SeqCst);
                        tracing::warn!(
                            bucket = NAME,
                            url,
                            bytes = body.len(),
                            "a manifest from LoC's bucket with neither x-amz-meta-md5 nor a single-part ETag; refused"
                        );
                        return None;
                    }
                };
                if want != Some(got) {
                    self.counts.md5_mismatches.fetch_add(1, Ordering::SeqCst);
                    tracing::warn!(
                        bucket = NAME,
                        url,
                        bytes = body.len(),
                        md5 = batch::hex(&got),
                        declared = ?declared,
                        "a manifest from LoC's bucket isn't the md5 its headers give; refused"
                    );
                    return None;
                }
                self.learn(dir, true);
                self.counts.manifests.fetch_add(1, Ordering::SeqCst);
                Some(body)
            }
            // No manifest: the bucket doesn't serve the batch (learnt in
            // its turn).
            Tried::Private | Tried::Missing | Tried::Skipped | Tried::Failed => None,
        }
    }

    /// An issue METS of batch `dir` (`rel`, its path in the bag), if the
    /// bucket serves it and it matches the bag's `md5`.
    pub(super) async fn mets(&self, dir: &str, rel: &str, md5: &Md5Sum) -> Option<Bytes> {
        let (body, _, _) = self
            .checked(
                dir,
                rel,
                md5,
                MAX_METS_BYTES,
                METS_TIMEOUT.min(self.cfg.timeout),
            )
            .await?;
        self.counts.mets.fetch_add(1, Ordering::SeqCst);
        Some(body)
    }

    /// The page's JP2, if the bucket serves its batch and the bytes match
    /// the bag's md5, with LoC's URL and IIIF base. `storage_root` is LoC's,
    /// under which `page.url` is.
    pub(super) async fn jp2(&self, page: &BatchPage, storage_root: &str) -> Option<(Scan, Hit)> {
        let (dir, rel) = key(storage_root, &page.url)?;
        let (body, took, url) = self
            .checked(dir, rel, &page.md5, MAX_IMAGE_BYTES, self.cfg.timeout)
            .await?;
        self.counts.jp2s.fetch_add(1, Ordering::SeqCst);
        Some((
            Scan {
                iiif_base: page.iiif_base.clone(),
                url: page.url.clone(),
                format: ScanFormat::Jp2,
                md5: Some(batch::hex(&page.md5)),
                bytes: body,
                download: took,
                from_cache: false,
                evict: None,
            },
            Hit { source: NAME, url },
        ))
    }

    /// The counts, and the hold if one is running.
    pub(super) fn status(&self) -> BucketStatus {
        let c = &self.counts;
        let get = |a: &AtomicU64| a.load(Ordering::SeqCst);
        let (public, private) = {
            let b = self.batches.lock().expect("bucket lock");
            let public = b.values().filter(|p| **p).count() as u64;
            (public, b.len() as u64 - public)
        };
        BucketStatus {
            requests: get(&c.requests),
            manifests: get(&c.manifests),
            mets: get(&c.mets),
            jp2s: get(&c.jp2s),
            public_batches: public,
            private_batches: private,
            misses: get(&c.misses),
            md5_mismatches: get(&c.md5_mismatches),
            no_md5: get(&c.no_md5),
            skipped_held: get(&c.skipped_held),
            holds: get(&c.holds),
            held_secs: self
                .gate
                .held_until()
                .map(|t| t.saturating_duration_since(Instant::now()).as_secs()),
        }
    }

    /// `rel` in batch `dir`, if it matches `md5`: its body, how long it
    /// took and the URL it came from.
    async fn checked(
        &self,
        dir: &str,
        rel: &str,
        md5: &Md5Sum,
        max: usize,
        timeout: Duration,
    ) -> Option<(Bytes, Duration, String)> {
        let url = format!("{}{dir}/{rel}", self.cfg.root);
        match self.get(dir, &url, max, timeout, false).await {
            Tried::Body(body, took, _) => {
                let got = batch::md5(&body);
                if &got != md5 {
                    self.counts.md5_mismatches.fetch_add(1, Ordering::SeqCst);
                    tracing::warn!(
                        bucket = NAME,
                        url,
                        bytes = body.len(),
                        md5 = batch::hex(&got),
                        bag = batch::hex(md5),
                        "a file from LoC's bucket isn't the bag's"
                    );
                    return None;
                }
                self.learn(dir, true);
                Some((body, took, url))
            }
            // A METS or JP2 it doesn't have says nothing of the batch.
            Tried::Missing => {
                self.counts.misses.fetch_add(1, Ordering::SeqCst);
                None
            }
            Tried::Private | Tried::Skipped | Tried::Failed => None,
        }
    }

    /// One GET in the bucket's turn, with no retry. A held or busy bucket,
    /// or a batch it is known not to serve, isn't asked. A 403, or a 404
    /// for a `manifest`, is learnt ([`Self::refused`]) before the turn
    /// ends, so a request of that batch waiting for the turn sees it.
    async fn get(
        &self,
        dir: &str,
        url: &str,
        max: usize,
        timeout: Duration,
        manifest: bool,
    ) -> Tried {
        if self.serves(dir) == Some(false) {
            return Tried::Skipped;
        }
        let Some(_turn) = self.gate.turn(Instant::now() + self.cfg.max_wait).await else {
            self.counts.skipped_held.fetch_add(1, Ordering::SeqCst);
            return Tried::Skipped;
        };
        // Learnt while this one waited its turn.
        if self.serves(dir) == Some(false) {
            return Tried::Skipped;
        }
        let tried = self.send(url, max, timeout).await;
        match &tried {
            Tried::Private => self.refused(dir, url, "403"),
            Tried::Missing if manifest => self.refused(dir, url, "404"),
            _ => {}
        }
        self.gate.done();
        tried
    }

    async fn send(&self, url: &str, max: usize, timeout: Duration) -> Tried {
        let c = &self.counts;
        let hold = |d: Duration, why: String| {
            self.gate.hold(d);
            c.holds.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                bucket = NAME,
                url,
                why,
                secs = d.as_secs(),
                "holding LoC's bucket"
            );
            Tried::Failed
        };
        c.requests.fetch_add(1, Ordering::SeqCst);
        let sent = Instant::now();
        let resp = match self.http.get(url).timeout(timeout).send().await {
            Ok(r) => r,
            Err(e) => return hold(self.cfg.backoff, e.without_url().to_string()),
        };
        let status = resp.status();
        if status.is_success() {
            if let Some(why) = html_page(&resp) {
                return hold(self.cfg.block_hold, why);
            }
            let declared = Declared::of(&resp);
            return match read_limited(resp, max).await {
                Ok(body) => Tried::Body(body, sent.elapsed(), declared),
                Err(e) => hold(self.cfg.backoff, e),
            };
        }
        // A challenge is a block whatever its status.
        if let Some(why) = challenge(&resp) {
            return hold(self.cfg.block_hold, format!("HTTP {status}: {why}"));
        }
        match status.as_u16() {
            // AccessDenied: a private batch, not a block.
            403 => Tried::Private,
            404 | 410 => Tried::Missing,
            429 | 503 => {
                let d =
                    retry_after(&resp).map_or(self.cfg.backoff, |(d, _)| d.max(self.cfg.backoff));
                hold(d, format!("HTTP {status}"))
            }
            // S3 redirects only towards another endpoint (a 301
            // PermanentRedirect when the root names the wrong one): every
            // request would wait its turn for nothing.
            300..=399 => {
                let to = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .unwrap_or("no Location");
                hold(
                    self.cfg.block_hold,
                    format!("HTTP {status} to {to}, not followed: is --loc-bucket the bucket's endpoint?"),
                )
            }
            400..=499 => {
                tracing::debug!(bucket = NAME, url, %status, "LoC's bucket refused");
                c.misses.fetch_add(1, Ordering::SeqCst);
                Tried::Failed
            }
            _ => hold(self.cfg.backoff, format!("HTTP {status}")),
        }
    }
}

/// The bucket's batch directory and path for a file on LoC's storage
/// service: `<storage root><aw>/batch_<b>_verNN/<path>` is
/// `(<b>_verNN, <path>)`.
pub(super) fn key<'a>(storage_root: &str, url: &'a str) -> Option<(&'a str, &'a str)> {
    let (_awardee, rest) = url.strip_prefix(storage_root)?.split_once('/')?;
    let (dir, path) = rest.split_once('/')?;
    let dir = dir.strip_prefix("batch_")?;
    (batch::safe_segment(dir) && !path.is_empty()).then_some((dir, path))
}

/// An `x-amz-meta-md5`: hex, or base64 of the 16 bytes.
fn meta_md5(v: &str) -> Option<Md5Sum> {
    let v = v.trim();
    batch::parse_md5(v).or_else(|| {
        base64::engine::general_purpose::STANDARD
            .decode(v)
            .ok()?
            .try_into()
            .ok()
    })
}

/// The md5 a single-part ETag gives: S3 makes the ETag of an object put in
/// one part its md5 (32 hex digits, quoted); a multipart one ends in
/// `-<parts>` and isn't one. An ETag that is no md5 (S3 encrypting with
/// KMS) only fails the check.
fn etag_md5(v: &str) -> Option<Md5Sum> {
    let v = v.trim();
    let v = v
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(v);
    batch::parse_md5(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_keys_drop_the_awardee_and_the_batch_prefix() {
        let root = "https://tile.loc.gov/storage-services/service/ndnp/";
        assert_eq!(
            key(
                root,
                &format!("{root}nbu/batch_nbu_abbott_ver01/data/sn1/00237/1890050301/0001.jp2")
            ),
            Some(("nbu_abbott_ver01", "data/sn1/00237/1890050301/0001.jp2"))
        );
        // A batch under another awardee's directory has the same key.
        assert_eq!(
            key(
                root,
                &format!("{root}yy/batch_xx_test_ver01/manifest-md5.txt")
            ),
            Some(("xx_test_ver01", "manifest-md5.txt"))
        );
        assert_eq!(key(root, "https://elsewhere/nbu/batch_x/y"), None);
        assert_eq!(key(root, &format!("{root}nbu/nbu_abbott_ver01/x")), None);
        assert_eq!(key(root, &format!("{root}nbu/batch_../x")), None);
    }

    #[test]
    fn reads_an_x_amz_meta_md5_as_hex_or_base64() {
        let want = batch::md5(b"x");
        assert_eq!(meta_md5(&batch::md5_hex(b"x")), Some(want));
        assert_eq!(
            meta_md5(&batch::md5_hex(b"x").to_ascii_uppercase()),
            Some(want)
        );
        assert_eq!(
            meta_md5(&base64::engine::general_purpose::STANDARD.encode(want)),
            Some(want)
        );
        assert_eq!(meta_md5("nonsense"), None);
        assert_eq!(meta_md5(""), None);
    }

    #[test]
    fn only_a_single_part_etag_is_an_md5() {
        let hex = batch::md5_hex(b"x");
        assert_eq!(etag_md5(&format!("\"{hex}\"")), Some(batch::md5(b"x")));
        assert_eq!(etag_md5(&hex), Some(batch::md5(b"x")));
        // Multipart: the md5 of the parts' md5s, and their count.
        assert_eq!(etag_md5(&format!("\"{hex}-3\"")), None);
        assert_eq!(etag_md5(&format!("W/\"{hex}\"")), None);
        assert_eq!(etag_md5("\"\""), None);
    }

    #[test]
    fn a_bucket_flag_is_an_s3_url_or_nothing() {
        let gap = MIN_GAP;
        assert!(BucketConfig::at("", gap).unwrap().is_none());
        assert!(BucketConfig::at("  ", Duration::ZERO).unwrap().is_none());
        let c = BucketConfig::at("https://ndnp-batches.s3.amazonaws.com", gap)
            .unwrap()
            .unwrap();
        assert_eq!(c.root, ROOT);
        assert_eq!(c.gap, Duration::from_secs(6));
        // Path-style, and a longer gap.
        let c = BucketConfig::at(
            "https://s3.us-east-1.amazonaws.com/ndnp-batches",
            Duration::from_secs(10),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (c.root.as_str(), c.gap),
            (
                "https://s3.us-east-1.amazonaws.com/ndnp-batches/",
                Duration::from_secs(10)
            )
        );
        assert!(BucketConfig::at("ftp://x.s3.amazonaws.com/", gap).is_err());
        assert!(BucketConfig::at("https://x.s3.amazonaws.com/?list-type=2", gap).is_err());
        assert!(BucketConfig::at("not a url", gap).is_err());
        // Never a LoC host, nor anything but S3.
        for root in [
            "https://tile.loc.gov/storage-services/service/ndnp/",
            "https://www.loc.gov/",
            "https://chroniclingamerica.loc.gov/data/batches/",
            "https://loc.gov/",
            "https://example.com/",
            "https://amazonaws.com.example.com/",
            "https://amazonaws.com/",
        ] {
            assert!(BucketConfig::at(root, gap).is_err(), "{root}");
        }
        // Nothing under 6 s apart.
        for ms in [0, 10, 5_999] {
            assert!(
                BucketConfig::at(ROOT, Duration::from_millis(ms)).is_err(),
                "{ms}"
            );
        }
    }
}
