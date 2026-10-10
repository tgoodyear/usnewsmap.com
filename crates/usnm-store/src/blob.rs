//! Azure Blob Storage over its REST API with Entra ID bearer tokens.
//!
//! Only a few operations are needed (Get Blob, Get Blob Properties, Put Blob,
//! and List Blobs for the search log), so this talks to the REST API
//! directly with the workspace's `reqwest` client rather than pulling in the
//! Azure SDK.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, IF_NONE_MATCH};
use reqwest::{Method, StatusCode, Url};

use crate::credential::Credential;
use crate::{validate_path, ByteStream, ObjectStore, StoreError, MAX_OBJECT_BYTES};

/// Bytes per block of a streamed upload (`put_stream`): 50,000 blocks of
/// 8 MiB allow 390 GiB, and the largest batch archive is 3.8 GB.
pub const BLOCK_BYTES: usize = 8 * 1024 * 1024;

/// How long one streamed read or block may take: a whole archive is read
/// in one response.
const STREAM_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// A prefix for one upload's block ids: Azure stages blocks by blob and id,
/// so two uploads to the same blob at once must not share ids, or a commit
/// could mix their blocks.
fn upload_tag() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mix = nanos
        ^ (u64::from(std::process::id()) << 32)
        ^ SEQ.fetch_add(1, Ordering::Relaxed).rotate_left(17);
    format!("{mix:016x}")
}

/// The `n`th block's id in upload `tag`: the same length for every block, base64.
fn block_id(tag: &str, n: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{tag}{n:08}"))
}

/// The Put Block List body committing `ids` in order.
fn block_list(ids: &[String]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
    for id in ids {
        xml.push_str("<Latest>");
        xml.push_str(id);
        xml.push_str("</Latest>");
    }
    xml.push_str("</BlockList>");
    xml
}

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

fn transport(op: &'static str, path: &str, e: reqwest::Error) -> StoreError {
    // Strip the URL: the error's own text is enough and paths are logged separately.
    StoreError::Io(format!("{op} `{path}`: {}", e.without_url()))
}

impl BlobStore {
    async fn put_block(&self, path: &str, id: &str, body: Vec<u8>) -> Result<(), StoreError> {
        let resp = self
            .request(Method::PUT, path)
            .await?
            .query(&[("comp", "block"), ("blockid", id)])
            .header(CONTENT_LENGTH, body.len())
            .timeout(STREAM_TIMEOUT)
            .body(body)
            .send()
            .await
            .map_err(|e| transport("put block", path, e))?;
        match resp.status() {
            StatusCode::CREATED => Ok(()),
            s => Err(StoreError::Http {
                op: "put block",
                path: path.into(),
                status: s.as_u16(),
            }),
        }
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
    async fn put_stream(
        &self,
        path: &str,
        content_type: &str,
        tier: Option<&str>,
        mut chunks: tokio::sync::mpsc::Receiver<bytes::Bytes>,
        commit: tokio::sync::oneshot::Receiver<bool>,
    ) -> Result<u64, StoreError> {
        validate_path(path)?;
        let tag = upload_tag();
        let mut ids = Vec::new();
        let mut buf: Vec<u8> = Vec::with_capacity(BLOCK_BYTES);
        let mut total = 0u64;
        while let Some(c) = chunks.recv().await {
            total += c.len() as u64;
            let mut rest = &c[..];
            while !rest.is_empty() {
                let take = (BLOCK_BYTES - buf.len()).min(rest.len());
                buf.extend_from_slice(&rest[..take]);
                rest = &rest[take..];
                if buf.len() == BLOCK_BYTES {
                    let id = block_id(&tag, ids.len());
                    let body = std::mem::replace(&mut buf, Vec::with_capacity(BLOCK_BYTES));
                    self.put_block(path, &id, body).await?;
                    ids.push(id);
                }
            }
        }
        if !buf.is_empty() {
            let id = block_id(&tag, ids.len());
            self.put_block(path, &id, buf).await?;
            ids.push(id);
        }
        if commit.await != Ok(true) {
            return Err(StoreError::Io(format!(
                "`{path}`: upload abandoned; its blocks were not committed"
            )));
        }
        let mut req = self
            .request(Method::PUT, path)
            .await?
            .query(&[("comp", "blocklist")])
            .header(IF_NONE_MATCH, "*")
            .header("x-ms-blob-content-type", content_type)
            .header(CONTENT_TYPE, "application/xml");
        if let Some(t) = tier {
            req = req.header("x-ms-access-tier", t);
        }
        let resp = req
            .body(block_list(&ids))
            .send()
            .await
            .map_err(|e| transport("put block list", path, e))?;
        match (resp.status(), error_code(&resp)) {
            (StatusCode::CREATED, _) => Ok(total),
            (StatusCode::CONFLICT, "BlobAlreadyExists")
            | (StatusCode::PRECONDITION_FAILED, "ConditionNotMet") => {
                Err(StoreError::AlreadyExists(path.into()))
            }
            (s, _) => Err(StoreError::Http {
                op: "put block list",
                path: path.into(),
                status: s.as_u16(),
            }),
        }
    }

    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
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

    /// List Blobs, following continuation markers.
    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
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
        let mut names = Vec::new();
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
            for name in xml_values(&xml, "Name") {
                if let Some(path) = name.strip_prefix(&strip) {
                    if validate_path(path).is_ok() {
                        names.push(path.to_owned());
                    }
                }
            }
            marker = xml_values(&xml, "NextMarker").next().unwrap_or_default();
            if marker.is_empty() {
                break;
            }
        }
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_never_share_block_ids() {
        let (a, b) = (upload_tag(), upload_tag());
        assert_ne!(a, b);
        assert_ne!(block_id(&a, 0), block_id(&b, 0));
        // Every id of an upload has the same length, as Azure requires.
        assert_eq!(block_id(&a, 0).len(), block_id(&a, 49_999).len());
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
