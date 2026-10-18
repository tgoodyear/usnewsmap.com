//! Separate, unwired scan streaming service. See README.md for the reader identity contract
//! and deployment gates. This crate neither signs tickets nor binds a network listener.

use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use bytes::Bytes;
use ed25519_dalek::{Signature, VerifyingKey};
use futures::{Stream, StreamExt};
use md5::Md5;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

/// Implementations must use their own private archive reader identity, a fixed container,
/// and conditional reads (If-Match). Never accept a caller URL, SAS, or credentials.
#[async_trait]
pub trait ArchiveReader: Send + Sync {
    async fn properties(&self, blob: &str) -> Result<Properties, ArchiveError>;
    /// Return exactly the requested inclusive byte interval, pinned to `etag`.
    async fn read(
        &self,
        blob: &str,
        range: ByteRange,
        etag: &str,
    ) -> Result<ByteStream, ArchiveError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("archive object not found")]
    NotFound,
    #[error("archive unavailable or object changed")]
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct Properties {
    pub bytes: u64,
    pub sha256: String,
    pub md5: String,
    pub etag: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

// Field order is the canonical JSON order. All objects have fixed, sorted fields, except
// `blobs`, whose keys are sorted by BTreeMap. Unknown fields are rejected, not unsigned.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub app_name: String,
    pub app_version: String,
    pub blobs: BTreeMap<String, Scan>,
    pub expires_at: u64,
    pub issued_at: u64,
    pub key_id: String,
    pub ticket_id: String,
    pub workunit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    pub bytes: u64,
    pub md5: String,
    pub sha256: String,
}

impl Ticket {
    /// Exact Ed25519 message bytes: compact UTF-8 JSON with recursively sorted keys.
    /// Integers only; the fixed schema has no floating point or optional fields.
    pub fn canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

pub trait Clock: Send + Sync {
    fn unix_seconds(&self) -> u64;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_ticket_seconds: u64,
    pub archive_timeout_seconds: u64,
    pub max_pages: usize,
    pub max_scan_bytes: u64,
    pub retry_multiple: u64,
    pub ip_requests_per_minute: u64,
    pub ip_bytes_per_minute: u64,
    pub replica_bytes_per_day: u64,
    pub max_live_tickets: usize,
    pub max_live_ips: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_ticket_seconds: 86400,
            archive_timeout_seconds: 60,
            max_pages: 32,
            max_scan_bytes: 64 * 1024 * 1024,
            retry_multiple: 3,
            ip_requests_per_minute: 120,
            ip_bytes_per_minute: 256 * 1024 * 1024,
            replica_bytes_per_day: 20 * 1024 * 1024 * 1024,
            max_live_tickets: 10000,
            max_live_ips: 10000,
        }
    }
}

struct Counter {
    until: u64,
    requests: u64,
    bytes: u64,
}
#[derive(Default)]
struct Budgets {
    tickets: BTreeMap<[u8; 32], Counter>,
    ips: BTreeMap<IpAddr, Counter>,
    day: u64,
    day_bytes: u64,
}

pub struct Service {
    reader: Arc<dyn ArchiveReader>,
    keys: BTreeMap<String, VerifyingKey>,
    limits: Limits,
    clock: Arc<dyn Clock>,
    budgets: Mutex<Budgets>,
}

impl Service {
    pub fn new(
        reader: Arc<dyn ArchiveReader>,
        keys: BTreeMap<String, VerifyingKey>,
        limits: Limits,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, &'static str> {
        if keys.is_empty()
            || limits.archive_timeout_seconds == 0
            || limits.archive_timeout_seconds > 3600
            || limits.max_ticket_seconds == 0
            || limits.max_pages == 0
            || limits.max_scan_bytes == 0
            || limits.retry_multiple == 0
            || limits.ip_requests_per_minute == 0
            || limits.ip_bytes_per_minute == 0
            || limits.replica_bytes_per_day == 0
            || limits.max_live_tickets == 0
            || limits.max_live_ips == 0
        {
            return Err("public keys and nonzero limits are required");
        }
        Ok(Self {
            reader,
            keys,
            limits,
            clock,
            budgets: Mutex::default(),
        })
    }

    /// The host must supply ConnectInfo<SocketAddr> from the real peer. Forwarded headers
    /// are deliberately ignored. No listener or deployment is provided by this crate.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/v1/scans/{*blob}", get(download))
            .with_state(self)
    }

    fn verify(&self, headers: &HeaderMap, now: u64) -> Result<(Ticket, [u8; 32]), Failure> {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or(Failure::Unauthorized)?;
        if auth.len() > 16384 {
            return Err(Failure::Unauthorized);
        }
        let (payload, signature) = auth.split_once('.').ok_or(Failure::Unauthorized)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Failure::Unauthorized)?;
        let ticket: Ticket = serde_json::from_slice(&payload).map_err(|_| Failure::Unauthorized)?;
        let canonical = ticket.canonical_json().map_err(|_| Failure::Unauthorized)?;
        if canonical != payload {
            return Err(Failure::Unauthorized);
        }
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| Failure::Unauthorized)?;
        let signature = Signature::from_slice(&signature).map_err(|_| Failure::Unauthorized)?;
        self.keys
            .get(&ticket.key_id)
            .ok_or(Failure::Unauthorized)?
            .verify_strict(&canonical, &signature)
            .map_err(|_| Failure::Unauthorized)?;
        if ticket.issued_at > now
            || ticket.expires_at <= now
            || ticket.expires_at <= ticket.issued_at
            || ticket.expires_at - ticket.issued_at > self.limits.max_ticket_seconds
            || !identifier(&ticket.ticket_id)
            || !identifier(&ticket.workunit)
            || !identifier(&ticket.app_name)
            || !identifier(&ticket.app_version)
            || ticket.blobs.is_empty()
            || ticket.blobs.len() > self.limits.max_pages
        {
            return Err(Failure::Unauthorized);
        }
        for (blob, scan) in &ticket.blobs {
            if !blob_name(blob)
                || scan.bytes == 0
                || scan.bytes > self.limits.max_scan_bytes
                || scan.sha256.len() != 64
                || !scan
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                || STANDARD.decode(&scan.md5).map_or(true, |m| m.len() != 16)
            {
                return Err(Failure::Unauthorized);
            }
        }
        Ok((ticket, Sha256::digest(&canonical).into()))
    }

    // Charge before any archive I/O. Reservations are never refunded, including failures,
    // disconnects and ranges: retrying an aborted response must not evade retrieval bounds.
    fn reserve(
        &self,
        ticket: &Ticket,
        id: [u8; 32],
        ip: IpAddr,
        bytes: u64,
        now: u64,
        request: bool,
    ) -> Result<(), Failure> {
        let mut budgets = self.budgets.lock().map_err(|_| Failure::Upstream)?;
        budgets.tickets.retain(|_, c| c.until > now);
        budgets.ips.retain(|_, c| c.until > now);
        let day = now / 86400;
        // A backwards clock does not reset the daily brake.
        if day > budgets.day {
            budgets.day = day;
            budgets.day_bytes = 0;
        }
        let until_day = budgets.day.saturating_add(1).saturating_mul(86400);
        let ip_until = now.saturating_add(60 - now % 60);
        let total = ticket
            .blobs
            .values()
            .try_fold(0u64, |a, s| a.checked_add(s.bytes))
            .and_then(|v| v.checked_mul(self.limits.retry_multiple))
            .ok_or(Failure::Unauthorized)?;
        let requests = (ticket.blobs.len() as u64)
            .checked_mul(self.limits.retry_multiple)
            .ok_or(Failure::Unauthorized)?;
        if (!budgets.tickets.contains_key(&id)
            && budgets.tickets.len() >= self.limits.max_live_tickets)
            || (!budgets.ips.contains_key(&ip) && budgets.ips.len() >= self.limits.max_live_ips)
        {
            return Err(Failure::Limited(60));
        }
        if let Some(c) = budgets.tickets.get(&id) {
            if (request && c.requests >= requests) || bytes > total.saturating_sub(c.bytes) {
                return Err(Failure::Limited(
                    ticket.expires_at.saturating_sub(now).max(1),
                ));
            }
        }
        if let Some(c) = budgets.ips.get(&ip) {
            if (request && c.requests >= self.limits.ip_requests_per_minute)
                || bytes > self.limits.ip_bytes_per_minute.saturating_sub(c.bytes)
            {
                return Err(Failure::Limited(c.until.saturating_sub(now).max(1)));
            }
        }
        if bytes > total || bytes > self.limits.ip_bytes_per_minute {
            return Err(Failure::Limited(60));
        }
        if bytes
            > self
                .limits
                .replica_bytes_per_day
                .saturating_sub(budgets.day_bytes)
        {
            return Err(Failure::Limited(until_day.saturating_sub(now).max(1)));
        }
        let c = budgets.tickets.entry(id).or_insert(Counter {
            until: ticket.expires_at,
            requests: 0,
            bytes: 0,
        });
        c.requests += u64::from(request);
        c.bytes += bytes;
        let c = budgets.ips.entry(ip).or_insert(Counter {
            until: ip_until,
            requests: 0,
            bytes: 0,
        });
        c.requests += u64::from(request);
        c.bytes += bytes;
        budgets.day_bytes += bytes;
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

fn blob_name(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 6
        && parts[0] == "keep"
        && parts
            .iter()
            .all(|p| identifier(p) && *p != "." && *p != "..")
        && (s.ends_with(".jp2") || s.ends_with(".jpg"))
}

#[derive(Debug)]
enum Failure {
    Unauthorized,
    Forbidden,
    Missing,
    Upstream,
    Range(u64),
    Limited(u64),
    Method,
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let (status, extra) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Some((header::WWW_AUTHENTICATE, "Bearer".to_owned())),
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, None),
            Self::Missing => (StatusCode::NOT_FOUND, None),
            Self::Upstream => (StatusCode::BAD_GATEWAY, None),
            Self::Range(n) => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                Some((header::CONTENT_RANGE, format!("bytes */{n}"))),
            ),
            Self::Limited(s) => (
                StatusCode::TOO_MANY_REQUESTS,
                Some((header::RETRY_AFTER, s.to_string())),
            ),
            Self::Method => (
                StatusCode::METHOD_NOT_ALLOWED,
                Some((header::ALLOW, "GET".to_owned())),
            ),
        };
        let mut response = status.into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        if let Some((name, value)) = extra {
            if let Ok(value) = HeaderValue::from_str(&value) {
                response.headers_mut().insert(name, value);
            }
        }
        response
    }
}

impl From<ArchiveError> for Failure {
    fn from(error: ArchiveError) -> Self {
        match error {
            ArchiveError::NotFound => Self::Missing,
            ArchiveError::Unavailable => Self::Upstream,
        }
    }
}

fn range(headers: &HeaderMap, size: u64) -> Result<(ByteRange, bool), Failure> {
    let full = ByteRange {
        start: 0,
        end: size - 1,
    };
    let values: Vec<_> = headers.get_all(header::RANGE).iter().collect();
    if values.is_empty() {
        return Ok((full, false));
    }
    if values.len() != 1 {
        return Err(Failure::Range(size));
    }
    let s = values[0]
        .to_str()
        .ok()
        .and_then(|s| s.strip_prefix("bytes="))
        .ok_or(Failure::Range(size))?;
    let (start, end) = s.split_once('-').ok_or(Failure::Range(size))?;
    let number = |s: &str| -> Result<u64, Failure> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            return Err(Failure::Range(size));
        }
        s.parse().map_err(|_| Failure::Range(size))
    };
    let (start, end) = if start.is_empty() {
        let suffix = number(end)?;
        if suffix == 0 {
            return Err(Failure::Range(size));
        }
        (size.saturating_sub(suffix), size - 1)
    } else {
        (
            number(start)?,
            if end.is_empty() {
                size - 1
            } else {
                number(end)?.min(size - 1)
            },
        )
    };
    if start > end || start >= size {
        return Err(Failure::Range(size));
    }
    Ok((ByteRange { start, end }, true))
}

async fn download(
    State(service): State<Arc<Service>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(blob): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, Failure> {
    if method != Method::GET {
        return Err(Failure::Method);
    }
    let now = service.clock.unix_seconds();
    let (ticket, id) = service.verify(&headers, now)?;
    if headers.get("x-usnm-workunit").and_then(|h| h.to_str().ok()) != Some(&ticket.workunit) {
        return Err(Failure::Forbidden);
    }
    let scan = ticket.blobs.get(&blob).ok_or(Failure::Forbidden)?;
    // Malformed ranges still consume a request, but perform no archive reads.
    let requested = range(&headers, scan.bytes);
    let mut bytes = requested.as_ref().map_or(0, |(r, _)| r.end - r.start + 1);
    service.reserve(&ticket, id, peer.ip(), bytes, now, true)?;
    let (mut interval, mut partial) = requested?;
    // If-Range mismatch means a full representation, not an unpinned partial response.
    // Reserve the extra bytes before the GET if metadata later shows a mismatch.
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(service.limits.archive_timeout_seconds);
    let properties = tokio::time::timeout_at(deadline, service.reader.properties(&blob))
        .await
        .map_err(|_| Failure::Upstream)??;
    if properties.bytes != scan.bytes
        || properties.md5 != scan.md5
        || properties.sha256 != scan.sha256
    {
        return Err(Failure::Upstream);
    }
    let etag = HeaderValue::from_str(&properties.etag).map_err(|_| Failure::Upstream)?;
    if properties.etag.len() < 2
        || !properties.etag.starts_with('"')
        || !properties.etag.ends_with('"')
        || !properties.etag.as_bytes()[1..properties.etag.len() - 1]
            .iter()
            .all(|&c| c == 0x21 || (0x23..=0x7e).contains(&c))
    {
        return Err(Failure::Upstream);
    }
    if partial
        && headers.contains_key(header::IF_RANGE)
        && headers.get(header::IF_RANGE) != Some(&etag)
    {
        service.reserve(&ticket, id, peer.ip(), scan.bytes - bytes, now, false)?;
        bytes = scan.bytes;
        interval = ByteRange {
            start: 0,
            end: scan.bytes - 1,
        };
        partial = false;
    }
    // No automatic retries or source fallback: a caller can fall back politely on 404,
    // 429, 5xx, timeout, or integrity failure, retaining the job's provenance URL.
    let stream = tokio::time::timeout_at(
        deadline,
        service.reader.read(&blob, interval, &properties.etag),
    )
    .await
    .map_err(|_| Failure::Upstream)??;
    let full = interval.start == 0 && interval.end == scan.bytes - 1;
    let body = checked_stream(stream, bytes, full.then_some(scan.clone()), deadline);
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = response.headers_mut();
    h.insert(header::ETAG, etag);
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    // Never share/cache an authorization-gated body outside this ticket's lifetime.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if blob.ends_with(".jp2") {
            "image/jp2"
        } else {
            "image/jpeg"
        }),
    );
    if full {
        h.insert(
            "content-md5",
            HeaderValue::from_str(&scan.md5).map_err(|_| Failure::Upstream)?,
        );
    }
    if partial {
        let content_range = format!("bytes {}-{}/{}", interval.start, interval.end, scan.bytes);
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range).map_err(|_| Failure::Upstream)?,
        );
    }
    Ok(response)
}

fn checked_stream(
    stream: ByteStream,
    remaining: u64,
    scan: Option<Scan>,
    deadline: tokio::time::Instant,
) -> ByteStream {
    Box::pin(futures::stream::try_unfold(
        (stream, remaining, Sha256::new(), Md5::new(), scan, false),
        move |(mut stream, remaining, mut sha, mut md5, scan, done)| async move {
            if done {
                return Ok(None);
            }
            let invalid =
                || io::Error::new(io::ErrorKind::InvalidData, "archive integrity failure");
            let chunk = loop {
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "archive timeout"));
                }
                let chunk = tokio::time::timeout_at(deadline, stream.next())
                    .await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "archive timeout"))?
                    .ok_or_else(invalid)??;
                if !chunk.is_empty() {
                    break chunk;
                }
                tokio::task::yield_now().await;
            };
            let remaining = remaining
                .checked_sub(chunk.len() as u64)
                .ok_or_else(invalid)?;
            sha.update(&chunk);
            md5.update(&chunk);
            if remaining == 0 {
                // Hold the final chunk until EOF and checksum verification, so a bad body
                // cannot complete successfully even with an advertised Content-Length.
                while let Some(extra) = tokio::time::timeout_at(deadline, stream.next())
                    .await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "archive timeout"))?
                {
                    if !extra?.is_empty() {
                        return Err(invalid());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "archive timeout"));
                    }
                    tokio::task::yield_now().await;
                }
                if let Some(scan) = &scan {
                    if hex(&sha.clone().finalize()) != scan.sha256
                        || STANDARD.encode(md5.clone().finalize()) != scan.md5
                    {
                        return Err(invalid());
                    }
                }
            }
            Ok(Some((
                chunk,
                (stream, remaining, sha, md5, scan, remaining == 0),
            )))
        },
    ))
}

#[cfg(test)]
mod tests;
