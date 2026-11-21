//! Cloudflare R2 through its S3 API, for the BOINC pieces that run in
//! Cloudflare (the validator container) and the on-prem hosts that read their
//! buckets.
//!
//! A location is `r2://<bucket>[/<prefix>]`. The account endpoint and a
//! bucket-scoped API token come from the environment, as in the repo's other
//! R2 users (ADR-0024): `R2_ENDPOINT` (`https://<account id>.r2.cloudflarestorage.com`),
//! `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY`. Requests are signed with
//! AWS Signature Version 4 (region `auto`, service `s3`), path-style, with
//! the body's SHA-256. The secret is never logged or put in an error.
//!
//! What it does: `get`, `put`, `put_new` (a create with `If-None-Match: *`,
//! which R2 answers 412 when the key exists), `exists` and `modified` (HEAD),
//! `list`, `list_stem` and `list_info` (ListObjectsV2, every page), and
//! `get_if_match`, `get_stream` (the body read as it arrives) and `delete_if_match`: a HEAD
//! that must show the ETag, then a DELETE carrying `If-Match`. Only the service enforcing
//! `If-Match` on the DELETE makes it atomic; if it ignores the header, an object written again
//! between the HEAD and the DELETE is deleted. Its one user, `cloud pull-results`, deletes keys
//! the export writes once and never again (every publication has a key of its own), so that
//! window has nothing to race with there; check the header is honoured before relying on it
//! for keys that are rewritten.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::{Method, StatusCode};
use sha2::{Digest, Sha256};

use crate::retry::{retryable, retrying, Retry};
use crate::{validate_path, ByteStream, ObjectInfo, ObjectStore, StoreError, MAX_OBJECT_BYTES};

/// Environment variables the store reads its endpoint and token from.
pub const ENV_ENDPOINT: &str = "R2_ENDPOINT";
pub const ENV_KEY_ID: &str = "R2_ACCESS_KEY_ID";
pub const ENV_SECRET: &str = "R2_SECRET_ACCESS_KEY";

const REGION: &str = "auto";
const SERVICE: &str = "s3";
/// Keys a ListObjectsV2 page holds at most.
const LIST_PAGE: usize = 1000;

/// An access key pair. The secret is kept out of `Debug`.
#[derive(Clone)]
pub struct R2Key {
    pub id: String,
    pub secret: String,
}

impl std::fmt::Debug for R2Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("R2Key")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

pub struct R2Store {
    /// `https://<account>.r2.cloudflarestorage.com`, no trailing slash.
    endpoint: String,
    /// The `Host` header value the signature covers.
    host: String,
    bucket: String,
    /// Object key prefix with a trailing `/`, or empty.
    prefix: String,
    key: R2Key,
    http: reqwest::Client,
    retry: Retry,
}

impl std::fmt::Debug for R2Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("R2Store")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl R2Store {
    /// Opens `r2://<bucket>[/<prefix>]` with the endpoint and key from the
    /// environment.
    pub fn from_env(location: &str) -> Result<Self, StoreError> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| StoreError::Credential(format!("{name} is not set")))
        };
        let key = R2Key {
            id: var(ENV_KEY_ID)?,
            secret: var(ENV_SECRET)?,
        };
        Self::new(location, &var(ENV_ENDPOINT)?, key)
    }

    /// `endpoint` must be `https`, or `http` on a loopback host (tests).
    pub fn new(location: &str, endpoint: &str, key: R2Key) -> Result<Self, StoreError> {
        let bad = |why: &str| StoreError::InvalidLocation(format!("{location}: {why}"));
        let rest = location
            .strip_prefix("r2://")
            .ok_or_else(|| bad("must start with r2://"))?
            .trim_matches('/');
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        let bucket_ok = (3..=63).contains(&bucket.len())
            && bucket
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
        if !bucket_ok {
            return Err(bad("the bucket name must be 3 to 63 of a-z, 0-9 and -"));
        }
        if !prefix.is_empty() {
            validate_path(prefix).map_err(|_| bad("the prefix must be plain path segments"))?;
        }
        let url = reqwest::Url::parse(endpoint)
            .map_err(|e| StoreError::InvalidLocation(format!("{ENV_ENDPOINT}: {e}")))?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback)) {
            return Err(StoreError::InvalidLocation(format!(
                "{ENV_ENDPOINT} must be https"
            )));
        }
        if !url.path().trim_matches('/').is_empty() || url.query().is_some() {
            return Err(StoreError::InvalidLocation(format!(
                "{ENV_ENDPOINT} must be the account endpoint with no path"
            )));
        }
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(Self {
            endpoint: url.origin().ascii_serialization(),
            host,
            bucket: bucket.to_owned(),
            prefix: if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}/")
            },
            key,
            http,
            retry: Retry::STANDARD,
        })
    }

    #[cfg(test)]
    fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    fn object_key(&self, path: &str) -> Result<String, StoreError> {
        validate_path(path)?;
        Ok(format!("{}{path}", self.prefix))
    }

    /// One signed request. `key` is the object key (empty for a bucket
    /// request); `extra` are headers the signature covers too.
    async fn send(
        &self,
        method: Method,
        key: &str,
        query: &[(&str, String)],
        extra: &[(&str, String)],
        body: Bytes,
        op: &'static str,
    ) -> Result<reqwest::Response, StoreError> {
        let uri = canonical_uri(&self.bucket, key);
        let now = SystemTime::now();
        let signed = sign(&SignInput {
            method: method.as_str(),
            uri: &uri,
            query,
            host: &self.host,
            extra,
            payload_sha256: &hex(&Sha256::digest(&body)),
            time: now,
            region: REGION,
            service: SERVICE,
            key: &self.key,
        });
        let mut url = format!("{}{uri}", self.endpoint);
        let q = canonical_query(query);
        if !q.is_empty() {
            url.push('?');
            url.push_str(&q);
        }
        let mut req = self
            .http
            .request(method, url)
            .header("x-amz-date", &signed.amz_date)
            .header("x-amz-content-sha256", &signed.payload_sha256)
            .header("authorization", &signed.authorization);
        for (name, value) in extra {
            req = req.header(*name, value);
        }
        req.body(body)
            .send()
            .await
            .map_err(|e| StoreError::Io(format!("{op} `{key}`: {}", e.without_url())))
    }

    async fn get_once(
        &self,
        path: &str,
        if_match: Option<&str>,
        limit: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let key = self.object_key(path)?;
        let extra: Vec<(&str, String)> = if_match
            .map(|e| ("if-match", quoted(e)))
            .into_iter()
            .collect();
        let mut resp = self
            .send(Method::GET, &key, &[], &extra, Bytes::new(), "get")
            .await?;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND | StatusCode::PRECONDITION_FAILED => return Ok(None),
            s => return Err(http("get", path, s)),
        }
        if resp.content_length().is_some_and(|n| n > limit) {
            return Err(StoreError::TooLarge(path.into()));
        }
        // The length header can be missing, so the limit holds while reading too: the rest of a
        // larger body is never read.
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| StoreError::Io(format!("get `{path}`: {}", e.without_url())))?
        {
            if (body.len() + chunk.len()) as u64 > limit {
                return Err(StoreError::TooLarge(path.into()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }

    async fn head(&self, path: &str) -> Result<Option<reqwest::header::HeaderMap>, StoreError> {
        let key = self.object_key(path)?;
        retrying("head", path, self.retry, retryable, |_| async {
            let resp = self
                .send(Method::HEAD, &key, &[], &[], Bytes::new(), "head")
                .await?;
            match resp.status() {
                StatusCode::OK => Ok(Some(resp.headers().clone())),
                StatusCode::NOT_FOUND => Ok(None),
                s => Err(http("head", path, s)),
            }
        })
        .await
    }

    async fn put_once(
        &self,
        key: &str,
        path: &str,
        body: Bytes,
        content_type: &str,
        create_only: bool,
    ) -> Result<bool, StoreError> {
        let mut extra = vec![("content-type", content_type.to_owned())];
        if create_only {
            extra.push(("if-none-match", "*".to_owned()));
        }
        let resp = self
            .send(Method::PUT, key, &[], &extra, body, "put")
            .await?;
        match resp.status() {
            StatusCode::OK => Ok(true),
            StatusCode::PRECONDITION_FAILED if create_only => Ok(false),
            s => Err(http("put", path, s)),
        }
    }

    /// Every object under the key prefix `stem` (already including the
    /// store's prefix), over all pages.
    async fn list_objects(&self, stem: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![
                ("list-type", "2".to_owned()),
                ("max-keys", LIST_PAGE.to_string()),
                ("prefix", stem.to_owned()),
            ];
            if let Some(t) = &token {
                query.push(("continuation-token", t.clone()));
            }
            let xml = retrying("list", stem, self.retry, retryable, |_| async {
                let resp = self
                    .send(Method::GET, "", &query, &[], Bytes::new(), "list")
                    .await?;
                if resp.status() != StatusCode::OK {
                    return Err(http("list", stem, resp.status()));
                }
                resp.text()
                    .await
                    .map_err(|e| StoreError::Io(format!("list `{stem}`: {}", e.without_url())))
            })
            .await?;
            let page = parse_list(&xml).map_err(|why| {
                StoreError::Io(format!("list `{stem}`: malformed listing: {why}"))
            })?;
            for mut o in page.objects {
                // Paths are relative to the store's prefix, as the other stores give them. A key
                // the other operations would refuse (one written by something else, or a
                // directory marker) is left out, as BlobStore leaves it out.
                if let Some(rel) = o.path.strip_prefix(&self.prefix) {
                    if validate_path(rel).is_ok() {
                        o.path = rel.to_owned();
                        out.push(o);
                    }
                }
            }
            match page.next {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }
}

#[async_trait]
impl ObjectStore for R2Store {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        retrying("get", path, self.retry, retryable, |_| {
            self.get_once(path, None, MAX_OBJECT_BYTES)
        })
        .await
    }

    /// Overwrites, so another try after a lost answer leaves the same object.
    async fn put(&self, path: &str, body: Vec<u8>, content_type: &str) -> Result<(), StoreError> {
        let key = self.object_key(path)?;
        let body = Bytes::from(body);
        retrying("put", path, self.retry, retryable, |_| {
            let body = body.clone();
            let key = key.clone();
            async move {
                self.put_once(&key, path, body, content_type, false)
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    /// One try only: a retry of a create that went through would read as
    /// "already exists".
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, StoreError> {
        let key = self.object_key(path)?;
        self.put_once(&key, path, Bytes::from(body), content_type, true)
            .await
    }

    async fn exists(&self, path: &str) -> Result<bool, StoreError> {
        Ok(self.head(path).await?.is_some())
    }

    async fn modified(&self, path: &str) -> Result<Option<SystemTime>, StoreError> {
        Ok(self.head(path).await?.and_then(|h| {
            h.get("last-modified")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| httpdate::parse_http_date(v).ok())
        }))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let stem = if prefix.is_empty() {
            self.prefix.clone()
        } else {
            validate_path(prefix)?;
            format!("{}{prefix}/", self.prefix)
        };
        Ok(self
            .list_objects(&stem)
            .await?
            .into_iter()
            .map(|o| o.path)
            .collect())
    }

    async fn list_stem(&self, stem: &str) -> Result<Vec<String>, StoreError> {
        validate_path(stem)?;
        let stem = format!("{}{stem}", self.prefix);
        Ok(self
            .list_objects(&stem)
            .await?
            .into_iter()
            .map(|o| o.path)
            .collect())
    }

    async fn list_info(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let stem = if prefix.is_empty() {
            self.prefix.clone()
        } else {
            validate_path(prefix)?;
            format!("{}{prefix}/", self.prefix)
        };
        self.list_objects(&stem).await
    }

    async fn get_if_match(
        &self,
        path: &str,
        etag: &str,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let limit = max_bytes.min(MAX_OBJECT_BYTES);
        retrying("get", path, self.retry, retryable, |_| {
            self.get_once(path, Some(etag), limit)
        })
        .await
    }
    /// Only the request is tried again; the body is read as it arrives, however large.
    async fn get_stream(&self, path: &str) -> Result<Option<ByteStream>, StoreError> {
        use futures::StreamExt;
        let key = self.object_key(path)?;
        let resp = retrying("get", path, self.retry, retryable, |_| async {
            let resp = self
                .send(Method::GET, &key, &[], &[], Bytes::new(), "get")
                .await?;
            match resp.status() {
                StatusCode::OK => Ok(Some(resp)),
                StatusCode::NOT_FOUND => Ok(None),
                s => Err(http("get", path, s)),
            }
        })
        .await?;
        let Some(resp) = resp else {
            return Ok(None);
        };
        let p = path.to_owned();
        let s: ByteStream = Box::pin(resp.bytes_stream().map(move |c| {
            c.map_err(|e| StoreError::Io(format!("get `{p}`: {}", e.without_url())))
        }));
        Ok(Some(s))
    }

    async fn delete_if_match(&self, path: &str, etag: &str) -> Result<bool, StoreError> {
        let unquote = |e: &str| e.trim_matches('"').to_owned();
        let current = self
            .head(path)
            .await?
            .and_then(|h| h.get("etag").and_then(|v| v.to_str().ok()).map(unquote));
        if current.as_deref() != Some(unquote(etag).as_str()) {
            return Ok(false);
        }
        let key = self.object_key(path)?;
        let extra = [("if-match", quoted(etag))];
        retrying("delete", path, self.retry, retryable, |_| async {
            let resp = self
                .send(Method::DELETE, &key, &[], &extra, Bytes::new(), "delete")
                .await?;
            match resp.status() {
                StatusCode::OK | StatusCode::NO_CONTENT => Ok(true),
                // Gone since the HEAD (or a lost answer to an earlier try), or written again.
                StatusCode::NOT_FOUND | StatusCode::PRECONDITION_FAILED => Ok(false),
                s => Err(http("delete", path, s)),
            }
        })
        .await
    }
}

fn http(op: &'static str, path: &str, status: StatusCode) -> StoreError {
    StoreError::Http {
        op,
        path: path.into(),
        status: status.as_u16(),
    }
}

fn quoted(etag: &str) -> String {
    if etag.starts_with('"') {
        etag.to_owned()
    } else {
        format!("\"{etag}\"")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC 3986 unreserved characters pass; everything else is `%XX`. `/` too
/// unless `keep_slash`.
fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && b == b'/')
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn canonical_uri(bucket: &str, key: &str) -> String {
    if key.is_empty() {
        format!("/{bucket}")
    } else {
        format!("/{bucket}/{}", uri_encode(key, true))
    }
}

fn canonical_query(query: &[(&str, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// HMAC-SHA-256 (RFC 2104), so no extra crate is needed for it.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// `YYYYMMDDTHHMMSSZ` in UTC.
fn amz_date(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

struct SignInput<'a> {
    method: &'a str,
    /// Already encoded.
    uri: &'a str,
    query: &'a [(&'a str, String)],
    host: &'a str,
    /// Lowercase header names.
    extra: &'a [(&'a str, String)],
    payload_sha256: &'a str,
    time: SystemTime,
    region: &'a str,
    service: &'a str,
    key: &'a R2Key,
}

struct Signed {
    amz_date: String,
    payload_sha256: String,
    authorization: String,
}

/// AWS Signature Version 4 over `host`, `x-amz-content-sha256`,
/// `x-amz-date` and the extra headers.
fn sign(i: &SignInput<'_>) -> Signed {
    let amz_date = amz_date(i.time);
    let day = &amz_date[..8];
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), i.host.to_owned()),
        ("x-amz-content-sha256".into(), i.payload_sha256.to_owned()),
        ("x-amz-date".into(), amz_date.clone()),
    ];
    for (n, v) in i.extra {
        headers.push((n.to_ascii_lowercase(), v.trim().to_owned()));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}",
        i.method,
        i.uri,
        canonical_query(i.query),
        i.payload_sha256
    );
    let scope = format!("{day}/{}/{}/aws4_request", i.region, i.service);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );
    let k_date = hmac_sha256(format!("AWS4{}", i.key.secret).as_bytes(), day.as_bytes());
    let k_region = hmac_sha256(&k_date, i.region.as_bytes());
    let k_service = hmac_sha256(&k_region, i.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex(&hmac_sha256(&k_signing, to_sign.as_bytes()));
    Signed {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            i.key.id
        ),
        amz_date,
        payload_sha256: i.payload_sha256.to_owned(),
    }
}

struct ListPage {
    objects: Vec<ObjectInfo>,
    next: Option<String>,
}

fn xml_text(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find(&close)?;
    Some(&xml[start..start + len])
}

/// One ListObjectsV2 page. A page that is cut short, an entry without a key or a size, or a
/// truncated page without a continuation token is an error: a listing must never look complete
/// when it is not. `next` is set only when there is another page.
fn parse_list(xml: &str) -> Result<ListPage, String> {
    if !xml.contains("</ListBucketResult>") {
        return Err("no closing ListBucketResult".into());
    }
    let mut objects = Vec::new();
    for part in xml.split("<Contents>").skip(1) {
        let Some((entry, _)) = part.split_once("</Contents>") else {
            return Err("an unclosed Contents".into());
        };
        let path = element(entry, "Key")
            .map(xml_text)
            .ok_or("an entry without a Key")?;
        let size = element(entry, "Size")
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| format!("{path}: no valid Size"))?;
        let modified = element(entry, "LastModified").and_then(parse_iso8601);
        objects.push(ObjectInfo {
            path,
            size,
            etag: element(entry, "ETag").map(xml_text).unwrap_or_default(),
            created: modified,
            modified,
            content_md5: None,
        });
    }
    let truncated = match element(xml, "IsTruncated").map(str::trim) {
        Some("true") => true,
        Some("false") => false,
        _ => return Err("no valid IsTruncated".into()),
    };
    let next = if truncated {
        let t = element(xml, "NextContinuationToken")
            .map(xml_text)
            .filter(|t| !t.is_empty())
            .ok_or("a truncated page without a NextContinuationToken")?;
        Some(t)
    } else {
        None
    };
    Ok(ListPage { objects, next })
}

/// `2026-11-20T14:02:01.000Z` (the listing's `LastModified`).
fn parse_iso8601(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    u64::try_from(secs)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::body::Bytes as AxBytes;
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, StatusCode as AxStatus, Uri};
    use axum::response::IntoResponse;
    use axum::routing::any;
    use axum::Router;

    fn key() -> R2Key {
        R2Key {
            id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        }
    }

    #[test]
    fn the_signature_matches_the_aws_get_object_example() {
        // "Example: GET Object" in the AWS Signature Version 4 documentation
        // for Amazon S3: GET /test.txt with a Range header, 24 May 2013.
        let t = UNIX_EPOCH + Duration::from_secs(1_369_353_600);
        let empty = hex(&Sha256::digest(b""));
        let s = sign(&SignInput {
            method: "GET",
            uri: "/test.txt",
            query: &[],
            host: "examplebucket.s3.amazonaws.com",
            extra: &[("range", "bytes=0-9".into())],
            payload_sha256: &empty,
            time: t,
            region: "us-east-1",
            service: "s3",
            key: &key(),
        });
        assert_eq!(s.amz_date, "20130524T000000Z");
        assert!(s.authorization.ends_with(
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        ));
        assert!(s.authorization.contains(
            "Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,"
        ));
    }

    #[test]
    fn the_signature_matches_the_aws_list_objects_example() {
        // "Example: GET Bucket (List Objects)": GET /?max-keys=2&prefix=J.
        let t = UNIX_EPOCH + Duration::from_secs(1_369_353_600);
        let empty = hex(&Sha256::digest(b""));
        let s = sign(&SignInput {
            method: "GET",
            uri: "/",
            query: &[("max-keys", "2".into()), ("prefix", "J".into())],
            host: "examplebucket.s3.amazonaws.com",
            extra: &[],
            payload_sha256: &empty,
            time: t,
            region: "us-east-1",
            service: "s3",
            key: &key(),
        });
        assert!(s.authorization.ends_with(
            "Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        ));
    }

    #[test]
    fn dates_round_trip() {
        for secs in [
            0u64,
            951_782_400,
            1_369_353_600,
            1_795_183_035,
            4_102_444_799,
        ] {
            let t = UNIX_EPOCH + Duration::from_secs(secs);
            let a = amz_date(t);
            let iso = format!(
                "{}-{}-{}T{}:{}:{}.000Z",
                &a[0..4],
                &a[4..6],
                &a[6..8],
                &a[9..11],
                &a[11..13],
                &a[13..15]
            );
            assert_eq!(parse_iso8601(&iso), Some(t), "{a}");
        }
    }

    #[test]
    fn locations_are_checked() {
        let k = key();
        assert!(R2Store::new(
            "r2://usnm-results/validator",
            "https://acct.r2.cloudflarestorage.com",
            k.clone()
        )
        .is_ok());
        assert!(R2Store::new(
            "s3://usnm-results",
            "https://acct.r2.cloudflarestorage.com",
            k.clone()
        )
        .is_err());
        assert!(R2Store::new(
            "r2://Bad_Bucket",
            "https://acct.r2.cloudflarestorage.com",
            k.clone()
        )
        .is_err());
        assert!(R2Store::new(
            "r2://usnm-results/../x",
            "https://acct.r2.cloudflarestorage.com",
            k.clone()
        )
        .is_err());
        assert!(R2Store::new(
            "r2://usnm-results",
            "http://acct.r2.cloudflarestorage.com",
            k.clone()
        )
        .is_err());
        assert!(R2Store::new(
            "r2://usnm-results",
            "https://acct.r2.cloudflarestorage.com/x",
            k.clone()
        )
        .is_err());
        assert!(R2Store::new("r2://usnm-results", "http://127.0.0.1:9", k).is_ok());
        let s = R2Store::new(
            "r2://usnm-results/p",
            "https://acct.r2.cloudflarestorage.com",
            key(),
        )
        .unwrap();
        assert!(!format!("{s:?}").contains("wJalr"));
    }

    #[test]
    fn keys_and_queries_are_encoded_as_sigv4_wants() {
        assert_eq!(canonical_uri("b", "a b/c+d~e"), "/b/a%20b/c%2Bd~e");
        assert_eq!(
            canonical_query(&[("prefix", "x/y z".into()), ("list-type", "2".into())]),
            "list-type=2&prefix=x%2Fy%20z"
        );
    }

    /// An in-memory S3 subset: PUT (with `If-None-Match: *`), GET (with
    /// `If-Match`), HEAD and ListObjectsV2 with pages of two. Every request
    /// must carry a SigV4 authorization for the test key.
    #[derive(Default)]
    struct Fake {
        objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
        unsigned: Mutex<usize>,
        busy: Mutex<usize>,
        /// GET bodies are streamed without a Content-Length.
        no_length: Mutex<bool>,
    }

    async fn handle(
        State(f): State<Arc<Fake>>,
        method: axum::http::Method,
        uri: Uri,
        Query(q): Query<HashMap<String, String>>,
        h: HeaderMap,
        body: AxBytes,
    ) -> axum::response::Response {
        let auth = h
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/")
            || h.get("x-amz-date").is_none()
        {
            *f.unsigned.lock().unwrap() += 1;
            return AxStatus::FORBIDDEN.into_response();
        }
        {
            let mut busy = f.busy.lock().unwrap();
            if *busy > 0 {
                *busy -= 1;
                return AxStatus::SERVICE_UNAVAILABLE.into_response();
            }
        }
        let path = uri
            .path()
            .trim_start_matches("/bkt")
            .trim_start_matches('/')
            .to_owned();
        let mut objects = f.objects.lock().unwrap();
        match method.as_str() {
            "PUT" => {
                if h.get("if-none-match").is_some() && objects.contains_key(&path) {
                    return AxStatus::PRECONDITION_FAILED.into_response();
                }
                let etag = format!("\"{}\"", hex(&Sha256::digest(&body))[..32].to_owned());
                objects.insert(path, (body.to_vec(), etag));
                AxStatus::OK.into_response()
            }
            "GET" if path.is_empty() => {
                let prefix = q.get("prefix").cloned().unwrap_or_default();
                let mut keys: Vec<&String> =
                    objects.keys().filter(|k| k.starts_with(&prefix)).collect();
                keys.sort();
                let start = q
                    .get("continuation-token")
                    .map_or(0, |t| t.parse().unwrap());
                let page: Vec<&&String> = keys.iter().skip(start).take(2).collect();
                let more = start + 2 < keys.len();
                let mut xml = String::from("<ListBucketResult>");
                for k in page {
                    let (b, e) = &objects[*k];
                    xml.push_str(&format!("<Contents><Key>{}</Key><LastModified>2026-11-20T14:02:01.000Z</LastModified><ETag>{}</ETag><Size>{}</Size></Contents>", k.replace('&', "&amp;"), e.replace('"', "&quot;"), b.len()));
                }
                xml.push_str(&format!("<IsTruncated>{more}</IsTruncated>"));
                if more {
                    xml.push_str(&format!(
                        "<NextContinuationToken>{}</NextContinuationToken>",
                        start + 2
                    ));
                }
                xml.push_str("</ListBucketResult>");
                (AxStatus::OK, xml).into_response()
            }
            "GET" | "HEAD" => match objects.get(&path) {
                None => AxStatus::NOT_FOUND.into_response(),
                Some((_, e)) if h.get("if-match").is_some_and(|m| m.to_str().unwrap() != e) => {
                    AxStatus::PRECONDITION_FAILED.into_response()
                }
                Some((b, _)) if *f.no_length.lock().unwrap() => {
                    let chunks: Vec<Result<AxBytes, std::io::Error>> = b
                        .chunks(4)
                        .map(|c| Ok(AxBytes::copy_from_slice(c)))
                        .collect();
                    axum::body::Body::from_stream(futures::stream::iter(chunks)).into_response()
                }
                Some((b, e)) => (
                    AxStatus::OK,
                    [
                        ("last-modified", "Fri, 20 Nov 2026 14:02:01 GMT"),
                        ("etag", e.as_str()),
                    ],
                    b.clone(),
                )
                    .into_response(),
            },
            "DELETE" => match objects.get(&path) {
                None => AxStatus::NOT_FOUND.into_response(),
                Some((_, e)) if h.get("if-match").is_some_and(|m| m.to_str().unwrap() != e) => {
                    AxStatus::PRECONDITION_FAILED.into_response()
                }
                Some(_) => {
                    objects.remove(&path);
                    AxStatus::NO_CONTENT.into_response()
                }
            },
            _ => AxStatus::METHOD_NOT_ALLOWED.into_response(),
        }
    }

    async fn fake() -> (Arc<Fake>, String) {
        let f = Arc::new(Fake::default());
        let app = Router::new().fallback(any(handle)).with_state(f.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (f, format!("http://{addr}"))
    }

    const FAST: Retry = Retry {
        tries: 3,
        first_wait: Duration::from_millis(1),
        max_wait: Duration::from_millis(2),
    };

    #[tokio::test]
    async fn the_store_reads_writes_creates_once_and_lists_every_page() {
        let (f, endpoint) = fake().await;
        let s = R2Store::new("r2://bkt/val", &endpoint, key())
            .unwrap()
            .with_retry(FAST);
        assert_eq!(s.get("calls/a.json").await.unwrap(), None);
        assert!(s
            .put_new("calls/a.json", b"one".to_vec(), "application/json")
            .await
            .unwrap());
        assert!(!s
            .put_new("calls/a.json", b"two".to_vec(), "application/json")
            .await
            .unwrap());
        assert_eq!(
            s.get("calls/a.json").await.unwrap().as_deref(),
            Some(&b"one"[..])
        );
        s.put("calls/a.json", b"three".to_vec(), "application/json")
            .await
            .unwrap();
        assert_eq!(
            s.get("calls/a.json").await.unwrap().as_deref(),
            Some(&b"three"[..])
        );
        for n in ["b", "c", "d", "e"] {
            s.put(
                &format!("calls/{n}.json"),
                n.as_bytes().to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        }
        s.put("other/x", b"x".to_vec(), "text/plain").await.unwrap();
        assert_eq!(
            s.list("calls").await.unwrap(),
            [
                "calls/a.json",
                "calls/b.json",
                "calls/c.json",
                "calls/d.json",
                "calls/e.json"
            ]
        );
        assert_eq!(s.list_stem("calls/c").await.unwrap(), ["calls/c.json"]);
        let info = s.list_info("other").await.unwrap();
        assert_eq!((info[0].path.as_str(), info[0].size), ("other/x", 1));
        assert!(s.exists("other/x").await.unwrap());
        assert!(!s.exists("other/y").await.unwrap());
        assert!(s.modified("other/x").await.unwrap().is_some());
        // Stored under the location's prefix.
        assert!(f.objects.lock().unwrap().contains_key("val/other/x"));
        assert_eq!(*f.unsigned.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_conditional_read_sees_only_the_listed_version() {
        let (_f, endpoint) = fake().await;
        let s = R2Store::new("r2://bkt", &endpoint, key())
            .unwrap()
            .with_retry(FAST);
        s.put("a", b"1".to_vec(), "text/plain").await.unwrap();
        let etag = s.list_info("").await.unwrap()[0].etag.clone();
        assert_eq!(
            s.get_if_match("a", &etag, 10).await.unwrap().as_deref(),
            Some(&b"1"[..])
        );
        s.put("a", b"2".to_vec(), "text/plain").await.unwrap();
        assert_eq!(s.get_if_match("a", &etag, 10).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_conditional_delete_takes_only_the_listed_version() {
        let (f, endpoint) = fake().await;
        let s = R2Store::new("r2://bkt", &endpoint, key())
            .unwrap()
            .with_retry(FAST);
        s.put("a", b"1".to_vec(), "text/plain").await.unwrap();
        let old = s.list_info("").await.unwrap()[0].etag.clone();
        s.put("a", b"2".to_vec(), "text/plain").await.unwrap();
        assert!(!s.delete_if_match("a", &old).await.unwrap());
        assert!(f.objects.lock().unwrap().contains_key("a"));
        let now = s.list_info("").await.unwrap()[0].etag.clone();
        assert!(s.delete_if_match("a", &now).await.unwrap());
        assert!(!f.objects.lock().unwrap().contains_key("a"));
        assert!(!s.delete_if_match("a", &now).await.unwrap());
    }

    #[tokio::test]
    async fn a_stream_reads_past_the_whole_object_limit_in_chunks() {
        use futures::StreamExt;
        let (f, endpoint) = fake().await;
        *f.no_length.lock().unwrap() = true;
        let s = R2Store::new("r2://bkt", &endpoint, key())
            .unwrap()
            .with_retry(FAST);
        assert!(s.get_stream("missing").await.unwrap().is_none());
        s.put("big", b"0123456789".to_vec(), "text/plain")
            .await
            .unwrap();
        let mut got = Vec::new();
        let mut chunks = 0;
        let mut st = s.get_stream("big").await.unwrap().unwrap();
        while let Some(c) = st.next().await {
            got.extend_from_slice(&c.unwrap());
            chunks += 1;
        }
        assert_eq!(got, b"0123456789");
        assert!(chunks > 1);
    }

    #[tokio::test]
    async fn a_busy_service_is_tried_again() {
        let (f, endpoint) = fake().await;
        let s = R2Store::new("r2://bkt", &endpoint, key())
            .unwrap()
            .with_retry(FAST);
        *f.busy.lock().unwrap() = 2;
        s.put("a", b"1".to_vec(), "text/plain").await.unwrap();
        *f.busy.lock().unwrap() = 2;
        assert_eq!(s.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
        *f.busy.lock().unwrap() = 5;
        assert!(matches!(
            s.get("a").await,
            Err(StoreError::Http { status: 503, .. })
        ));
    }

    #[test]
    fn listing_entities_are_decoded() {
        let p = parse_list("<ListBucketResult><Contents><Key>a&amp;b</Key><ETag>&quot;e&quot;</ETag><Size>3</Size></Contents><IsTruncated>false</IsTruncated></ListBucketResult>").unwrap();
        assert_eq!(
            (
                p.objects[0].path.as_str(),
                p.objects[0].etag.as_str(),
                p.next
            ),
            ("a&b", "\"e\"", None)
        );
    }

    #[test]
    fn a_malformed_listing_is_an_error() {
        let ok = "<Contents><Key>a</Key><Size>1</Size></Contents>";
        for xml in [
            // Cut short.
            format!("<ListBucketResult>{ok}"),
            format!("<ListBucketResult>{ok}<Contents><Key>b</Key>"),
            // An entry without a size, a bad size or no key.
            "<ListBucketResult><Contents><Key>a</Key></Contents><IsTruncated>false</IsTruncated></ListBucketResult>".to_owned(),
            "<ListBucketResult><Contents><Key>a</Key><Size>x</Size></Contents><IsTruncated>false</IsTruncated></ListBucketResult>".to_owned(),
            "<ListBucketResult><Contents><Size>1</Size></Contents><IsTruncated>false</IsTruncated></ListBucketResult>".to_owned(),
            // No IsTruncated, or truncated without a token.
            format!("<ListBucketResult>{ok}</ListBucketResult>"),
            format!("<ListBucketResult>{ok}<IsTruncated>true</IsTruncated></ListBucketResult>"),
            format!("<ListBucketResult>{ok}<IsTruncated>true</IsTruncated><NextContinuationToken></NextContinuationToken></ListBucketResult>"),
        ] {
            assert!(parse_list(&xml).is_err(), "{xml}");
        }
        let last = parse_list(&format!(
            "<ListBucketResult>{ok}<IsTruncated>true</IsTruncated><NextContinuationToken>t</NextContinuationToken></ListBucketResult>"
        ))
        .unwrap();
        assert_eq!(last.next.as_deref(), Some("t"));
    }

    #[tokio::test]
    async fn keys_that_are_not_store_paths_are_left_out_of_listings() {
        let (f, endpoint) = fake().await;
        let s = R2Store::new("r2://bkt/p", &endpoint, key())
            .unwrap()
            .with_retry(FAST);
        s.put("calls/a", b"1".to_vec(), "text/plain").await.unwrap();
        {
            let mut o = f.objects.lock().unwrap();
            for k in ["p/calls/", "p/calls/a b", "p/calls/../x", "p//calls/b"] {
                o.insert(k.to_owned(), (b"x".to_vec(), "\"e\"".to_owned()));
            }
        }
        assert_eq!(s.list("calls").await.unwrap(), vec!["calls/a".to_owned()]);
        let all: Vec<String> = s
            .list_info("")
            .await
            .unwrap()
            .into_iter()
            .map(|o| o.path)
            .collect();
        assert_eq!(all, vec!["calls/a".to_owned()]);
    }

    #[tokio::test]
    async fn reads_stop_at_the_limit_with_or_without_a_length() {
        let (f, endpoint) = fake().await;
        let s = R2Store::new("r2://bkt", &endpoint, key())
            .unwrap()
            .with_retry(FAST);
        s.put("a", vec![7; 20], "application/octet-stream")
            .await
            .unwrap();
        let etag = s.list_info("").await.unwrap()[0].etag.clone();
        for streamed in [false, true] {
            *f.no_length.lock().unwrap() = streamed;
            assert!(
                matches!(
                    s.get_if_match("a", &etag, 19).await,
                    Err(StoreError::TooLarge(_))
                ),
                "streamed {streamed}"
            );
            assert_eq!(
                s.get_if_match("a", &etag, 20).await.unwrap(),
                Some(vec![7; 20])
            );
            assert_eq!(s.get("a").await.unwrap(), Some(vec![7; 20]));
        }
    }
}
