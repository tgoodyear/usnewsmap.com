//! Entra ID access tokens for Azure services (Blob Storage, Cosmos DB, Azure
//! Monitor ingestion).
//!
//! In Container Apps the managed identity endpoint (`IDENTITY_ENDPOINT` +
//! `IDENTITY_HEADER`) issues tokens; `AZURE_CLIENT_ID` selects the
//! user-assigned identity (`id-usnm-app`). Elsewhere (a developer machine) the
//! Azure CLI's signed-in account is used. Tokens are cached until five minutes
//! before they expire, and one refresh at a time is in flight.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::StoreError;

/// Token audience for Azure Storage.
pub const STORAGE_RESOURCE: &str = "https://storage.azure.com/";

/// Token audience for the Cosmos DB data plane.
pub const COSMOS_RESOURCE: &str = "https://cosmos.azure.com";

/// Token audience for Azure Monitor ingestion (Application Insights with
/// local auth disabled; the scope is `https://monitor.azure.com/.default`).
pub const MONITOR_RESOURCE: &str = "https://monitor.azure.com";

const REFRESH_MARGIN: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub struct AccessToken {
    pub token: String,
    pub expires_at: SystemTime,
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessToken")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait TokenSource: Send + Sync + fmt::Debug {
    async fn fetch(&self) -> Result<AccessToken, StoreError>;
}

/// Caches a [`TokenSource`]'s token and refreshes it before it expires.
#[derive(Debug)]
pub struct Credential {
    source: Box<dyn TokenSource>,
    cached: Mutex<Option<AccessToken>>,
}

impl Credential {
    pub fn new(source: impl TokenSource + 'static) -> Self {
        Self {
            source: Box::new(source),
            cached: Mutex::new(None),
        }
    }

    pub async fn token(&self) -> Result<String, StoreError> {
        let mut cached = self.cached.lock().await;
        if let Some(t) = cached.as_ref() {
            if t.expires_at > SystemTime::now() + REFRESH_MARGIN {
                return Ok(t.token.clone());
            }
        }
        let fresh = self.source.fetch().await?;
        let token = fresh.token.clone();
        *cached = Some(fresh);
        Ok(token)
    }
}

/// The ambient credential for Blob Storage: managed identity when its
/// endpoint is present, otherwise the Azure CLI.
pub fn from_env() -> Arc<Credential> {
    from_env_for(STORAGE_RESOURCE)
}

/// The ambient credential for another resource (e.g. [`COSMOS_RESOURCE`]).
pub fn from_env_for(resource: &'static str) -> Arc<Credential> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match (var("IDENTITY_ENDPOINT"), var("IDENTITY_HEADER")) {
        (Some(endpoint), Some(header)) => Arc::new(Credential::new(
            ManagedIdentity::new(endpoint, header, var("AZURE_CLIENT_ID")).with_resource(resource),
        )),
        _ => Arc::new(Credential::new(AzureCli { resource })),
    }
}

/// The App Service / Container Apps managed identity endpoint (API version 2019-08-01).
pub struct ManagedIdentity {
    endpoint: String,
    header: String,
    client_id: Option<String>,
    resource: &'static str,
    http: reqwest::Client,
}

impl ManagedIdentity {
    pub fn new(endpoint: String, header: String, client_id: Option<String>) -> Self {
        Self {
            endpoint,
            header,
            client_id,
            resource: STORAGE_RESOURCE,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("static client config"),
        }
    }

    pub fn with_resource(mut self, resource: &'static str) -> Self {
        self.resource = resource;
        self
    }
}

impl fmt::Debug for ManagedIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedIdentity")
            .field("endpoint", &self.endpoint)
            .field("client_id", &self.client_id)
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl TokenSource for ManagedIdentity {
    async fn fetch(&self) -> Result<AccessToken, StoreError> {
        let mut query = vec![("api-version", "2019-08-01"), ("resource", self.resource)];
        if let Some(id) = &self.client_id {
            query.push(("client_id", id));
        }
        let resp = self
            .http
            .get(&self.endpoint)
            .query(&query)
            .header("X-IDENTITY-HEADER", &self.header)
            .send()
            .await
            .map_err(|e| StoreError::Credential(format!("managed identity: {e}")))?;
        if !resp.status().is_success() {
            return Err(StoreError::Credential(format!(
                "managed identity returned HTTP {}",
                resp.status().as_u16()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| StoreError::Credential(format!("managed identity: {e}")))?;
        parse_token(&body, "access_token", "expires_on")
    }
}

/// `az account get-access-token` for local development.
#[derive(Debug)]
pub struct AzureCli {
    pub resource: &'static str,
}

#[async_trait]
impl TokenSource for AzureCli {
    async fn fetch(&self) -> Result<AccessToken, StoreError> {
        let out = tokio::process::Command::new("az")
            .args([
                "account",
                "get-access-token",
                "--resource",
                self.resource,
                "--output",
                "json",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| {
                StoreError::Credential(format!(
                    "no managed identity endpoint and the Azure CLI is unavailable: {e}"
                ))
            })?;
        if !out.status.success() {
            return Err(StoreError::Credential(
                "`az account get-access-token` failed; run `az login`".into(),
            ));
        }
        let body: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| StoreError::Credential(format!("Azure CLI output: {e}")))?;
        parse_token(&body, "accessToken", "expires_on")
    }
}

/// A fixed token (tests and emulators). It never expires.
pub struct StaticToken(pub String);

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StaticToken(..)")
    }
}

#[async_trait]
impl TokenSource for StaticToken {
    async fn fetch(&self) -> Result<AccessToken, StoreError> {
        Ok(AccessToken {
            token: self.0.clone(),
            expires_at: SystemTime::now() + Duration::from_secs(365 * 24 * 3600),
        })
    }
}

/// Read a token and its expiry (Unix seconds, as a number or a string). A
/// missing expiry is treated as ten minutes so the token is refreshed soon.
fn parse_token(body: &Value, token_key: &str, expiry_key: &str) -> Result<AccessToken, StoreError> {
    let token = body[token_key]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| StoreError::Credential(format!("response has no `{token_key}`")))?;
    let expires = match &body[expiry_key] {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    };
    let expires_at = expires.map_or_else(
        || SystemTime::now() + Duration::from_secs(600),
        |secs| UNIX_EPOCH + Duration::from_secs(secs),
    );
    Ok(AccessToken {
        token: token.to_owned(),
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn parses_string_and_numeric_expiry() {
        let t = parse_token(
            &serde_json::json!({"access_token": "a", "expires_on": "4102444800"}),
            "access_token",
            "expires_on",
        )
        .unwrap();
        assert_eq!(
            t.expires_at,
            UNIX_EPOCH + Duration::from_secs(4_102_444_800)
        );
        let t = parse_token(
            &serde_json::json!({"accessToken": "b", "expires_on": 4102444800u64}),
            "accessToken",
            "expires_on",
        )
        .unwrap();
        assert_eq!(t.token, "b");
        assert!(parse_token(&serde_json::json!({}), "access_token", "expires_on").is_err());
    }

    #[derive(Debug)]
    struct Counting(Arc<AtomicU32>, u64);

    #[async_trait]
    impl TokenSource for Counting {
        async fn fetch(&self) -> Result<AccessToken, StoreError> {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(AccessToken {
                token: format!("t{n}"),
                expires_at: SystemTime::now() + Duration::from_secs(self.1),
            })
        }
    }

    #[tokio::test]
    async fn caches_until_near_expiry() {
        let n = Arc::new(AtomicU32::new(0));
        let long = Credential::new(Counting(n.clone(), 3600));
        assert_eq!(long.token().await.unwrap(), "t0");
        assert_eq!(long.token().await.unwrap(), "t0");
        // Inside the refresh margin: fetched every time.
        let short = Credential::new(Counting(n.clone(), 60));
        assert_eq!(short.token().await.unwrap(), "t1");
        assert_eq!(short.token().await.unwrap(), "t2");
    }
}
