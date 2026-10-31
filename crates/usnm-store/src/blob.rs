//! Azure Blob Storage over its REST API with Entra ID bearer tokens.
//!
//! Only a few operations are needed (Get Blob, Get Blob Properties, Put Blob,
//! and List Blobs for the search log), so this talks to the REST API
//! directly with the workspace's `reqwest` client rather than pulling in the
//! Azure SDK.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, IF_MATCH, IF_NONE_MATCH};
use reqwest::{Method, StatusCode, Url};

use crate::credential::Credential;
use crate::retry::{retryable, retrying, Retry};
use crate::{validate_path, ByteStream, ObjectInfo, ObjectStore, StoreError, MAX_OBJECT_BYTES};

mod upload;

#[cfg(test)]
use upload::block_id;

/// Bytes per block of a streamed upload (`put_stream`): 50,000 blocks of
/// 8 MiB allow 390 GiB, and the largest batch archive is 3.8 GB.
pub const BLOCK_BYTES: usize = 8 * 1024 * 1024;

/// How long one streamed read may take: a whole archive is read in one
/// response.
const STREAM_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// One Put Block may take [`BLOCK_TIMEOUT_FLOOR`] plus its bytes at
/// [`BLOCK_MIN_RATE`]: 2 min 38 s for an 8 MiB block. A connection that
/// hangs fails within that and the block is sent again, where one timeout
/// for every request would have to allow for a whole archive.
const BLOCK_TIMEOUT_FLOOR: Duration = Duration::from_secs(30);
/// Bytes per second: far below any uplink a large upload could finish on.
const BLOCK_MIN_RATE: u64 = 64 * 1024;

/// Blob service REST API version (bearer tokens need 2017-11-09 or later).
pub const API_VERSION: &str = "2023-11-03";

/// Blob endpoint host suffixes (public, US Government and China clouds).
/// Private endpoints keep these names, resolved through private DNS. The
/// managed identity's token is only ever sent to one of these hosts.
pub const BLOB_HOST_SUFFIXES: [&str; 3] = [
    ".blob.core.windows.net",
    ".blob.core.usgovcloudapi.net",
    ".blob.core.chinacloudapi.cn",
];

pub struct BlobStore {
    /// `https://{account}.blob.core.windows.net/{container}[/prefix]`, no trailing `/`.
    base: String,
    credential: Arc<Credential>,
    http: reqwest::Client,
    tuning: Tuning,
    /// Commits whose outcome is unknown (their last try failed in a way
    /// that may pass, so it may have gone through), by path: the block ids
    /// each sent. A later upload of the same ids to the path that finds the
    /// blob there takes it as that commit.
    unsure: std::sync::Mutex<HashMap<String, Vec<String>>>,
}

/// Tries, timeouts and the block size; the tests shorten them.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    /// A whole-object GET ([`ObjectStore::get`]).
    get: Retry,
    /// Each request of a streamed upload: Get Block List, Put Block, Put
    /// Block List.
    write: Retry,
    block_bytes: usize,
    block_timeout_floor: Duration,
    block_min_rate: u64,
}

impl Tuning {
    const STANDARD: Tuning = Tuning {
        get: Retry::STANDARD,
        write: Retry::STANDARD,
        block_bytes: BLOCK_BYTES,
        block_timeout_floor: BLOCK_TIMEOUT_FLOOR,
        block_min_rate: BLOCK_MIN_RATE,
    };

    /// How long a Put Block of `bytes` may take.
    fn block_timeout(&self, bytes: usize) -> Duration {
        self.block_timeout_floor
            + Duration::from_secs_f64(bytes as f64 / self.block_min_rate.max(1) as f64)
    }
}

impl std::fmt::Debug for BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobStore")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl BlobStore {
    /// `location` is a container URL, optionally with a path prefix, on an
    /// Azure Blob host ([`BLOB_HOST_SUFFIXES`]). SAS tokens are refused:
    /// access is by Entra ID only. Plain `http` is allowed for loopback test
    /// servers, never for a real account.
    pub fn new(location: &str, credential: Arc<Credential>) -> Result<Self, StoreError> {
        let bad = |why: &str| StoreError::InvalidLocation(format!("{location}: {why}"));
        let url = Url::parse(location).map_err(|e| bad(&e.to_string()))?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        let blob_host = url.host_str().is_some_and(|h| {
            let h = h.to_ascii_lowercase();
            BLOB_HOST_SUFFIXES
                .iter()
                .any(|s| h.len() > s.len() && h.ends_with(s))
        });
        match url.scheme() {
            "https" if blob_host || loopback => {}
            "https" => {
                return Err(bad(
                    "must be an Azure Blob endpoint (*.blob.core.windows.net)",
                ))
            }
            "http" if loopback => {}
            _ => return Err(bad("must be https")),
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(bad("must not carry a query (SAS tokens are not used)"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad("must not carry credentials"));
        }
        let path = url.path().trim_matches('/');
        if path.is_empty() {
            return Err(bad("must name a container"));
        }
        validate_path(path).map_err(|_| bad("container and prefix must be plain path segments"))?;
        let base = format!("{}/{path}", url.origin().ascii_serialization());
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            // Never carry the bearer token anywhere but the configured host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(Self {
            base,
            credential,
            http,
            tuning: Tuning::STANDARD,
            unsure: std::sync::Mutex::default(),
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, StoreError> {
        validate_path(path)?;
        let token = self.credential.token().await?;
        Ok(self
            .http
            .request(method, format!("{}/{path}", self.base))
            .bearer_auth(token)
            .header("x-ms-version", API_VERSION)
            .header("x-ms-date", httpdate::fmt_http_date(SystemTime::now())))
    }
}

/// The text of each `<tag>…</tag>` element, with XML's five entities decoded.
/// Each `<tag>…</tag>` element's raw content, entities left as they are:
/// for an element whose children are read with [`xml_values`], so text is
/// decoded once and a name holding `&lt;/Name&gt;` stays text.
fn xml_elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.split(open.as_str())
        .skip(1)
        .filter_map(|rest| rest.split_once(close.as_str()).map(|(v, _)| v))
        .collect()
}

fn xml_values(xml: &str, tag: &str) -> std::vec::IntoIter<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.split(open.as_str())
        .skip(1)
        .filter_map(|rest| {
            rest.split_once(close.as_str()).map(|(v, _)| {
                v.replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&amp;", "&")
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
}

fn error_code(resp: &reqwest::Response) -> &str {
    resp.headers()
        .get("x-ms-error-code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// One `<Blob>` of a List Blobs response. `None` for an entry without a name
/// or size.
fn blob_info(blob_xml: &str) -> Option<ObjectInfo> {
    let one = |tag: &str| xml_values(blob_xml, tag).next();
    let path = one("Name")?;
    let size = one("Content-Length")?.trim().parse().ok()?;
    let etag = one("Etag").unwrap_or_default();
    let created = one("Creation-Time").and_then(|t| httpdate::parse_http_date(t.trim()).ok());
    let content_md5 = one("Content-MD5").filter(|md5| !md5.trim().is_empty());
    Some(ObjectInfo {
        path,
        size,
        etag,
        created,
        content_md5,
    })
}

/// An ETag as an `If-Match` value: quoted, as the service sends it in headers
/// (List Blobs gives it bare).
fn quoted(etag: &str) -> String {
    if etag.starts_with('"') {
        etag.to_owned()
    } else {
        format!("\"{etag}\"")
    }
}

fn transport(op: &'static str, path: &str, e: reqwest::Error) -> StoreError {
    // Strip the URL: the error's own text is enough and paths are logged
    // separately. reqwest's text is only its kind ("error sending
    // request"), so the causes follow: a reset, a timeout, a body cut short.
    let e = e.without_url();
    let mut text = if e.is_timeout() {
        format!("{e} (timed out)")
    } else {
        e.to_string()
    };
    let mut cause = std::error::Error::source(&e);
    while let Some(c) = cause {
        text.push_str(": ");
        text.push_str(&c.to_string());
        cause = c.source();
    }
    StoreError::Io(format!("{op} `{path}`: {text}"))
}

impl BlobStore {
    /// [`ObjectStore::get`], tried as `retry` says.
    async fn get_retrying(&self, path: &str, retry: Retry) -> Result<Option<Vec<u8>>, StoreError> {
        retrying("get", path, retry, retryable, |_| self.get_once(path)).await
    }

    /// One try of [`ObjectStore::get`].
    async fn get_once(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let resp = self
            .request(Method::GET, path)
            .await?
            .send()
            .await
            .map_err(|e| transport("get", path, e))?;
        match resp.status() {
            StatusCode::NOT_FOUND => return Ok(None),
            s if !s.is_success() => {
                return Err(StoreError::Http {
                    op: "get",
                    path: path.into(),
                    status: s.as_u16(),
                })
            }
            _ => {}
        }
        if resp.content_length().is_some_and(|n| n > MAX_OBJECT_BYTES) {
            return Err(StoreError::TooLarge(path.into()));
        }
        // Stream, so a response without an honest Content-Length still stops
        // at the limit instead of being buffered whole.
        let mut resp = resp;
        let mut body = Vec::with_capacity(
            resp.content_length()
                .map_or(0, |n| usize::try_from(n).unwrap_or(0)),
        );
        while let Some(chunk) = resp.chunk().await.map_err(|e| transport("get", path, e))? {
            if (body.len() + chunk.len()) as u64 > MAX_OBJECT_BYTES {
                return Err(StoreError::TooLarge(path.into()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }
}

#[async_trait]
impl ObjectStore for BlobStore {
    async fn get_stream(&self, path: &str) -> Result<Option<ByteStream>, StoreError> {
        use futures::StreamExt;
        let resp = self
            .request(Method::GET, path)
            .await?
            .timeout(STREAM_TIMEOUT)
            .send()
            .await
            .map_err(|e| transport("get", path, e))?;
        match resp.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if !s.is_success() => Err(StoreError::Http {
                op: "get",
                path: path.into(),
                status: s.as_u16(),
            }),
            _ => {
                let path = path.to_owned();
                let s: ByteStream = Box::pin(
                    resp.bytes_stream()
                        .map(move |c| c.map_err(|e| transport("get", &path, e))),
                );
                Ok(Some(s))
            }
        }
    }

    /// Put Block for every [`BLOCK_BYTES`], then Put Block List with the
    /// tier and `If-None-Match: *`: blocks stay invisible (and are discarded
    /// within a week) unless the list is committed, and the commit fails if
    /// a blob is already there, leaving that blob as it was.
    ///
    /// Each request is sent again after a failure that may pass
    /// ([`Retry::STANDARD`]), and a Put Block's timeout is sized to the
    /// block. An upload to a path that an interrupted one left blocks at
    /// (in this process or another), or that one committed with its answer
    /// lost, sends only the blocks Azure doesn't hold already: their ids
    /// name their bytes ([`upload::block_id`]).
    async fn put_stream(
        &self,
        path: &str,
        content_type: &str,
        tier: Option<&str>,
        chunks: tokio::sync::mpsc::Receiver<bytes::Bytes>,
        commit: tokio::sync::oneshot::Receiver<bool>,
    ) -> Result<u64, StoreError> {
        self.upload_stream(path, content_type, tier, chunks, commit)
            .await
    }

    /// A whole object, read again when a try fails on the way (a dropped
    /// connection or a body cut short, a timeout, 408, 429 or a 5xx): GET
    /// changes nothing, so another try is always safe. [`Retry::STANDARD`].
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.get_retrying(path, self.tuning.get).await
    }

    async fn put(&self, path: &str, body: Vec<u8>, content_type: &str) -> Result<(), StoreError> {
        let resp = self
            .request(Method::PUT, path)
            .await?
            .header("x-ms-blob-type", "BlockBlob")
            .header(CONTENT_TYPE, content_type)
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .send()
            .await
            .map_err(|e| transport("put", path, e))?;
        match resp.status() {
            StatusCode::CREATED => Ok(()),
            s => Err(StoreError::Http {
                op: "put",
                path: path.into(),
                status: s.as_u16(),
            }),
        }
    }

    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, StoreError> {
        let resp = self
            .request(Method::PUT, path)
            .await?
            .header("x-ms-blob-type", "BlockBlob")
            .header(IF_NONE_MATCH, "*")
            .header(CONTENT_TYPE, content_type)
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .send()
            .await
            .map_err(|e| transport("put", path, e))?;
        match (resp.status(), error_code(&resp)) {
            (StatusCode::CREATED, _) => Ok(true),
            // Only "the blob already exists" is a normal create-only miss;
            // lease, immutability and blob-type conflicts are errors.
            (StatusCode::CONFLICT, "BlobAlreadyExists")
            | (StatusCode::PRECONDITION_FAILED, "ConditionNotMet") => Ok(false),
            (s, _) => Err(StoreError::Http {
                op: "put",
                path: path.into(),
                status: s.as_u16(),
            }),
        }
    }

    async fn exists(&self, path: &str) -> Result<bool, StoreError> {
        let resp = self
            .request(Method::HEAD, path)
            .await?
            .send()
            .await
            .map_err(|e| transport("head", path, e))?;
        match resp.status() {
            s if s.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            s => Err(StoreError::Http {
                op: "head",
                path: path.into(),
                status: s.as_u16(),
            }),
        }
    }

    async fn modified(&self, path: &str) -> Result<Option<SystemTime>, StoreError> {
        let resp = self
            .request(Method::HEAD, path)
            .await?
            .send()
            .await
            .map_err(|e| transport("head", path, e))?;
        match resp.status() {
            s if s.is_success() => Ok(resp
                .headers()
                .get(reqwest::header::LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| httpdate::parse_http_date(v).ok())),
            StatusCode::NOT_FOUND => Ok(None),
            s => Err(StoreError::Http {
                op: "head",
                path: path.into(),
                status: s.as_u16(),
            }),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let mut names: Vec<String> = self
            .list_blobs(prefix)
            .await?
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        names.sort();
        Ok(names)
    }

    async fn list_info(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let mut out = Vec::new();
        for (path, xml) in self.list_blobs(prefix).await? {
            let info = blob_info(&xml)
                .ok_or_else(|| StoreError::Io(format!("list `{prefix}`: no size for `{path}`")))?;
            out.push(ObjectInfo { path, ..info });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn get_if_match(
        &self,
        path: &str,
        etag: &str,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let mut resp = self
            .request(Method::GET, path)
            .await?
            .header(IF_MATCH, quoted(etag))
            .send()
            .await
            .map_err(|e| transport("get", path, e))?;
        match resp.status() {
            StatusCode::NOT_FOUND | StatusCode::PRECONDITION_FAILED => return Ok(None),
            s if !s.is_success() => {
                return Err(StoreError::Http {
                    op: "get",
                    path: path.into(),
                    status: s.as_u16(),
                })
            }
            _ => {}
        }
        let max = max_bytes.min(MAX_OBJECT_BYTES);
        if resp.content_length().is_some_and(|n| n > max) {
            return Err(StoreError::TooLarge(path.into()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| transport("get", path, e))? {
            if (body.len() + chunk.len()) as u64 > max {
                return Err(StoreError::TooLarge(path.into()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }

    async fn delete_if_match(&self, path: &str, etag: &str) -> Result<bool, StoreError> {
        let resp = self
            .request(Method::DELETE, path)
            .await?
            .header(IF_MATCH, quoted(etag))
            .send()
            .await
            .map_err(|e| transport("delete", path, e))?;
        match resp.status() {
            StatusCode::ACCEPTED => Ok(true),
            StatusCode::NOT_FOUND | StatusCode::PRECONDITION_FAILED => Ok(false),
            s => Err(StoreError::Http {
                op: "delete",
                path: path.into(),
                status: s.as_u16(),
            }),
        }
    }
}

impl BlobStore {
    /// List Blobs under `prefix`, following continuation markers: each
    /// blob's path (relative to the store) and its `<Blob>` element.
    async fn list_blobs(&self, prefix: &str) -> Result<Vec<(String, String)>, StoreError> {
        validate_path(prefix)?;
        // `base` is `{origin}/{container}[/{store prefix}]`.
        let (container, store_prefix) = match self.base.split_once("://") {
            Some((scheme, rest)) => {
                let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
                let (container, sub) = path.split_once('/').unwrap_or((path, ""));
                (format!("{scheme}://{host}/{container}"), sub.to_owned())
            }
            None => return Err(StoreError::InvalidLocation(self.base.clone())),
        };
        let full = if store_prefix.is_empty() {
            format!("{prefix}/")
        } else {
            format!("{store_prefix}/{prefix}/")
        };
        let strip = if store_prefix.is_empty() {
            String::new()
        } else {
            format!("{store_prefix}/")
        };
        let mut blobs = Vec::new();
        let mut marker = String::new();
        loop {
            let token = self.credential.token().await?;
            let mut query = vec![
                ("restype", "container"),
                ("comp", "list"),
                ("prefix", full.as_str()),
            ];
            if !marker.is_empty() {
                query.push(("marker", marker.as_str()));
            }
            let resp = self
                .http
                .get(&container)
                .query(&query)
                .bearer_auth(token)
                .header("x-ms-version", API_VERSION)
                .header("x-ms-date", httpdate::fmt_http_date(SystemTime::now()))
                .send()
                .await
                .map_err(|e| transport("list", prefix, e))?;
            if !resp.status().is_success() {
                return Err(StoreError::Http {
                    op: "list",
                    path: prefix.into(),
                    status: resp.status().as_u16(),
                });
            }
            let xml = resp
                .text()
                .await
                .map_err(|e| transport("list", prefix, e))?;
            for blob in xml_elements(&xml, "Blob") {
                let Some(name) = xml_values(blob, "Name").next() else {
                    continue;
                };
                if let Some(path) = name.strip_prefix(&strip) {
                    if validate_path(path).is_ok() {
                        blobs.push((path.to_owned(), blob.to_owned()));
                    }
                }
            }
            marker = xml_values(&xml, "NextMarker").next().unwrap_or_default();
            if marker.is_empty() {
                break;
            }
        }
        Ok(blobs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn block_ids_name_their_bytes_and_place() {
        let id = block_id(b"abc", 0);
        assert_eq!(id, block_id(b"abc", 0));
        assert_ne!(id, block_id(b"abd", 0));
        assert_ne!(id, block_id(b"abc", 1));
        // Every id has one length, as Azure requires, and the length of the
        // random-tag ids earlier uploads may have left staged.
        use base64::Engine;
        let raw = |id: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(id)
                .unwrap()
                .len()
        };
        assert_eq!(raw(&id), 40);
        assert_eq!(raw(&block_id(&[7u8; BLOCK_BYTES], 49_999)), 40);
    }

    #[test]
    fn block_timeouts_scale_with_the_block() {
        let t = Tuning::STANDARD;
        assert_eq!(t.block_timeout(0), Duration::from_secs(30));
        assert_eq!(t.block_timeout(BLOCK_BYTES), Duration::from_secs(30 + 128));
        assert!(t.block_timeout(BLOCK_BYTES) < Duration::from_secs(300));
    }
    use crate::credential::StaticToken;

    fn cred() -> Arc<Credential> {
        Arc::new(Credential::new(StaticToken("t".into())))
    }

    #[test]
    fn list_responses() {
        let xml = "<EnumerationResults><Blobs><Blob><Name>a/b.jsonl</Name></Blob>\
                   <Blob><Name>a/x&amp;y</Name></Blob></Blobs><NextMarker>m1</NextMarker></EnumerationResults>";
        assert_eq!(
            xml_values(xml, "Name").collect::<Vec<_>>(),
            ["a/b.jsonl", "a/x&y"]
        );
        assert_eq!(xml_values(xml, "NextMarker").next().as_deref(), Some("m1"));
        assert_eq!(xml_values("<NextMarker />", "NextMarker").next(), None);
    }

    #[test]
    fn list_entries_with_properties() {
        let xml = "<EnumerationResults><Blobs><Blob><Name>pages/a.parquet</Name><Properties>\
                   <Creation-Time>Fri, 09 Oct 2026 21:04:30 GMT</Creation-Time>\
                   <Last-Modified>Fri, 09 Oct 2026 21:05:00 GMT</Last-Modified>\
                   <Etag>0x8DCE1</Etag><Content-Length>1234</Content-Length></Properties></Blob>\
                   <Blob><Name>pages/b</Name><Properties /></Blob></Blobs></EnumerationResults>";
        let blobs: Vec<&str> = xml_elements(xml, "Blob");
        assert_eq!(blobs.len(), 2);
        let ObjectInfo {
            path: name,
            size,
            etag,
            created,
            content_md5,
        } = blob_info(blobs[0]).unwrap();
        assert_eq!(content_md5, None);
        assert_eq!(
            (name.as_str(), size, etag.as_str()),
            ("pages/a.parquet", 1234, "0x8DCE1")
        );
        assert_eq!(
            created,
            Some(httpdate::parse_http_date("Fri, 09 Oct 2026 21:04:30 GMT").unwrap())
        );
        assert_eq!(blob_info(blobs[1]), None, "no size");
        // A name holding escaped markup stays one name: decoded once.
        let sneaky = "<Blobs><Blob><Name>pages/a&lt;/Name&gt;&lt;Content-Length&gt;1</Name>\
                      <Properties><Content-Length>99</Content-Length></Properties></Blob></Blobs>";
        let b = xml_elements(sneaky, "Blob");
        let ObjectInfo {
            path: name, size, ..
        } = blob_info(b[0]).unwrap();
        assert_eq!(
            (name.as_str(), size),
            ("pages/a</Name><Content-Length>1", 99)
        );
        assert!(validate_path(&name).is_err());
        assert_eq!(quoted("0x8DCE1"), "\"0x8DCE1\"");
        assert_eq!(quoted("\"0x8DCE1\""), "\"0x8DCE1\"");
    }

    /// A stand-in container holding one blob with ETag `"e1"`: List Blobs
    /// with properties, and Get and Delete Blob honouring `If-Match`.
    async fn conditional_service() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::http::{HeaderMap, StatusCode as S};
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (l1, l2) = (log.clone(), log.clone());
        let matches =
            |h: &HeaderMap| h.get("if-match").and_then(|v| v.to_str().ok()) == Some("\"e1\"");
        let app = axum::Router::new()
            .route(
                "/c",
                axum::routing::get(|| async {
                    "<EnumerationResults><Blobs><Blob><Name>out/pages/a.parquet</Name><Properties>\
                     <Creation-Time>Fri, 09 Oct 2026 21:04:30 GMT</Creation-Time><Etag>e1</Etag>\
                     <Content-Length>5</Content-Length></Properties></Blob></Blobs>\
                     <NextMarker /></EnumerationResults>"
                }),
            )
            .route(
                "/c/out/pages/a.parquet",
                axum::routing::get(move |h: HeaderMap| {
                    l1.lock().unwrap().push("get".into());
                    async move {
                        if matches(&h) {
                            (S::OK, "hello").into_response()
                        } else {
                            S::PRECONDITION_FAILED.into_response()
                        }
                    }
                })
                .delete(move |h: HeaderMap| {
                    l2.lock().unwrap().push("delete".into());
                    async move {
                        if matches(&h) {
                            S::ACCEPTED
                        } else {
                            S::PRECONDITION_FAILED
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/c/out"), log)
    }

    use axum::response::IntoResponse;

    /// A server that answers each connection with the next of `replies`
    /// (raw HTTP/1.1, then closes), repeating the last, and counts them.
    async fn scripted(replies: Vec<&'static str>) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let n = Arc::new(AtomicUsize::new(0));
        let count = n.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let i = count.fetch_add(1, Ordering::SeqCst);
                let reply = replies[i.min(replies.len() - 1)];
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/c"), n)
    }

    const CUT_SHORT: &str = "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhel";
    const BUSY: &str =
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const DENIED: &str = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const WHOLE: &str =
        "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world";

    #[tokio::test]
    async fn get_tries_again_after_a_cut_body_or_a_busy_server() {
        use std::sync::atomic::Ordering;
        let (url, n) = scripted(vec![CUT_SHORT, BUSY, WHOLE]).await;
        let store = BlobStore::new(&url, cred()).unwrap();
        let got = store.get("pages/a.parquet").await.unwrap().unwrap();
        assert_eq!(got, b"hello world");
        assert_eq!(n.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn get_gives_up_on_a_refusal_and_after_its_tries() {
        use std::sync::atomic::Ordering;
        let (url, n) = scripted(vec![DENIED]).await;
        let store = BlobStore::new(&url, cred()).unwrap();
        let err = store.get("pages/a.parquet").await.unwrap_err();
        assert!(matches!(err, StoreError::Http { status: 403, .. }), "{err}");
        assert_eq!(n.load(Ordering::SeqCst), 1);
        let (url, n) = scripted(vec![CUT_SHORT]).await;
        let store = BlobStore::new(&url, cred()).unwrap();
        let err = store
            .get_retrying("pages/a.parquet", FAST)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Io(_)), "{err}");
        assert_eq!(n.load(Ordering::SeqCst), 4);
        // The text says what went wrong, not only "error decoding response body".
        assert!(err.to_string().contains("get `pages/a.parquet`: "), "{err}");
    }

    /// Four tries, a millisecond or two apart.
    const FAST: Retry = Retry {
        tries: 4,
        first_wait: Duration::from_millis(1),
        max_wait: Duration::from_millis(2),
    };

    /// What the upload server does with one request.
    #[derive(Clone, Debug)]
    enum Act {
        Reply(String),
        /// Close the connection without an answer.
        Drop,
        /// Hold the connection open without an answer.
        Hang,
    }

    fn reply(raw: &str) -> Act {
        Act::Reply(raw.to_owned())
    }

    fn xml(body: &str) -> Act {
        Act::Reply(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ))
    }

    const CREATED: &str = "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const EXISTS: &str = "HTTP/1.1 409 Conflict\r\nx-ms-error-code: BlobAlreadyExists\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const NO_BLOB: &str = "HTTP/1.1 404 Not Found\r\nx-ms-error-code: BlobNotFound\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// One request the upload server saw: its kind (`all` for Get Block
    /// List, `block`, `commit`), its block id and its body.
    type Seen = Arc<std::sync::Mutex<Vec<(String, String, Vec<u8>)>>>;

    /// A server that reads each request whole, then acts as `script` says
    /// for its kind: the next act of that kind's list, repeating the last.
    /// A kind without a list is answered 404 (no such blob) for a Get Block
    /// List and 201 otherwise.
    async fn upload_server(script: Vec<(&'static str, Vec<Act>)>) -> (String, Seen) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let script: HashMap<&'static str, Vec<Act>> = script.into_iter().collect();
        let script = Arc::new(script);
        let counts = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let (script, counts, log) = (script.clone(), counts.clone(), log.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = vec![0u8; 64 * 1024];
                    let head_end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let length: usize = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    while buf.len() < head_end + length {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let mut line = head.split_whitespace();
                    let method = line.next().unwrap_or("").to_owned();
                    let target = line.next().unwrap_or("").to_owned();
                    let url = Url::parse(&format!("http://h{target}")).unwrap();
                    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
                    let kind = match (method.as_str(), q.get("comp").map(String::as_str)) {
                        ("GET", Some("blocklist")) => q["blocklisttype"].clone(),
                        ("PUT", Some("block")) => "block".into(),
                        ("PUT", Some("blocklist")) => "commit".into(),
                        _ => format!("{method} {target}"),
                    };
                    let id = q.get("blockid").cloned().unwrap_or_default();
                    log.lock().unwrap().push((
                        kind.clone(),
                        id,
                        buf[head_end..head_end + length].to_vec(),
                    ));
                    let act = {
                        let mut counts = counts.lock().unwrap();
                        let i = counts.entry(kind.clone()).or_default();
                        let act = match script.get(kind.as_str()) {
                            Some(acts) => acts[(*i).min(acts.len() - 1)].clone(),
                            None if method == "GET" => reply(NO_BLOB),
                            None => reply(CREATED),
                        };
                        *i += 1;
                        act
                    };
                    match act {
                        Act::Reply(raw) => {
                            let _ = sock.write_all(raw.as_bytes()).await;
                            let _ = sock.shutdown().await;
                        }
                        Act::Drop => drop(sock),
                        Act::Hang => {
                            tokio::time::sleep(Duration::from_secs(120)).await;
                            drop(sock);
                        }
                    }
                });
            }
        });
        (format!("http://{addr}/c"), seen)
    }

    /// A store against `url` with 1 KiB blocks, [`FAST`] tries and a
    /// half-second block timeout.
    fn quick_store(url: &str) -> BlobStore {
        let mut s = BlobStore::new(url, cred()).unwrap();
        s.tuning = Tuning {
            get: FAST,
            write: FAST,
            block_bytes: 1024,
            block_timeout_floor: Duration::from_millis(500),
            block_min_rate: u64::MAX,
        };
        s
    }

    /// 2.5 blocks of [`quick_store`]'s size.
    fn upload_data() -> Vec<u8> {
        (0..2560).map(|i| (i % 251) as u8).collect()
    }

    /// Stream `data` into `store` at `path` in uneven chunks and commit it.
    async fn upload(store: &BlobStore, path: &str, data: &[u8]) -> Result<u64, StoreError> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (done, commit) = tokio::sync::oneshot::channel();
        let feed = async move {
            for c in data.chunks(700) {
                if tx.send(Bytes::copy_from_slice(c)).await.is_err() {
                    return; // the upload failed
                }
            }
            drop(tx);
            let _ = done.send(true);
        };
        let (put, ()) = tokio::join!(
            store.put_stream(path, "application/octet-stream", None, rx, commit),
            feed
        );
        put
    }

    fn kinds(seen: &Seen) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(k, _, _)| k.clone())
            .collect()
    }

    /// The ids a commit listed, and the bytes of the blocks sent under them,
    /// in order.
    fn committed(seen: &Seen) -> (Vec<String>, Vec<u8>) {
        let seen = seen.lock().unwrap();
        let (_, _, list) = seen.iter().rev().find(|(k, _, _)| k == "commit").unwrap();
        let ids: Vec<String> = xml_values(&String::from_utf8_lossy(list), "Latest").collect();
        let mut bytes = Vec::new();
        for id in &ids {
            if let Some((_, _, b)) = seen.iter().rev().find(|(k, i, _)| k == "block" && i == id) {
                bytes.extend_from_slice(b);
            }
        }
        (ids, bytes)
    }

    #[tokio::test]
    async fn a_busy_or_dropped_block_is_sent_again() {
        let busy = reply(BUSY);
        let ok = reply(CREATED);
        let (url, seen) =
            upload_server(vec![("block", vec![ok.clone(), busy, Act::Drop, ok])]).await;
        let store = quick_store(&url);
        let data = upload_data();
        assert_eq!(upload(&store, "qw/i/s.split", &data).await.unwrap(), 2560);
        assert_eq!(
            kinds(&seen),
            ["all", "block", "block", "block", "block", "block", "commit"]
        );
        let (ids, bytes) = committed(&seen);
        assert_eq!(ids.len(), 3);
        assert_eq!(bytes, data);
    }

    #[tokio::test]
    async fn a_refused_block_fails_at_once() {
        let (url, seen) = upload_server(vec![("block", vec![reply(DENIED)])]).await;
        let store = quick_store(&url);
        let err = upload(&store, "qw/i/s.split", &upload_data())
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Http { status: 403, .. }), "{err}");
        assert_eq!(kinds(&seen), ["all", "block"]);
    }

    #[tokio::test]
    async fn a_hung_block_times_out_and_is_sent_again() {
        let (url, seen) = upload_server(vec![("block", vec![Act::Hang, reply(CREATED)])]).await;
        let store = quick_store(&url);
        let started = std::time::Instant::now();
        let data = upload_data();
        assert_eq!(upload(&store, "qw/i/s.split", &data).await.unwrap(), 2560);
        assert!(started.elapsed() < Duration::from_secs(30));
        assert_eq!(
            kinds(&seen),
            ["all", "block", "block", "block", "block", "commit"]
        );
        assert_eq!(committed(&seen).1, data);
    }

    #[tokio::test]
    async fn blocks_an_earlier_try_staged_are_not_sent_again() {
        let data = upload_data();
        let (a, b) = (block_id(&data[..1024], 0), block_id(&data[1024..2048], 1));
        // The second block with other bytes, and a block of another upload.
        let other = block_id(&[0u8; 1024], 1);
        let listing = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList><CommittedBlocks />\
             <UncommittedBlocks><Block><Name>{a}</Name><Size>1024</Size></Block>\
             <Block><Name>{other}</Name><Size>1024</Size></Block>\
             <Block><Name>{b}</Name><Size>1000</Size></Block></UncommittedBlocks></BlockList>"
        );
        let (url, seen) = upload_server(vec![("all", vec![xml(&listing)])]).await;
        let store = quick_store(&url);
        assert_eq!(upload(&store, "qw/i/s.split", &data).await.unwrap(), 2560);
        // Only the first block was there with its bytes and size.
        assert_eq!(kinds(&seen), ["all", "block", "block", "commit"]);
        let (ids, _) = committed(&seen);
        assert_eq!(ids[0], a);
        assert_eq!(ids[1], b);
        assert_eq!(ids.len(), 3);
    }

    #[tokio::test]
    async fn a_commit_whose_answer_was_lost_is_recognised() {
        let data = upload_data();
        let ids: Vec<String> = data
            .chunks(1024)
            .enumerate()
            .map(|(n, c)| block_id(c, n))
            .collect();
        let listing = |ids: &[String]| {
            let blocks: String = ids
                .iter()
                .map(|id| format!("<Block><Name>{id}</Name><Size>1024</Size></Block>"))
                .collect();
            format!("<BlockList><CommittedBlocks>{blocks}</CommittedBlocks><UncommittedBlocks /></BlockList>")
        };
        let (url, seen) = upload_server(vec![
            ("commit", vec![Act::Drop, reply(EXISTS)]),
            ("all", vec![reply(NO_BLOB), xml(&listing(&ids))]),
        ])
        .await;
        let store = quick_store(&url);
        assert_eq!(upload(&store, "qw/i/s.split", &data).await.unwrap(), 2560);
        assert_eq!(kinds(&seen)[4..], ["commit", "commit", "all"]);

        // Another upload's blob is there: create-only, so that stays.
        let (url, _) = upload_server(vec![
            ("commit", vec![Act::Drop, reply(EXISTS)]),
            ("all", vec![reply(NO_BLOB), xml(&listing(&ids[..2]))]),
        ])
        .await;
        let err = upload(&quick_store(&url), "qw/i/s.split", &data)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists(_)), "{err}");

        // On a first try a conflict is someone else's blob, and isn't checked.
        let (url, seen) = upload_server(vec![("commit", vec![reply(EXISTS)])]).await;
        let err = upload(&quick_store(&url), "qw/i/s.split", &data)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists(_)), "{err}");
        assert_eq!(kinds(&seen).iter().filter(|k| *k == "all").count(), 1);

        // An empty upload lists no blocks, and neither does a blob written
        // whole: it never takes a blob that is there as its own.
        let (url, _) = upload_server(vec![
            ("commit", vec![Act::Drop, reply(EXISTS)]),
            (
                "all",
                vec![xml(
                    "<BlockList><CommittedBlocks /><UncommittedBlocks /></BlockList>",
                )],
            ),
        ])
        .await;
        let err = upload(&quick_store(&url), "qw/i/s.split", &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists(_)), "{err}");
    }

    /// A commit whose answer was lost and whose other tries all failed: the
    /// next upload of the same bytes (a publish's next round for the split)
    /// sends no block again and takes the blob as its own. Another process,
    /// which sent no such commit, still finds it already there.
    #[tokio::test]
    async fn a_commit_lost_on_its_last_try_is_found_by_the_next_upload() {
        let data = upload_data();
        let ids: Vec<String> = data
            .chunks(1024)
            .enumerate()
            .map(|(n, c)| block_id(c, n))
            .collect();
        let blocks: String = ids
            .iter()
            .zip([1024, 1024, 512])
            .map(|(id, n)| format!("<Block><Name>{id}</Name><Size>{n}</Size></Block>"))
            .collect();
        let committed =
            format!("<BlockList><CommittedBlocks>{blocks}</CommittedBlocks><UncommittedBlocks /></BlockList>");
        let lost = vec![Act::Drop; FAST.tries as usize];
        let (url, seen) = upload_server(vec![
            ("commit", [lost, vec![reply(EXISTS)]].concat()),
            ("all", vec![reply(NO_BLOB), xml(&committed)]),
        ])
        .await;
        let store = quick_store(&url);
        let err = upload(&store, "qw/i/s.split", &data).await.unwrap_err();
        assert!(matches!(err, StoreError::Io(_)), "{err}");
        assert_eq!(upload(&store, "qw/i/s.split", &data).await.unwrap(), 2560);
        let k = kinds(&seen);
        assert_eq!(k.iter().filter(|k| *k == "block").count(), 3, "{k:?}");
        assert_eq!(k[k.len() - 3..], ["all", "commit", "all"]);

        let (url, seen) = upload_server(vec![
            ("commit", vec![reply(EXISTS)]),
            ("all", vec![xml(&committed)]),
        ])
        .await;
        let err = upload(&quick_store(&url), "qw/i/s.split", &data)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists(_)), "{err}");
        assert_eq!(kinds(&seen), ["all", "commit"]);
    }

    #[tokio::test]
    async fn an_unlisted_upload_still_sends_every_block() {
        // Listing staged blocks fails; the upload goes on without it.
        let (url, seen) = upload_server(vec![("all", vec![reply(DENIED)])]).await;
        let store = quick_store(&url);
        assert_eq!(
            upload(&store, "qw/i/s.split", &upload_data())
                .await
                .unwrap(),
            2560
        );
        assert_eq!(kinds(&seen), ["all", "block", "block", "block", "commit"]);
    }

    #[tokio::test]
    async fn conditional_reads_and_deletes() {
        let (url, log) = conditional_service().await;
        let store = BlobStore::new(&url, cred()).unwrap();
        let listed = store.list_info("pages").await.unwrap();
        assert_eq!(listed.len(), 1);
        let a = &listed[0];
        assert_eq!(
            (a.path.as_str(), a.size, a.etag.as_str()),
            ("pages/a.parquet", 5, "e1")
        );
        assert!(a.created.is_some());
        assert_eq!(
            store.get_if_match(&a.path, &a.etag, 5).await.unwrap(),
            Some(b"hello".to_vec())
        );
        assert!(matches!(
            store.get_if_match(&a.path, &a.etag, 4).await,
            Err(StoreError::TooLarge(_))
        ));
        assert_eq!(store.get_if_match(&a.path, "e2", 5).await.unwrap(), None);
        assert!(!store.delete_if_match(&a.path, "e2").await.unwrap());
        assert!(store.delete_if_match(&a.path, &a.etag).await.unwrap());
        assert_eq!(
            *log.lock().unwrap(),
            ["get", "get", "get", "delete", "delete"]
        );
    }

    #[test]
    fn locations() {
        let ok = BlobStore::new("https://acct.blob.core.windows.net/reference/", cred()).unwrap();
        assert_eq!(ok.base, "https://acct.blob.core.windows.net/reference");
        let ok = BlobStore::new("https://acct.blob.core.windows.net/cache/api", cred()).unwrap();
        assert_eq!(ok.base, "https://acct.blob.core.windows.net/cache/api");
        assert!(BlobStore::new("http://127.0.0.1:10000/c", cred()).is_ok());
        assert!(BlobStore::new("https://acct.blob.core.usgovcloudapi.net/c", cred()).is_ok());
        assert!(BlobStore::new("https://ACCT.BLOB.CORE.WINDOWS.NET/c", cred()).is_ok());
        for bad in [
            "https://acct.blob.core.windows.net",
            "https://acct.blob.core.windows.net/",
            "http://acct.blob.core.windows.net/c",
            "https://acct.blob.core.windows.net/c?sv=2024&sig=x",
            "https://u:p@acct.blob.core.windows.net/c",
            "ftp://acct/c",
            "https://example.com/c",
            "https://acct.blob.core.windows.net.evil.example/c",
            "https://.blob.core.windows.net/c",
        ] {
            assert!(BlobStore::new(bad, cred()).is_err(), "{bad}");
        }
    }
}
