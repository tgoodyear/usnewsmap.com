//! Traces and metrics reach an Application Insights ingestion endpoint with
//! an Entra bearer token, and shutdown flushes them.

use std::io::Read;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::{Tracer as _, TracerProvider as _};
use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
use usnm_ingest::telemetry::{providers, EntraClient};
use usnm_store::credential::{Credential, StaticToken};

type Seen = Arc<Mutex<Vec<(String, Option<String>, String)>>>;

/// A fake ingestion endpoint that records (path, authorization, JSON body).
async fn fake_ingestion() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = axum::Router::new().fallback({
        let seen = seen.clone();
        move |req: axum::extract::Request| async move {
            let path = req.uri().path().to_owned();
            let auth = req
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
            seen.lock().unwrap().push((path, auth, json));
            format!(r#"{{"itemsReceived":{items},"itemsAccepted":{items},"errors":[]}}"#)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/"), seen)
}

/// The exporter only accepts https ingestion endpoints; the fake one is
/// plain http.
#[derive(Debug, Clone)]
struct Plain(EntraClient);

#[async_trait]
impl HttpClient for Plain {
    async fn send_bytes(&self, mut req: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let uri = req.uri().to_string().replacen("https://", "http://", 1);
        *req.uri_mut() = uri.parse()?;
        self.0.send_bytes(req).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_with_a_bearer_token_and_flushes_on_shutdown() {
    let (endpoint, seen) = fake_ingestion().await;
    let conn = format!(
        "InstrumentationKey=00000000-0000-0000-0000-000000000000;IngestionEndpoint={endpoint}"
    );
    let client = Plain(EntraClient::new(
        Arc::new(Credential::new(StaticToken("tok".into()))),
        tokio::runtime::Handle::current(),
    ));
    let (tracer, meter) = providers(&conn, client).unwrap();
    meter
        .meter("usnm-ingest")
        .u64_counter("ingest.docs_sent")
        .build()
        .add(3, &[]);
    tracer.tracer("usnm-ingest").in_span("release", |_| {});

    // As Telemetry::shutdown does: the SDK blocks while the export runs on
    // this runtime.
    tokio::task::spawn_blocking(move || {
        tracer.shutdown().unwrap();
        meter.shutdown().unwrap();
    })
    .await
    .unwrap();

    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "nothing was exported");
    for (path, auth, _) in seen.iter() {
        assert_eq!(path, "/v2.1/track");
        assert_eq!(auth.as_deref(), Some("Bearer tok"));
    }
    let all: String = seen.iter().map(|(_, _, j)| j.as_str()).collect();
    assert!(all.contains("ingest.docs_sent"), "{all}");
    assert!(all.contains("\"release\""), "{all}");
}
