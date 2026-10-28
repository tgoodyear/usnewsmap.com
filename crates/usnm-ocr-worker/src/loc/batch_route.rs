//! Checked batch URLs, bag/METS caching and storage-route scan resolution.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use opentelemetry::KeyValue;
use reqwest::StatusCode;
use usnm_core::ids::PageKey;

use crate::batch::{self, BagIndex, Md5Sum};

use super::{
    bucket, challenge, html_page, iiif_base, image_url, jpeg, mirror, retry_after, FetchError,
    ImageSize, LocClient, Pages, Scan, ScanFormat, Server, MAX_BAG_BYTES, MAX_IMAGE_BYTES,
    MAX_METS_BYTES,
};

/// Beside a batch's kept manifest: its directory on the storage service, as
/// LoC served the manifest there or Chronicling America's redirect named it.
const URL_FILE: &str = "url";
/// Beside a manifest from LoC's bucket instead: only the guess the batch's
/// name gives ([`Bag::guessed`]).
const GUESSED_URL_FILE: &str = "url.guessed";
/// A guess LoC's checked URL later replaced: the batch's earlier sidecars,
/// fetched under the guess, name it.
const WRONG_URL_FILE: &str = "url.wrong";

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
                target: "usnm_ocr_worker::loc",
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
                tracing::warn!(target: "usnm_ocr_worker::loc",
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
                        tracing::warn!(target: "usnm_ocr_worker::loc",
                            batch = dir,
                            to,
                            "the batch's redirect leaves the storage service; not following it"
                        );
                    }
                }
            }
        }
        let Some((url, body, from, guessed)) = found else {
            tracing::warn!(target: "usnm_ocr_worker::loc",
                batch = dir,
                "LoC has no manifest for this batch; its pages go through the item API"
            );
            return Ok(None);
        };
        let text = String::from_utf8_lossy(&body).into_owned();
        let index = BagIndex::parse(&text);
        tracing::info!(target: "usnm_ocr_worker::loc",
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
                tracing::warn!(target: "usnm_ocr_worker::loc", batch = dir, error = %e, "can't keep the batch manifest on disk");
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
                        target: "usnm_ocr_worker::loc",
                        batch = dir,
                        url = bag.url,
                        concat!(
                            "LoC answered 404 under the guessed URL of a batch whose manifest came from LoC's bucket, ",
                            "and Chronicling America names no other place on the storage service; keeping the guess"
                        )
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
            tracing::warn!(target: "usnm_ocr_worker::loc",
                batch = dir,
                guessed = bag.url,
                url,
                "a batch whose manifest came from LoC's bucket isn't where its name says: moved to where Chronicling America's redirect says (pages of it already fetched keep the guessed URL in their sidecars)"
            );
        } else {
            tracing::info!(target: "usnm_ocr_worker::loc",
                batch = dir,
                url,
                "Chronicling America confirms a batch's guessed URL"
            );
        }
        if let Some(c) = self.cache_dir(dir) {
            if let Err(e) = write_checked_url(&c, &url).await {
                tracing::warn!(target: "usnm_ocr_worker::loc", batch = dir, error = %e, "can't keep the batch's URL on disk");
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

    /// Keep batch `dir`'s guessed URL `url` as checked: LoC served one of
    /// its files there, matching the bag's md5 (a METS in
    /// [`Self::issue_frames`], a JP2 in [`Self::scan_resolved`]), as it
    /// would the manifest. Only while the bag in memory is still that
    /// guess, and once for the batch's pages in flight at once.
    async fn confirm(&self, dir: &str, url: &str) {
        let Some(Some(bag)) = self.bags.get(dir).await else {
            return;
        };
        if !bag.guessed || bag.url != url {
            return;
        }
        if !self.confirming.lock().unwrap().insert(dir.to_owned()) {
            return;
        }
        tracing::info!(target: "usnm_ocr_worker::loc",
            batch = dir,
            url,
            "LoC serves a batch's files under its guessed URL; keeping it as checked"
        );
        if let Some(c) = self.cache_dir(dir) {
            if let Err(e) = write_checked_url(&c, url).await {
                tracing::warn!(target: "usnm_ocr_worker::loc", batch = dir, error = %e, "can't keep the batch's URL on disk");
            }
        }
        self.bags
            .insert(
                dir.to_owned(),
                Some(Arc::new(Bag {
                    dir: dir.to_owned(),
                    url: url.to_owned(),
                    guessed: false,
                    index: bag.index.clone(),
                })),
            )
            .await;
        self.confirming.lock().unwrap().remove(dir);
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
                                let under_bag_url = got.is_some();
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
                                    tracing::warn!(target: "usnm_ocr_worker::loc", url, "the issue's METS isn't there");
                                    return Ok(None);
                                };
                                if batch::md5(&got.body) != md5 {
                                    return Err(FetchError::Transient(format!(
                                        "{url}: md5 {} isn't the bag's {}",
                                        batch::md5_hex(&got.body),
                                        batch::hex(&md5)
                                    )));
                                }
                                if under_bag_url && bag.guessed {
                                    self.confirm(&bag.dir, &bag.url).await;
                                }
                                got.body
                            }
                        };
                        if let Some(p) = &disk {
                            if let Err(e) = write_file(p, &body).await {
                                tracing::warn!(target: "usnm_ocr_worker::loc", url, error = %e, "can't keep the METS on disk");
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
        let (page, got, first) = match jp2(page.url.clone()).await? {
            Some(got) => (page.clone(), got, true),
            None => {
                let moved = self.moved(page).await?;
                let got = match &moved {
                    Some(p) => jp2(p.url.clone()).await?,
                    None => None,
                };
                let (Some(p), Some(got)) = (moved, got) else {
                    tracing::warn!(target: "usnm_ocr_worker::loc", url = page.url, "a JP2 the bag lists isn't there");
                    return Ok(None);
                };
                (p, got, false)
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
        // Served where the bag put it: if that is a guess, it was right.
        if first && self.bucket.is_some() {
            if let Some((dir, _)) = bucket::key(&self.cfg.storage_root, &page.url) {
                let guess = batch::storage_batch_url(&self.cfg.storage_root, dir);
                if page.url.starts_with(&guess) {
                    self.confirm(dir, &guess).await;
                }
            }
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
