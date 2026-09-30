//! Page views for Application Insights (`AppPageViews`), sent on behalf of
//! the web app (07 §7.8, 08 §8.1.2).
//!
//! The browser SDK can't send to a component with local auth disabled
//! (ADR-0009), so the web app reports each page view to the API
//! (`POST /v1/beacon`) and the API forwards it here. Envelopes are batched
//! (up to [`MAX_BATCH`], or [`BATCH_WAIT`] after the first) and uploaded
//! through the same Entra-signed client as the traces and metrics. A page view
//! that can't be queued or uploaded is dropped: they are statistics, and
//! nothing waits on them.
//!
//! Nothing here logs a page view's contents or the client address.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::IpAddr;
use std::time::Duration;

use opentelemetry_http::{Bytes, HttpClient, Request};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

/// Page views held for upload at most; more are dropped.
const QUEUE: usize = 1000;
/// Envelopes per upload at most.
pub const MAX_BATCH: usize = 100;
/// How long the first page view of a batch waits for others.
pub const BATCH_WAIT: Duration = Duration::from_secs(5);
/// How long [`PageViews::flush`] waits for the upload.
const FLUSH_WAIT: Duration = Duration::from_secs(10);

/// One page view.
#[derive(Debug, Clone, Default)]
pub struct PageView {
    /// The page's name (`AppPageViews.Name`).
    pub name: String,
    /// The page's URL, without a query string (`Url`).
    pub url: Option<String>,
    /// The visitor's address, sent only as the `ai.location.ip` tag. Ingestion
    /// derives the city, region and country from it, then stores `0.0.0.0`
    /// in its place (the component keeps IP masking on).
    pub client_ip: Option<IpAddr>,
    /// Custom dimensions (`Properties`).
    pub properties: BTreeMap<String, String>,
}

enum Msg {
    View(Value),
    Flush(oneshot::Sender<()>),
}

/// A handle on the uploader. Clones share one queue.
#[derive(Clone)]
pub struct PageViews {
    tx: mpsc::Sender<Msg>,
    ikey: std::sync::Arc<str>,
    role: &'static str,
    sdk: std::sync::Arc<str>,
}

impl std::fmt::Debug for PageViews {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageViews").finish_non_exhaustive()
    }
}

impl PageViews {
    /// Start uploading to the component named by `connection_string`,
    /// through `client`, on the current Tokio runtime. Page views report the
    /// cloud role `role`; `sdk` goes in `ai.internal.sdkVersion`.
    pub fn start<C>(
        connection_string: &str,
        client: C,
        role: &'static str,
        sdk: &str,
    ) -> anyhow::Result<Self>
    where
        C: HttpClient + 'static,
    {
        let (ikey, endpoint) = parse_connection_string(connection_string)?;
        let url = format!("{}/v2/track", endpoint.trim_end_matches('/'));
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(run(rx, url, client));
        Ok(Self {
            tx,
            ikey: ikey.into(),
            role,
            sdk: sdk.into(),
        })
    }

    /// Queue a page view, timestamped now. False if the queue is full (the
    /// page view is dropped).
    pub fn track(&self, view: PageView) -> bool {
        let envelope = self.envelope(view, chrono::Utc::now());
        self.tx.try_send(Msg::View(envelope)).is_ok()
    }

    /// Upload what is queued, waiting at most 10 s.
    pub async fn flush(&self) {
        let (ack, done) = oneshot::channel();
        // The wait for room in a full queue counts against the limit too.
        let _ = tokio::time::timeout(FLUSH_WAIT, async {
            if self.tx.send(Msg::Flush(ack)).await.is_ok() {
                let _ = done.await;
            }
        })
        .await;
    }

    fn envelope(&self, view: PageView, time: chrono::DateTime<chrono::Utc>) -> Value {
        let mut tags = serde_json::Map::new();
        tags.insert("ai.cloud.role".into(), self.role.into());
        tags.insert("ai.internal.sdkVersion".into(), self.sdk.as_ref().into());
        if let Some(ip) = view.client_ip {
            tags.insert(
                "ai.location.ip".into(),
                ip.to_canonical().to_string().into(),
            );
        }
        let mut base = json!({
            "ver": 2,
            "name": view.name,
            "duration": "00:00:00.000",
            "properties": view.properties,
        });
        if let Some(url) = view.url {
            base["url"] = url.into();
        }
        json!({
            "name": "Microsoft.ApplicationInsights.PageView",
            "time": time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "iKey": self.ikey.as_ref(),
            "tags": tags,
            "data": { "baseType": "PageViewData", "baseData": base },
        })
    }
}

/// `(instrumentation key, ingestion endpoint)` from a connection string.
pub fn parse_connection_string(s: &str) -> anyhow::Result<(String, String)> {
    let mut ikey = None;
    let mut endpoint = None;
    for part in s.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let v = v.trim();
        if k.trim().eq_ignore_ascii_case("InstrumentationKey") && !v.is_empty() {
            ikey = Some(v.to_owned());
        } else if k.trim().eq_ignore_ascii_case("IngestionEndpoint") && !v.is_empty() {
            endpoint = Some(v.to_owned());
        }
    }
    let ikey = ikey.ok_or_else(|| anyhow::anyhow!("no InstrumentationKey"))?;
    let endpoint = endpoint.unwrap_or_else(|| "https://dc.services.visualstudio.com/".to_owned());
    if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
        anyhow::bail!("IngestionEndpoint must be an http(s) URL");
    }
    Ok((ikey, endpoint))
}

async fn run<C: HttpClient>(mut rx: mpsc::Receiver<Msg>, url: String, client: C) {
    let mut batch: Vec<Value> = Vec::new();
    let mut deadline = tokio::time::Instant::now();
    loop {
        let msg = if batch.is_empty() {
            rx.recv().await
        } else {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(msg) => msg,
                Err(_) => {
                    upload(&client, &url, &mut batch).await;
                    continue;
                }
            }
        };
        match msg {
            Some(Msg::View(v)) => {
                if batch.is_empty() {
                    deadline = tokio::time::Instant::now() + BATCH_WAIT;
                }
                batch.push(v);
                if batch.len() >= MAX_BATCH {
                    upload(&client, &url, &mut batch).await;
                }
            }
            Some(Msg::Flush(ack)) => {
                upload(&client, &url, &mut batch).await;
                let _ = ack.send(());
            }
            None => {
                upload(&client, &url, &mut batch).await;
                return;
            }
        }
    }
}

/// How many of `sent` envelopes the ingestion response says it didn't
/// accept. A 206 lists them; a body that can't be read counts as all
/// accepted, as a 200 without one is.
fn rejected(body: &[u8], sent: usize) -> usize {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return 0;
    };
    let accepted = v["itemsAccepted"].as_u64();
    let received = v["itemsReceived"].as_u64().unwrap_or(sent as u64);
    match accepted {
        Some(a) => usize::try_from(received.saturating_sub(a)).unwrap_or(sent),
        None => v["errors"].as_array().map_or(0, Vec::len),
    }
}

/// Send and clear `batch`. Failures are logged by count only.
async fn upload<C: HttpClient>(client: &C, url: &str, batch: &mut Vec<Value>) {
    if batch.is_empty() {
        return;
    }
    let items = batch.len();
    let body = Value::Array(std::mem::take(batch)).to_string();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let compressed = gz.write_all(body.as_bytes()).and_then(|()| gz.finish());
    let result = match compressed {
        Ok(bytes) => match Request::post(url)
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(Bytes::from(bytes))
        {
            Ok(req) => client
                .send_bytes(req)
                .await
                .map_err(|e| e.to_string())
                .and_then(|resp| {
                    let status = resp.status().as_u16();
                    if !(200..300).contains(&status) {
                        return Err(format!("status {status}"));
                    }
                    match rejected(resp.body(), items) {
                        0 => Ok(()),
                        n => Err(format!("status {status}, {n} of {items} rejected")),
                    }
                }),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = result {
        tracing::warn!(target: "telemetry", error = %e, items, "page view upload failed; dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::rejected;

    #[test]
    fn partial_acceptance_is_counted() {
        let partial = br#"{"itemsReceived":3,"itemsAccepted":1,"errors":[{"index":0,"statusCode":400},{"index":2,"statusCode":500}]}"#;
        assert_eq!(rejected(partial, 3), 2);
        assert_eq!(
            rejected(br#"{"itemsReceived":3,"itemsAccepted":3,"errors":[]}"#, 3),
            0
        );
        assert_eq!(rejected(br#"{"errors":[{"index":1}]}"#, 3), 1);
        assert_eq!(rejected(b"", 3), 0);
    }
}
