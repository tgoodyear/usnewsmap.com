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
//! **What is taken.** A bag manifest whose body doesn't match the object's
//! `x-amz-meta-md5` (the uploader's md5), when it has one, is refused. A
//! METS or JP2 must match the bag's md5, as LoC's must. What the bucket
//! serves is recorded as LoC's: the bag's URL is the storage path its name
//! gives on LoC (the fetcher's first guess there), and a scan keeps LoC's
//! URL and IIIF base. Where a JP2's bytes came from is the caller's to
//! record ([`NAME`] and the bucket's URL).
//!
//! **Which batches.** The first answer for a batch says whether the bucket
//! serves it: a file it sends is public; a 403, or a 404 for its manifest,
//! is not, and nothing more of that batch is asked of the bucket in this
//! process. A 404 for a METS or JP2 of a public batch is that file only.
//!
//! **Pacing.** As for a mirror ([`super::mirror`]): one request at a time,
//! the next at least [`BucketConfig::gap`] (6 s) after the last one ended,
//! never through LoC's pacers or the host's LoC budget. A request waits at
//! most [`BucketConfig::max_wait`] for its turn and then goes to LoC. A 429
//! or 503 (S3's SlowDown) holds the bucket for `Retry-After` (at least
//! [`BucketConfig::backoff`], at most an hour), a challenge or an HTML page
//! for [`BucketConfig::block_hold`], and a 5xx, a network error or a body
//! that can't be read for the backoff. A held bucket is skipped, never
//! waited out, and nothing the bucket answers holds or slows LoC. A 403
//! holds nothing: it means the batch is private. No redirect is followed
//! (S3 sends one only towards another endpoint, which is another host).

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

/// How long a bag manifest may take (a 10,000-page batch's is about 3 MB).
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(120);
/// How long an issue METS may take.
const METS_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a JP2 may take.
const IMAGE_TIMEOUT: Duration = Duration::from_secs(120);
/// The uploader's md5 of an object.
const META_MD5: &str = "x-amz-meta-md5";

#[derive(Debug, Clone)]
pub struct BucketConfig {
    /// The bucket's root, ending in `/` ([`ROOT`]; a stand-in in tests).
    pub root: String,
    /// From the end of one request to the start of the next (6 s, the
    /// maintainer's).
    pub gap: Duration,
    /// A 5xx, a timeout, a network error or a body that can't be read
    /// holds the bucket this long, and a 429 or 503 at least this long.
    pub backoff: Duration,
    /// A challenge or an HTML page holds the bucket this long.
    pub block_hold: Duration,
    /// The longest a request waits for the bucket's turn before going to
    /// LoC without it.
    pub max_wait: Duration,
}

impl Default for BucketConfig {
    fn default() -> Self {
        Self {
            root: ROOT.into(),
            gap: Duration::from_secs(6),
            backoff: Duration::from_secs(60),
            block_hold: Duration::from_secs(3600),
            max_wait: Duration::from_secs(30),
        }
    }
}

impl BucketConfig {
    /// The bucket at `root` (`--loc-bucket`), with the default pacing;
    /// `None` for an empty `root`: off.
    pub fn at(root: &str) -> anyhow::Result<Option<Self>> {
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
        let mut root = u.to_string();
        if !root.ends_with('/') {
            root.push('/');
        }
        Ok(Some(Self {
            root,
            ..Self::default()
        }))
    }
}

/// The bucket's counts, for the fetcher's status.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Files of a public batch it doesn't have (404), and other refusals.
    pub misses: u64,
    /// Files that didn't match their md5 (the bag's, or a manifest's
    /// `x-amz-meta-md5`).
    pub md5_mismatches: u64,
    /// Requests it was skipped for: held, or busy past the wait.
    pub skipped_held: u64,
    /// Holds it set (429, 503, a challenge, an error).
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
    skipped_held: AtomicU64,
    holds: AtomicU64,
}

/// What one request got.
enum Tried {
    /// The body, how long it took, and the object's `x-amz-meta-md5`.
    Body(Bytes, Duration, Option<String>),
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

    /// Batch `dir`'s bag manifest, if the bucket serves it and the body
    /// matches the object's `x-amz-meta-md5` (when it has one); `None`
    /// sends the caller to LoC.
    pub(super) async fn manifest(&self, dir: &str) -> Option<Bytes> {
        let url = format!("{}{dir}/manifest-md5.txt", self.cfg.root);
        match self.get(dir, &url, MAX_BAG_BYTES, MANIFEST_TIMEOUT).await {
            Tried::Body(body, _, meta) => {
                if let Some(meta) = meta {
                    let got = batch::md5(&body);
                    if meta_md5(&meta) != Some(got) {
                        self.counts.md5_mismatches.fetch_add(1, Ordering::SeqCst);
                        tracing::warn!(
                            bucket = NAME,
                            url,
                            bytes = body.len(),
                            md5 = batch::hex(&got),
                            meta,
                            "a manifest from LoC's bucket isn't its x-amz-meta-md5; refused"
                        );
                        return None;
                    }
                }
                self.learn(dir, true);
                self.counts.manifests.fetch_add(1, Ordering::SeqCst);
                Some(body)
            }
            // No manifest: the bucket doesn't serve the batch.
            Tried::Private | Tried::Missing => {
                self.learn(dir, false);
                None
            }
            Tried::Skipped | Tried::Failed => None,
        }
    }

    /// An issue METS of batch `dir` (`rel`, its path in the bag), if the
    /// bucket serves it and it matches the bag's `md5`.
    pub(super) async fn mets(&self, dir: &str, rel: &str, md5: &Md5Sum) -> Option<Bytes> {
        let (body, _, _) = self
            .checked(dir, rel, md5, MAX_METS_BYTES, METS_TIMEOUT)
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
            .checked(dir, rel, &page.md5, MAX_IMAGE_BYTES, IMAGE_TIMEOUT)
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
        match self.get(dir, &url, max, timeout).await {
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
            Tried::Private => {
                self.learn(dir, false);
                None
            }
            Tried::Missing => {
                self.counts.misses.fetch_add(1, Ordering::SeqCst);
                None
            }
            Tried::Skipped | Tried::Failed => None,
        }
    }

    /// One GET in the bucket's turn, with no retry. A held or busy bucket,
    /// or a batch it is known not to serve, isn't asked.
    async fn get(&self, dir: &str, url: &str, max: usize, timeout: Duration) -> Tried {
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
            let meta = resp
                .headers()
                .get(META_MD5)
                .map(|v| v.to_str().unwrap_or_default().to_owned());
            return match read_limited(resp, max).await {
                Ok(body) => Tried::Body(body, sent.elapsed(), meta),
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
            300..=499 => {
                tracing::debug!(bucket = NAME, url, %status, "LoC's bucket refused or redirected; not followed");
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
fn key<'a>(storage_root: &str, url: &'a str) -> Option<(&'a str, &'a str)> {
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
    fn a_bucket_flag_is_a_plain_url_or_nothing() {
        assert!(BucketConfig::at("").unwrap().is_none());
        assert!(BucketConfig::at("  ").unwrap().is_none());
        let c = BucketConfig::at("https://ndnp-batches.s3.amazonaws.com")
            .unwrap()
            .unwrap();
        assert_eq!(c.root, ROOT);
        assert_eq!(c.gap, Duration::from_secs(6));
        assert!(BucketConfig::at("ftp://x/").is_err());
        assert!(BucketConfig::at("https://x/?list-type=2").is_err());
        assert!(BucketConfig::at("not a url").is_err());
    }
}
