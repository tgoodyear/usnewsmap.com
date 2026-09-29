//! A fake Application Insights ingestion endpoint, for tests that check what
//! a service exports.

use std::io::Read;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
use usnm_store::credential::{Credential, StaticToken};

use crate::EntraClient;

/// One upload the fake endpoint received.
#[derive(Debug, Clone)]
pub struct Upload {
    pub path: String,
    pub authorization: Option<String>,
    /// The decompressed body: a JSON array of envelopes.
    pub json: String,
}

/// What the endpoint has received so far.
#[derive(Debug, Clone, Default)]
pub struct Seen(Arc<Mutex<Vec<Upload>>>);

impl Seen {
    pub fn uploads(&self) -> Vec<Upload> {
        self.0.lock().unwrap().clone()
    }

    /// Every body, concatenated.
    pub fn all_json(&self) -> String {
        self.uploads().iter().map(|u| u.json.as_str()).collect()
    }
}

/// The fake ingestion endpoint. Returns its base URL (the connection
/// string's `IngestionEndpoint`) and what it receives.
pub async fn fake_ingestion() -> (String, Seen) {
    let seen = Seen::default();
    let app = axum::Router::new().fallback({
        let seen = seen.clone();
        move |req: axum::extract::Request| async move {
            let path = req.uri().path().to_owned();
            let authorization = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned());
            let body = axum::body::to_bytes(req.into_body(), 16 << 20)
                .await
                .unwrap();
            let mut json = String::new();
            flate2::read::GzDecoder::new(&body[..])
                .read_to_string(&mut json)
                .unwrap();
            let items = json.matches("\"iKey\"").count();
            seen.0.lock().unwrap().push(Upload {
                path,
                authorization,
                json,
            });
            format!(r#"{{"itemsReceived":{items},"itemsAccepted":{items},"errors":[]}}"#)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/"), seen)
}

/// An [`EntraClient`] with the static token `tok`, which also sends to plain
/// http: the exporter turns the fake endpoint's address into https.
#[derive(Debug, Clone)]
pub struct Plain(pub EntraClient);

impl Plain {
    pub fn with_token(token: &str) -> Self {
        Self(EntraClient::new(
            Arc::new(Credential::new(StaticToken(token.into()))),
            tokio::runtime::Handle::current(),
        ))
    }
}

#[async_trait]
impl HttpClient for Plain {
    async fn send_bytes(&self, mut req: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let uri = req.uri().to_string().replacen("https://", "http://", 1);
        *req.uri_mut() = uri.parse()?;
        self.0.send_bytes(req).await
    }
}
