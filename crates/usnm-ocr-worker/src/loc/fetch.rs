//! Source politeness, server health, bounded GETs, probes and block holds.

use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use opentelemetry::KeyValue;
use reqwest::StatusCode;
use tokio::time::Instant;

use super::{
    read_limited, FetchError, LocClient, Pacer, Server, ITEM_ERROR_FAST, ITEM_ERROR_OK_WITHIN,
    ITEM_ERROR_TITLES, ITEM_ERROR_WINDOW, MAX_ITEM_BYTES, MAX_RETRY_AFTER, PROBE_MAX_BYTES,
};

/// What a server's answers say about telling an item error from a hold.
#[derive(Debug, Default)]
pub(super) struct ApiHealth {
    /// The last small file (at most [`PROBE_MAX_BYTES`]) that answered
    /// 2xx, or one set by [`LocClient::set_known_good`]: what a check asks
    /// for again.
    pub(super) known_good: Option<String>,
    /// When anything on the server last answered 2xx.
    pub(super) last_ok: Option<Instant>,
    /// Item errors lately: when, and the title.
    pub(super) item_errors: std::collections::VecDeque<(Instant, String)>,
}

/// Why a known-good item cannot establish that the server is serving.
pub(super) enum ProbeFailure {
    /// It failed too: a hold, for at least this long if it said so.
    Failed(Option<Duration>),
    /// A strict block already held and counted; stop the original fetch.
    Blocked(FetchError),
}

/// A successful GET.
#[derive(Debug)]
pub(super) struct Got {
    pub(super) body: Bytes,
    /// Sending to the end of the body.
    pub(super) took: Duration,
}

/// A `Retry-After` value (seconds or an HTTP date), capped at an hour, and
/// whether the cap cut it.
pub(super) fn retry_after(resp: &reqwest::Response) -> Option<(Duration, bool)> {
    let v = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    let d = usnm_core::retry_after::parse(v, std::time::SystemTime::now())?;
    Some((d.min(MAX_RETRY_AFTER), d > MAX_RETRY_AFTER))
}

/// Why a 2xx answer is a web page rather than a file: a challenge header
/// or HTML content type. None for images, JSON, XML and text manifests.
pub(super) fn html_page(resp: &reqwest::Response) -> Option<String> {
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
pub(super) fn challenge(resp: &reqwest::Response) -> Option<String> {
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

impl LocClient {
    /// LoC pushed back with a 429, a 403 or a challenge page: hold the
    /// host's whole budget for `d`, and drop it to the slow rate for good
    /// (the maintainer's rule of 24 October 2026: 45 a minute, 10 if LoC
    /// objects). A 503 or an unreadable body holds only its server.
    pub(super) fn push_back(&self, status: StatusCode, challenged: bool, d: Duration) {
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

    pub(super) fn pacer(&self, server: Server) -> &Pacer {
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
                tracing::warn!(target: "usnm_ocr_worker::loc",
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
    pub(super) async fn probe(&self, server: Server, url: &str) -> Result<(), ProbeFailure> {
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
                tracing::warn!(target: "usnm_ocr_worker::loc",
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
                tracing::warn!(target: "usnm_ocr_worker::loc", error = %e.without_url(), server = server.name(), "the known-good item failed too");
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
    pub(super) async fn get(
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

    pub(super) fn block_hold(
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
    pub(super) async fn fetch(
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
                            tracing::warn!(target: "usnm_ocr_worker::loc",
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
                                    tracing::warn!(target: "usnm_ocr_worker::loc",
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
                        tracing::warn!(target: "usnm_ocr_worker::loc",
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
                tracing::warn!(target: "usnm_ocr_worker::loc", url, attempt, error = %last, wait_secs = wait.as_secs(), "retrying");
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
}
