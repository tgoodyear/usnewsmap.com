//! Azure Blob Storage over its REST API with Entra ID bearer tokens.
//!
//! Only two operations are needed (Get Blob, and Put Blob with
//! `If-None-Match: *`), so this talks to the REST API directly with the
//! workspace's `reqwest` client rather than pulling in the Azure SDK.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, IF_NONE_MATCH};
use reqwest::{Method, StatusCode, Url};

use crate::credential::Credential;
use crate::{validate_path, ObjectStore, StoreError, MAX_OBJECT_BYTES};

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

fn transport(op: &'static str, path: &str, e: reqwest::Error) -> StoreError {
    // Strip the URL: the error's own text is enough and paths are logged separately.
    StoreError::Io(format!("{op} `{path}`: {}", e.without_url()))
}

#[async_trait]
impl ObjectStore for BlobStore {
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
        let code = resp
            .headers()
            .get("x-ms-error-code")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        match (resp.status(), code) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::StaticToken;

    fn cred() -> Arc<Credential> {
        Arc::new(Credential::new(StaticToken("t".into())))
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
