//! Cosmos DB for NoSQL over its REST API with Entra ID tokens (05 §5.9.1).
//!
//! Local (key) auth is disabled on the account, so every request carries a
//! managed identity token (`type=aad`). Only point reads, create, conditional
//! replace, upsert and a one-field query are used; 429s are retried after the
//! service's `x-ms-retry-after-ms`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context};
use async_trait::async_trait;
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde_json::{json, Value};
use usnm_store::credential::Credential;

use crate::docs::{DocStore, Versioned};

const API_VERSION: &str = "2018-12-31";
const MAX_RETRIES: u32 = 8;

pub struct CosmosDocs {
    /// `https://{account}.documents.azure.com/`
    endpoint: Url,
    database: String,
    credential: Arc<Credential>,
    http: reqwest::Client,
    /// Latest session token per container and partition key range. The
    /// account uses Session consistency, so echoing these gives this client
    /// read-your-writes (a claim sees the enqueue that preceded it).
    sessions: Mutex<HashMap<String, BTreeMap<String, String>>>,
}

impl CosmosDocs {
    pub fn new(
        endpoint: &str,
        database: &str,
        credential: Arc<Credential>,
    ) -> anyhow::Result<Self> {
        let endpoint = Url::parse(endpoint).context("Cosmos endpoint")?;
        let loopback = matches!(endpoint.host_str(), Some("localhost" | "127.0.0.1"));
        let cosmos_host = endpoint.host_str().is_some_and(|h| {
            h.ends_with(".documents.azure.com") || h.ends_with(".documents.azure.us")
        });
        match endpoint.scheme() {
            "https" if cosmos_host => {}
            "http" if loopback => {}
            _ => bail!("Cosmos endpoint must be https://{{account}}.documents.azure.com/"),
        }
        Ok(Self {
            endpoint,
            database: database.to_owned(),
            credential,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            sessions: Mutex::default(),
        })
    }

    fn docs_url(&self, container: &str, id: Option<&str>) -> anyhow::Result<Url> {
        let mut path = format!("dbs/{}/colls/{container}/docs", self.database);
        if let Some(id) = id {
            path.push('/');
            path.push_str(id);
        }
        Ok(self.endpoint.join(&path)?)
    }

    fn session_token(&self, container: &str) -> Option<String> {
        let sessions = self.sessions.lock().expect("sessions");
        let ranges = sessions.get(container)?;
        (!ranges.is_empty()).then(|| {
            ranges
                .iter()
                .map(|(range, token)| format!("{range}:{token}"))
                .collect::<Vec<_>>()
                .join(",")
        })
    }

    /// Record `x-ms-session-token` (`{range}:{token}`, comma-separated).
    fn remember_session(&self, container: &str, resp: &Response) {
        let Some(header) = resp
            .headers()
            .get("x-ms-session-token")
            .and_then(|v| v.to_str().ok())
        else {
            return;
        };
        let mut sessions = self.sessions.lock().expect("sessions");
        let ranges = sessions.entry(container.to_owned()).or_default();
        for part in header.split(',') {
            if let Some((range, token)) = part.trim().split_once(':') {
                ranges.insert(range.to_owned(), token.to_owned());
            }
        }
    }

    async fn send(
        &self,
        container: &str,
        build: impl Fn() -> RequestBuilder,
        pk: Option<&str>,
    ) -> anyhow::Result<Response> {
        for attempt in 0.. {
            let token = self.credential.token().await?;
            let auth: String =
                form_urlencoded::byte_serialize(format!("type=aad&ver=1.0&sig={token}").as_bytes())
                    .collect();
            let mut req = build()
                .header("authorization", auth)
                .header("x-ms-version", API_VERSION)
                .header("x-ms-date", httpdate::fmt_http_date(SystemTime::now()));
            if let Some(pk) = pk {
                req = req.header("x-ms-documentdb-partitionkey", json!([pk]).to_string());
            }
            if let Some(session) = self.session_token(container) {
                req = req.header("x-ms-session-token", session);
            }
            let resp = req.send().await.context("Cosmos request")?;
            self.remember_session(container, &resp);
            if resp.status() != StatusCode::TOO_MANY_REQUESTS || attempt >= MAX_RETRIES {
                return Ok(resp);
            }
            let wait = resp
                .headers()
                .get("x-ms-retry-after-ms")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1000);
            tokio::time::sleep(Duration::from_millis(wait.min(30_000))).await;
        }
        unreachable!("the loop returns on its last attempt")
    }
}

fn etag_of(resp: &Response) -> String {
    resp.headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

async fn fail(op: &str, container: &str, resp: Response) -> anyhow::Error {
    let status = resp.status();
    // Cosmos error bodies carry a code and message, not item contents.
    let body = resp.text().await.unwrap_or_default();
    let body: String = body.chars().take(300).collect();
    anyhow::anyhow!("Cosmos {op} in `{container}` returned {status}: {body}")
}

/// Strip the system properties Cosmos adds, so items round-trip cleanly.
fn clean(mut doc: Value) -> (Value, String) {
    let etag = doc["_etag"].as_str().unwrap_or_default().to_owned();
    if let Some(o) = doc.as_object_mut() {
        o.retain(|k, _| !k.starts_with('_'));
    }
    (doc, etag)
}

#[async_trait]
impl DocStore for CosmosDocs {
    async fn get(&self, container: &str, pk: &str, id: &str) -> anyhow::Result<Option<Versioned>> {
        let url = self.docs_url(container, Some(id))?;
        let resp = self
            .send(
                container,
                || self.http.request(Method::GET, url.clone()),
                Some(pk),
            )
            .await?;
        match resp.status() {
            StatusCode::NOT_FOUND => Ok(None),
            StatusCode::OK => {
                let (doc, etag) = clean(resp.json().await?);
                Ok(Some(Versioned { doc, etag }))
            }
            _ => Err(fail("read", container, resp).await),
        }
    }

    async fn create(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
    ) -> anyhow::Result<Option<String>> {
        let url = self.docs_url(container, None)?;
        let resp = self
            .send(
                container,
                || self.http.post(url.clone()).json(doc),
                Some(pk),
            )
            .await?;
        match resp.status() {
            StatusCode::CREATED => Ok(Some(etag_of(&resp))),
            StatusCode::CONFLICT => Ok(None),
            _ => Err(fail("create", container, resp).await),
        }
    }

    async fn replace(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
        etag: &str,
    ) -> anyhow::Result<Option<String>> {
        let id = doc["id"].as_str().context("item has no `id`")?;
        let url = self.docs_url(container, Some(id))?;
        let resp = self
            .send(
                container,
                || {
                    self.http
                        .put(url.clone())
                        .header("if-match", etag)
                        .json(doc)
                },
                Some(pk),
            )
            .await?;
        match resp.status() {
            StatusCode::OK => Ok(Some(etag_of(&resp))),
            StatusCode::PRECONDITION_FAILED => Ok(None),
            _ => Err(fail("replace", container, resp).await),
        }
    }

    async fn upsert(&self, container: &str, pk: &str, doc: &Value) -> anyhow::Result<()> {
        let url = self.docs_url(container, None)?;
        let resp = self
            .send(
                container,
                || {
                    self.http
                        .post(url.clone())
                        .header("x-ms-documentdb-is-upsert", "True")
                        .json(doc)
                },
                Some(pk),
            )
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(fail("upsert", container, resp).await)
        }
    }

    async fn list(
        &self,
        container: &str,
        field: &str,
        values: &[&str],
    ) -> anyhow::Result<Vec<Versioned>> {
        anyhow::ensure!(
            field
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid field name `{field}`"
        );
        let body = if values.is_empty() {
            json!({"query": "SELECT * FROM c", "parameters": []})
        } else {
            json!({
                "query": format!("SELECT * FROM c WHERE ARRAY_CONTAINS(@values, c.{field})"),
                "parameters": [{"name": "@values", "value": values}],
            })
        };
        let url = self.docs_url(container, None)?;
        let mut out = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let cont = continuation.clone();
            let resp = self
                .send(
                    container,
                    || {
                        let mut r = self
                            .http
                            .post(url.clone())
                            .header("content-type", "application/query+json")
                            .header("x-ms-documentdb-isquery", "True")
                            .header("x-ms-documentdb-query-enablecrosspartition", "True")
                            .body(body.to_string());
                        if let Some(c) = &cont {
                            r = r.header("x-ms-continuation", c);
                        }
                        r
                    },
                    None,
                )
                .await?;
            if !resp.status().is_success() {
                return Err(fail("query", container, resp).await);
            }
            continuation = resp
                .headers()
                .get("x-ms-continuation")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let page: Value = resp.json().await?;
            for d in page["Documents"].as_array().cloned().unwrap_or_default() {
                let (doc, etag) = clean(d);
                out.push(Versioned { doc, etag });
            }
            if continuation.is_none() {
                break;
            }
        }
        out.sort_by(|a, b| a.doc["id"].as_str().cmp(&b.doc["id"].as_str()));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode as S};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use usnm_store::credential::StaticToken;

    /// A tiny Cosmos double: one container, ETags, the headers we send.
    #[derive(Default)]
    struct Fake {
        items: Mutex<Vec<(Value, String)>>,
        seen: Mutex<Vec<String>>,
    }

    type St = Arc<Fake>;

    fn check(h: &HeaderMap, seen: &Mutex<Vec<String>>) {
        let auth = h["authorization"].to_str().unwrap().to_owned();
        assert_eq!(auth, "type%3Daad%26ver%3D1.0%26sig%3Dtok");
        assert_eq!(h["x-ms-version"], API_VERSION);
        assert!(h.contains_key("x-ms-date"));
        seen.lock().unwrap().push(
            h.get("x-ms-session-token")
                .map(|v| format!("session={}", v.to_str().unwrap()))
                .unwrap_or_default(),
        );
        seen.lock().unwrap().push(
            h.get("x-ms-documentdb-partitionkey")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default(),
        );
    }

    async fn post_docs(State(s): State<St>, h: HeaderMap, body: String) -> (S, HeaderMap, String) {
        check(&h, &s.seen);
        let mut out = HeaderMap::new();
        let mut items = s.items.lock().unwrap();
        if h.get("x-ms-documentdb-isquery").is_some() {
            let docs: Vec<Value> = items
                .iter()
                .map(|(d, e)| {
                    let mut d = d.clone();
                    d["_etag"] = json!(e);
                    d["_rid"] = json!("x");
                    d
                })
                .collect();
            return (S::OK, out, json!({"Documents": docs}).to_string());
        }
        let doc: Value = serde_json::from_str(&body).unwrap();
        let upsert = h.get("x-ms-documentdb-is-upsert").is_some();
        let pos = items.iter().position(|(d, _)| d["id"] == doc["id"]);
        let etag = format!("\"e{}\"", items.len() + 10);
        out.insert("etag", etag.parse().unwrap());
        out.insert(
            "x-ms-session-token",
            format!("0:1#{}", items.len() + 1).parse().unwrap(),
        );
        match (pos, upsert) {
            (Some(_), false) => (S::CONFLICT, out, "{}".into()),
            (Some(i), true) => {
                items[i] = (doc, etag);
                (S::OK, out, "{}".into())
            }
            (None, _) => {
                items.push((doc, etag));
                (S::CREATED, out, "{}".into())
            }
        }
    }

    async fn put_doc(
        State(s): State<St>,
        Path((_, _, id)): Path<(String, String, String)>,
        h: HeaderMap,
        Json(doc): Json<Value>,
    ) -> (S, HeaderMap, String) {
        check(&h, &s.seen);
        let mut items = s.items.lock().unwrap();
        let mut out = HeaderMap::new();
        let Some(i) = items.iter().position(|(d, _)| d["id"] == id.as_str()) else {
            return (S::NOT_FOUND, out, "{}".into());
        };
        if h["if-match"].to_str().unwrap() != items[i].1 {
            return (S::PRECONDITION_FAILED, out, "{}".into());
        }
        let etag = format!("\"r{i}{}\"", items[i].1.len());
        items[i] = (doc, etag.clone());
        out.insert("etag", etag.parse().unwrap());
        (S::OK, out, "{}".into())
    }

    async fn get_doc(
        State(s): State<St>,
        Path((_, _, id)): Path<(String, String, String)>,
        h: HeaderMap,
    ) -> (S, String) {
        check(&h, &s.seen);
        let items = s.items.lock().unwrap();
        match items.iter().find(|(d, _)| d["id"] == id.as_str()) {
            Some((d, e)) => {
                let mut d = d.clone();
                d["_etag"] = json!(e);
                d["_ts"] = json!(1);
                (S::OK, d.to_string())
            }
            None => (S::NOT_FOUND, "{}".into()),
        }
    }

    #[tokio::test]
    async fn rest_round_trip() {
        let fake: St = Arc::default();
        let app = Router::new()
            .route("/dbs/usnm/colls/{c}/docs", post(post_docs))
            .route("/dbs/{db}/colls/{c}/docs/{id}", get(get_doc).put(put_doc))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let c = CosmosDocs::new(
            &format!("http://127.0.0.1:{}/", addr.port()),
            "usnm",
            Arc::new(Credential::new(StaticToken("tok".into()))),
        )
        .unwrap();
        let e1 = c
            .create(
                "batches",
                "b1",
                &json!({"id": "b1", "batch": "b1", "status": "queued"}),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(c
            .create("batches", "b1", &json!({"id": "b1"}))
            .await
            .unwrap()
            .is_none());
        let got = c.get("batches", "b1", "b1").await.unwrap().unwrap();
        assert_eq!(got.etag, e1);
        // System properties are stripped.
        assert!(got.doc.get("_ts").is_none());
        let e2 = c
            .replace(
                "batches",
                "b1",
                &json!({"id": "b1", "status": "curated"}),
                &e1,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(c
            .replace("batches", "b1", &json!({"id": "b1"}), &e1)
            .await
            .unwrap()
            .is_none());
        assert_ne!(e1, e2);
        assert!(c.get("batches", "b2", "missing").await.unwrap().is_none());
        c.upsert("batches", "b3", &json!({"id": "b3"}))
            .await
            .unwrap();
        let all = c.list("batches", "status", &["curated"]).await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].doc.get("_rid").is_none());
        let seen = fake.seen.lock().unwrap();
        assert!(seen.contains(&"[\"b1\"]".to_owned()));
        // Writes return session tokens; later requests echo the latest one.
        assert!(seen.contains(&"session=0:1#1".to_owned()), "{seen:?}");
        assert!(seen.contains(&"session=0:1#2".to_owned()), "{seen:?}");

        assert!(CosmosDocs::new(
            "https://evil.example/",
            "usnm",
            Arc::new(Credential::new(StaticToken("t".into())))
        )
        .is_err());
    }
}
