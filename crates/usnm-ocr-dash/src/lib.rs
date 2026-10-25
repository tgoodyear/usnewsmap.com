//! `usnm-ocrdash`: a read-only dashboard of the on-prem OCR pipeline's
//! local side (12 §12.2.5), for the LAN only. It reads the scan fetcher's
//! files, polls each worker's status, and takes the Proxmox hosts' stats,
//! and serves one HTML page and the same data as JSON. It never shows page
//! text: pages appear by doc_id, with counts, sizes, times and engine tags.

pub mod diagrams;
pub mod fetch;
pub mod hosts;
pub mod html;
pub mod net;
pub mod nodes;
pub mod pipeline;
pub mod public;
pub mod sset;
pub mod tail;
pub mod view;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use chrono::Utc;

use crate::fetch::Fetch;
use crate::hosts::Hosts;
use crate::net::Allow;
use crate::nodes::Nodes;
use crate::sset::Ssets;
use crate::view::{Settings, View};

pub struct App {
    pub pipeline: Mutex<pipeline::Pipeline>,
    pub fetch: Mutex<Fetch>,
    pub nodes: Mutex<Nodes>,
    pub hosts: Mutex<Hosts>,
    pub ssets: Mutex<Ssets>,
    pub seed_plan_path: Option<PathBuf>,
    pub seed_plan: Mutex<Option<BTreeMap<String, String>>>,
    pub settings: Settings,
    /// Who may ask anything.
    pub allow: Allow,
    /// Host → the addresses that may push its stats (its own).
    pub pushers: BTreeMap<String, Allow>,
    /// Serial Set fetch → the addresses that may push its status (the
    /// node that runs it).
    pub sset_pushers: BTreeMap<String, Allow>,
}

impl App {
    /// Read the files: the fetcher's, the hosts', the seed plan. Blocking.
    pub fn poll_files(&self) {
        let now = Utc::now();
        self.pipeline.lock().expect("pipeline lock").poll(now);
        self.fetch.lock().expect("fetch lock").poll(now);
        self.hosts.lock().expect("hosts lock").poll(now);
        if let Some(p) = &self.seed_plan_path {
            let plan = match std::fs::read(p) {
                Ok(b) => match view::read_seed_plan(&b) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        tracing::warn!(error = format!("{e:#}"), "can't read the seed plan");
                        None
                    }
                },
                Err(_) => None,
            };
            *self.seed_plan.lock().expect("seed lock") = plan;
        }
    }

    /// Ask every node, at once.
    pub async fn poll_nodes(self: &Arc<Self>, client: &reqwest::Client) {
        let targets: Vec<(usize, String, u64, String)> = {
            let n = self.nodes.lock().expect("nodes lock");
            n.nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (i, n.url.clone(), n.offset, n.journal_id.clone()))
                .collect()
        };
        let mut set = tokio::task::JoinSet::new();
        for (i, url, offset, id) in targets {
            let client = client.clone();
            set.spawn(async move { (i, Nodes::poll(&client, &url, offset, &id).await) });
        }
        while let Some(r) = set.join_next().await {
            if let Ok((i, r)) = r {
                self.nodes
                    .lock()
                    .expect("nodes lock")
                    .apply(i, r, Utc::now());
            }
        }
    }

    pub fn view(&self) -> View {
        let mut fetch = self.fetch.lock().expect("fetch lock");
        let nodes = self.nodes.lock().expect("nodes lock");
        let hosts = self.hosts.lock().expect("hosts lock");
        let ssets = self.ssets.lock().expect("ssets lock");
        let seed = self.seed_plan.lock().expect("seed lock").clone();
        let now = Utc::now();
        let mut v = view::build(
            &mut fetch,
            &nodes,
            &hosts,
            &ssets,
            seed,
            &self.settings,
            now,
        );
        v.pipeline = self.pipeline.lock().expect("pipeline lock").view(now);
        v
    }
}

async fn allow_only(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = peer.ip().to_canonical();
    if app.allow.contains(ip) {
        next.run(req).await
    } else {
        tracing::info!(peer = %ip, path = req.uri().path(), "refused: not on --allow");
        StatusCode::FORBIDDEN.into_response()
    }
}

/// Runs the blocking view work off the async threads.
async fn view_of(app: Arc<App>) -> Result<View, StatusCode> {
    tokio::task::spawn_blocking(move || app.view())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// The LAN's headers: nothing cached.
const LAN_CACHE: &str = "no-store";
/// The public listener's: Cloudflare may keep a page for a minute.
const PUBLIC_CACHE: &str = "public, max-age=60";

async fn view_for(app: Arc<App>, public: bool) -> Result<View, StatusCode> {
    let mut v = view_of(app).await?;
    if public {
        public::redact(&mut v);
    }
    Ok(v)
}

async fn page_as(app: Arc<App>, public: bool) -> Response {
    let cache = if public { PUBLIC_CACHE } else { LAN_CACHE };
    match view_for(app, public).await {
        Ok(v) => ([(header::CACHE_CONTROL, cache)], Html(html::render(&v))).into_response(),
        Err(s) => s.into_response(),
    }
}

async fn page(State(app): State<Arc<App>>) -> Response {
    page_as(app, false).await
}

async fn public_page(State(app): State<Arc<App>>) -> Response {
    page_as(app, true).await
}

/// The JSON parts the public listener serves.
pub const PUBLIC_PARTS: &[&str] = &["view", "pipeline", "fetch"];

async fn api_as(app: Arc<App>, part: &str, public: bool) -> Response {
    let Some(part) = part.strip_suffix(".json") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if public && !PUBLIC_PARTS.contains(&part) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let v = match view_for(app, public).await {
        Ok(v) => v,
        Err(s) => return s.into_response(),
    };
    let body = match part {
        "view" => serde_json::to_value(&v),
        "pipeline" => serde_json::to_value(&v.pipeline),
        "hosts" => serde_json::to_value(&v.hosts),
        "fetch" => serde_json::to_value(&v.fetch),
        "nodes" => serde_json::to_value(&v.nodes),
        "serialset" => serde_json::to_value(&v.serial_set),
        "next" => serde_json::to_value(&v.next),
        "ocred" => serde_json::to_value(&v.ocred),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let cache = if public { PUBLIC_CACHE } else { LAN_CACHE };
    match body {
        Ok(b) => ([(header::CACHE_CONTROL, cache)], Json(b)).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn api(State(app): State<Arc<App>>, Path(part): Path<String>) -> Response {
    api_as(app, &part, false).await
}

async fn public_api(State(app): State<Arc<App>>, Path(part): Path<String>) -> Response {
    api_as(app, &part, true).await
}

/// A host pushes its stats: only from its own address, and only its own.
async fn put_host(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    let ip = peer.ip().to_canonical();
    if !app.pushers.get(&name).is_some_and(|a| a.contains(ip)) {
        tracing::info!(peer = %ip, host = name, "refused a host push");
        return StatusCode::FORBIDDEN.into_response();
    }
    match hosts::parse(&body, Some(&name)) {
        Ok(s) => {
            app.hosts.lock().expect("hosts lock").put(s, Utc::now());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}\n")).into_response(),
    }
}

/// The node running the Serial Set fetch pushes its progress: only from
/// its own address.
async fn put_sset(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    let ip = peer.ip().to_canonical();
    if !app.sset_pushers.get(&name).is_some_and(|a| a.contains(ip)) {
        tracing::info!(peer = %ip, sset = name, "refused a Serial Set push");
        return StatusCode::FORBIDDEN.into_response();
    }
    match sset::parse(&body) {
        Ok(s) => {
            app.ssets
                .lock()
                .expect("ssets lock")
                .put(&name, s, Utc::now());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}\n")).into_response(),
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/api/{part}", get(api))
        .route("/api/host/{name}", put(put_host))
        .route("/api/sset/{name}", put(put_sset))
        .layer(DefaultBodyLimit::max(hosts::MAX_HOST_BYTES))
        .layer(middleware::from_fn_with_state(app.clone(), allow_only))
        .with_state(app)
}

/// Every public response says how long it may be kept: a minute for a
/// success, not at all for an error (a 404, a 405, a 500).
async fn public_cache(mut r: Response) -> Response {
    let v = if r.status().is_success() {
        PUBLIC_CACHE
    } else {
        LAN_CACHE
    };
    r.headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static(v));
    r
}

/// The public listener (`--public-listen`), for a tunnel to put on the
/// internet: GET and HEAD of the page, `/healthz` and
/// `/api/{view,pipeline,fetch}.json`, all redacted ([`public::redact`]).
/// No push routes, no `--allow` check (only the tunnel reaches it), no
/// TLS. Anything else is a 404, or a 405 for another method on a route.
pub fn public_router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(public_page))
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/api/{part}", get(public_api))
        .layer(DefaultBodyLimit::max(1024))
        .layer(middleware::map_response(public_cache))
        .with_state(app)
}

/// Plain HTTP when the page itself is served over HTTPS: every request is
/// redirected to `https://<authority>` (the name, and the port unless 443)
/// with the same path and query, except
/// the hosts' stats pushes (`PUT /api/host/{name}`) and the Serial Set
/// status push (`PUT /api/sset/{name}`), which carry no secret,
/// are checked by address, and come from `curl` that doesn't follow
/// redirects. The same `--allow` as the page.
pub fn redirect_router(app: Arc<App>, authority: &str) -> Router {
    let base = format!("https://{authority}");
    let redirect = move |uri: Uri| async move {
        let path = uri.path_and_query().map_or("/", |p| p.as_str());
        Redirect::permanent(&format!("{base}{path}"))
    };
    Router::new()
        // Any other method on the push path is redirected too, not a 405.
        .route("/api/host/{name}", put(put_host).fallback(redirect.clone()))
        .route("/api/sset/{name}", put(put_sset).fallback(redirect.clone()))
        .fallback(redirect)
        .layer(DefaultBodyLimit::max(hosts::MAX_HOST_BYTES))
        .layer(middleware::from_fn_with_state(app.clone(), allow_only))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn app(dir: &std::path::Path) -> Arc<App> {
        Arc::new(App {
            pipeline: Mutex::new(pipeline::Pipeline::new(None, 900)),
            fetch: Mutex::new(Fetch::new(dir.to_owned(), None)),
            nodes: Mutex::new(Nodes::open(dir.join("state"), &[]).unwrap()),
            hosts: Mutex::new(Hosts::new(Vec::new())),
            ssets: Mutex::new(Ssets::default()),
            seed_plan_path: None,
            seed_plan: Mutex::new(None),
            settings: Settings {
                next_n: 10,
                refresh_secs: 30,
                guest_prefix: "usnm-".into(),
                workbook_url: None,
            },
            allow: Allow::new(vec!["198.51.100.0/24".parse().unwrap()]),
            pushers: [(
                "pve".to_owned(),
                Allow::new(vec!["198.51.100.12".parse().unwrap()]),
            )]
            .into(),
            sset_pushers: [(
                "sset".to_owned(),
                Allow::new(vec!["198.51.100.13".parse().unwrap()]),
            )]
            .into(),
        })
    }

    #[tokio::test]
    async fn the_page_refreshes_in_the_background_without_a_reload() {
        let dir = std::env::temp_dir().join(format!("usnm-dash-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (_, body) = call(app(&dir), "198.51.100.50", "GET", "/", "").await;
        assert!(body.contains("<body data-refresh=\"30\">"));
        assert!(body.contains("fetch(location.pathname"));
        assert!(body.contains("setInterval(update, every)"));
        // Without script the page still reloads itself, and only then.
        let meta = "<meta http-equiv=\"refresh\" content=\"30\">";
        assert_eq!(body.matches(meta).count(), 1);
        assert!(body.contains(&format!("<noscript>{meta}</noscript>")));
        std::fs::remove_dir_all(&dir).ok();
    }

    async fn call(
        app: Arc<App>,
        from: &str,
        method: &str,
        uri: &str,
        body: &str,
    ) -> (StatusCode, String) {
        call_on(router(app), from, method, uri, body).await.0
    }

    #[tokio::test]
    async fn pipeline_api_is_read_only_and_matches_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let a = app(dir.path());
        let now = Utc::now();
        let path = dir.path().join("pipeline.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&pipeline::tests::fixture(now)).unwrap(),
        )
        .unwrap();
        *a.pipeline.lock().unwrap() = pipeline::Pipeline::new(Some(path), 900);
        a.poll_files();
        let (status, body) =
            call(a.clone(), "198.51.100.50", "GET", "/api/pipeline.json", "").await;
        assert_eq!(status, StatusCode::OK);
        let mut p: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(p["stale"], false);
        assert_eq!(p["snapshot"]["stages"][0]["count"], 1234);
        assert!(p["retrieved_at"].is_string());
        let (_, view) = call(a.clone(), "198.51.100.50", "GET", "/api/view.json", "").await;
        let mut view: serde_json::Value = serde_json::from_str(&view).unwrap();
        for pipeline in [&mut p, &mut view["pipeline"]] {
            pipeline.as_object_mut().unwrap().remove("age_secs");
            for stage in pipeline["stage_freshness"].as_array_mut().unwrap() {
                stage.as_object_mut().unwrap().remove("age_secs");
            }
        }
        assert_eq!(view["pipeline"], p);
        assert_eq!(
            call(
                a.clone(),
                "198.51.100.50",
                "PUT",
                "/api/pipeline.json",
                "{}"
            )
            .await
            .0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            call(a, "203.0.113.3", "GET", "/api/pipeline.json", "")
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }

    async fn call_on(
        r: Router,
        from: &str,
        method: &str,
        uri: &str,
        body: &str,
    ) -> ((StatusCode, String), Option<String>) {
        let peer: SocketAddr = format!("{from}:40000").parse().unwrap();
        let r = r
            .layer(MockConnectInfo(peer))
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status();
        let location = r
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_owned());
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        ((status, String::from_utf8_lossy(&b).into_owned()), location)
    }

    #[tokio::test]
    async fn plain_http_redirects_to_https_but_takes_host_pushes() {
        let dir = tempfile::tempdir().unwrap();
        let a = app(dir.path());
        let plain = || redirect_router(a.clone(), "ocr.example.org");
        let ((s, _), to) = call_on(plain(), "198.51.100.50", "GET", "/", "").await;
        assert_eq!(s, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(to.as_deref(), Some("https://ocr.example.org/"));
        let ((s, _), to) =
            call_on(plain(), "198.51.100.50", "GET", "/api/fetch.json?x=1", "").await;
        assert_eq!(s, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            to.as_deref(),
            Some("https://ocr.example.org/api/fetch.json?x=1")
        );
        let ((s, _), to) = call_on(plain(), "198.51.100.50", "GET", "/api/host/pve", "").await;
        assert_eq!(s, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(to.as_deref(), Some("https://ocr.example.org/api/host/pve"));
        let ((s, _), _) = call_on(plain(), "203.0.113.3", "GET", "/", "").await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let stats = r#"{"schema":1,"host":"pve","pools":[]}"#;
        let ((s, _), _) = call_on(plain(), "198.51.100.12", "PUT", "/api/host/pve", stats).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let ((s, _), _) = call_on(plain(), "198.51.100.50", "PUT", "/api/host/pve", stats).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn serves_the_lan_only_and_takes_a_host_push_from_that_host_only() {
        let dir = tempfile::tempdir().unwrap();
        let a = app(dir.path());
        let (s, body) = call(a.clone(), "198.51.100.50", "GET", "/", "").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("Scan downloads"));
        assert_eq!(
            call(a.clone(), "203.0.113.3", "GET", "/", "").await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(a.clone(), "198.51.100.50", "GET", "/api/fetch.json", "")
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            call(a.clone(), "198.51.100.50", "GET", "/api/other.json", "")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let stats = r#"{"schema":1,"host":"pve","pools":[{"name":"<script>","health":"ONLINE"}]}"#;
        assert_eq!(
            call(a.clone(), "198.51.100.50", "PUT", "/api/host/pve", stats)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(a.clone(), "198.51.100.12", "PUT", "/api/host/pve", stats)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(
                a.clone(),
                "198.51.100.12",
                "PUT",
                "/api/host/pve",
                r#"{"host":"pve-big"}"#
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let (_, body) = call(a.clone(), "198.51.100.50", "GET", "/", "").await;
        assert!(body.contains("&lt;script&gt;"));
        assert!(!body.contains("<code><script>"));
        let (_, json) = call(a.clone(), "198.51.100.50", "GET", "/api/hosts.json", "").await;
        assert!(json.contains("\"name\":\"pve\""));
        let sset = r#"{"unit":"usnm-sset.service","active":"active","to_fetch":900,"curated":120}"#;
        let put_sset =
            |from: &'static str, name: &'static str| call(a.clone(), from, "PUT", name, sset);
        assert_eq!(
            put_sset("198.51.100.50", "/api/sset/sset").await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            put_sset("198.51.100.13", "/api/sset/other").await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            put_sset("198.51.100.13", "/api/sset/sset").await.0,
            StatusCode::NO_CONTENT
        );
        let (_, json) = call(a.clone(), "198.51.100.50", "GET", "/api/serialset.json", "").await;
        assert!(json.contains("\"curated\":120"));
        let (_, page) = call(a, "198.51.100.50", "GET", "/", "").await;
        assert!(page.contains("Serial Set"));
    }

    /// The lab's names, numbers, paths and addresses, as the LAN sees them.
    fn realistic(dir: &std::path::Path) -> Arc<App> {
        use usnm_ocr_contract::status::{
            Completion, Dataset, DeadLetterView, HostStats, Pool, PveGuest, QueueStats,
            WorkerStatus,
        };
        let now = Utc::now();
        let at = (now - chrono::Duration::minutes(2)).to_rfc3339();
        let scans = dir.join("scans");
        std::fs::create_dir_all(&scans).unwrap();
        std::fs::write(
            scans.join("status-full.json"),
            format!(
                "{{\"state\":\"running\",\"updated_at\":\"{at}\",\"started_at\":\"{at}\",\
                 \"manifest\":\"/tank/ocr/manifests/queue-ocr-20261025t015125/manifest.parquet\",\
                 \"out\":\"/tank/ocr/scans\",\"pid\":4242,\
                 \"error\":\"can't write /var/lib/usnm/x on pve-big\"}}"
            ),
        )
        .unwrap();
        std::fs::write(
            scans.join("failures-full.jsonl"),
            format!(
                "{{\"doc_id\":\"sn1_1890-01-01_ed-1_seq-1\",\"kind\":\"transient\",\
                 \"error\":\"http://192.0.2.10:8390/scan: refused (CT 212, /srv/usnm/cache)\",\"at\":\"{at}\"}}\n"
            ),
        )
        .unwrap();
        let mut workbook =
            "https://portal.azure.com/#@00000000-0000-0000-0000-000000000001".to_owned();
        workbook.push_str("/resource/subscriptions/00000000-0000-0000-0000-000000000002/workbook");
        let a = Arc::new(App {
            pipeline: Mutex::new(pipeline::Pipeline::new(None, 900)),
            fetch: Mutex::new(Fetch::new(
                scans,
                Some(dir.join("etc-manifest").join("manifest.parquet")),
            )),
            nodes: Mutex::new(
                Nodes::open(
                    dir.join("state"),
                    &[(
                        "usnm-ocr-node3".to_owned(),
                        "http://192.0.2.10:8381".to_owned(),
                    )],
                )
                .unwrap(),
            ),
            hosts: Mutex::new(Hosts::new(Vec::new())),
            ssets: Mutex::new(Ssets::default()),
            seed_plan_path: None,
            seed_plan: Mutex::new(Some(
                [("pve-big".to_owned(), "/etc/usnm/seed.env".to_owned())].into(),
            )),
            settings: Settings {
                next_n: 10,
                refresh_secs: 30,
                guest_prefix: "usnm-".into(),
                workbook_url: Some(workbook),
            },
            allow: Allow::new(vec!["198.51.100.0/24".parse().unwrap()]),
            pushers: BTreeMap::new(),
            sset_pushers: BTreeMap::new(),
        });
        let guest = |vmid, kind: &str, name: &str, node: &str| PveGuest {
            vmid,
            kind: kind.into(),
            name: name.into(),
            node: node.into(),
            status: "running".into(),
            ..PveGuest::default()
        };
        {
            let mut h = a.hosts.lock().unwrap();
            h.put(
                HostStats {
                    schema: 1,
                    host: "pve-big".into(),
                    collected_at: now,
                    resources: vec![
                        guest(212, "lxc", "usnm-ocrdash", "pve-big"),
                        guest(215, "lxc", "usnm-release", "pve-big"),
                        guest(205, "qemu", "usnm-gpu1", "pve-big"),
                    ],
                    pools: vec![Pool {
                        name: "tank".into(),
                        health: "ONLINE".into(),
                        status_x: "pool 'tank' is healthy".into(),
                        ..Pool::default()
                    }],
                    datasets: vec![Dataset {
                        name: "tank/ocr/scans".into(),
                        ..Dataset::default()
                    }],
                    ..HostStats::default()
                },
                now,
            );
            h.put(
                HostStats {
                    schema: 1,
                    host: "pve".into(),
                    collected_at: now,
                    resources: vec![guest(230, "lxc", "usnm-boinc", "pve")],
                    ..HostStats::default()
                },
                now,
            );
        }
        {
            let mut n = a.nodes.lock().unwrap();
            let node = &mut n.nodes[0];
            node.last_ok = Some(now);
            node.status = Some(WorkerStatus {
                node_id: "usnm-ocr-node3".into(),
                started_at: now,
                now,
                scan_cache_url: Some("http://pve-big.tjg.fyi:8390".into()),
                routes: [("scan".to_owned(), "http://192.0.2.11:8390".to_owned())].into(),
                recent: vec![Completion {
                    doc_id: "sn1_1890-01-01_ed-1_seq-1".into(),
                    outcome: "done".into(),
                    scan: "/tank/ocr/scans/sn1.jp2".into(),
                    finished_at: now,
                    started_at: now,
                    ..Completion::default()
                }],
                queues: Some(QueueStats {
                    checked_at: now,
                    dead_letters: vec![DeadLetterView {
                        message_id: "m1".into(),
                        node: "usnm-ocr-node3".into(),
                        error: "timed out reaching ocr.tjg.fyi".into(),
                        ..DeadLetterView::default()
                    }],
                    ..QueueStats::default()
                }),
                ..WorkerStatus::default()
            });
        }
        a.ssets.lock().unwrap().put(
            "usnm-ocr-node4",
            sset::SsetStatus {
                unit: "usnm-sset.service".into(),
                active: "active".into(),
                current_volume: Some("/srv/usnm/sset/vol-1".into()),
                ..sset::SsetStatus::default()
            },
            now,
        );
        let mut snap = pipeline::tests::fixture(now);
        snap["stages"][0]["id"] = "local_usnm-ocr-node3_completed_hour".into();
        snap["stages"][0]["detail"] = "read /data/scans/status-full.json on pve-big".into();
        snap["issues"][0]["code"] = "usnm-ocr-node3-stale".into();
        snap["issues"][0]["detail"] =
            "tank/ocr/scans/index-full.jsonl from [2001:db8::7]:8381".into();
        let snap_path = dir.join("pipeline.json");
        std::fs::write(&snap_path, serde_json::to_vec(&snap).unwrap()).unwrap();
        *a.pipeline.lock().unwrap() = pipeline::Pipeline::new(Some(snap_path), 900);
        a.poll_files();
        a
    }

    /// What must never reach the internet.
    fn leaks(body: &str) -> Vec<String> {
        let mut found: Vec<String> = [
            "pve",
            "tjg.fyi",
            "CT 2",
            "VM 2",
            "usnm-ocr-node",
            "/tank",
            "tank/",
            "/var/",
            "/srv/",
            "/etc/",
            "/tmp/",
            "portal.azure",
            "subscriptions",
            "192.0.2.",
            "198.51.100.",
            "usnm-ocrdash",
            "usnm-release",
            "usnm-sset.service",
            "seed.env",
        ]
        .iter()
        .filter(|p| body.contains(*p))
        .map(|p| (*p).to_owned())
        .collect();
        // Any dotted quad at all.
        let b = body.as_bytes();
        for i in 0..b.len() {
            if !body.is_char_boundary(i)
                || (i > 0 && (b[i - 1].is_ascii_digit() || b[i - 1] == b'.'))
            {
                continue;
            }
            let rest = &body[i..];
            let quad: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let parts: Vec<&str> = quad.trim_end_matches('.').split('.').collect();
            if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
                found.push(quad);
            }
        }
        found
    }

    #[tokio::test]
    async fn the_public_view_names_no_host_container_path_or_address() {
        let dir = tempfile::tempdir().unwrap();
        let a = realistic(dir.path());
        // The fixture has them all on the LAN.
        let (s, lan) = call(a.clone(), "198.51.100.50", "GET", "/", "").await;
        assert_eq!(s, StatusCode::OK);
        for p in [
            "pve-big",
            "CT 212",
            "/tank",
            "portal.azure",
            "usnm-ocr-node3",
        ] {
            assert!(lan.contains(p), "the LAN page lacks {p}");
        }
        for uri in [
            "/",
            "/api/view.json",
            "/api/pipeline.json",
            "/api/fetch.json",
        ] {
            let ((s, body), _) =
                call_on(public_router(a.clone()), "203.0.113.3", "GET", uri, "").await;
            assert_eq!(s, StatusCode::OK, "{uri}");
            let found = leaks(&body);
            assert!(found.is_empty(), "{uri} shows {found:?}");
        }
        let ((_, page), _) = call_on(public_router(a.clone()), "203.0.113.3", "GET", "/", "").await;
        assert!(page.contains("scan server"));
        assert!(page.contains("queue-ocr-20261025t015125"));
        assert!(page.contains("OCR node 1"));
        let ((_, p), _) = call_on(
            public_router(a.clone()),
            "203.0.113.3",
            "GET",
            "/api/pipeline.json",
            "",
        )
        .await;
        assert!(p.contains("local_worker1_completed_hour"), "{p}");
    }

    #[tokio::test]
    async fn the_public_listener_serves_reads_only_and_lets_them_be_cached() {
        let dir = tempfile::tempdir().unwrap();
        let a = app(dir.path());
        let send = |method: &str, uri: &str| {
            let r = public_router(a.clone()).layer(MockConnectInfo(
                "203.0.113.3:40000".parse::<SocketAddr>().unwrap(),
            ));
            let req = HttpRequest::builder()
                .method(method)
                .uri(uri)
                .body(Body::from("{}"))
                .unwrap();
            async move { r.oneshot(req).await.unwrap() }
        };
        for uri in [
            "/",
            "/api/view.json",
            "/api/pipeline.json",
            "/api/fetch.json",
        ] {
            for m in ["GET", "HEAD"] {
                let r = send(m, uri).await;
                assert_eq!(r.status(), StatusCode::OK, "{m} {uri}");
                assert_eq!(
                    r.headers()[header::CACHE_CONTROL],
                    "public, max-age=60",
                    "{m} {uri}"
                );
            }
        }
        for (m, uri, want) in [
            ("PUT", "/api/host/pve", StatusCode::NOT_FOUND),
            ("PUT", "/api/sset/sset", StatusCode::NOT_FOUND),
            ("GET", "/api/host/pve", StatusCode::NOT_FOUND),
            ("GET", "/api/hosts.json", StatusCode::NOT_FOUND),
            ("GET", "/api/nodes.json", StatusCode::NOT_FOUND),
            ("GET", "/api/next.json", StatusCode::NOT_FOUND),
            ("POST", "/", StatusCode::METHOD_NOT_ALLOWED),
            ("PUT", "/api/view.json", StatusCode::METHOD_NOT_ALLOWED),
            ("DELETE", "/api/fetch.json", StatusCode::METHOD_NOT_ALLOWED),
        ] {
            let r = send(m, uri).await;
            assert_eq!(r.status(), want, "{m} {uri}");
            assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store", "{m} {uri}");
        }
        // The LAN listener keeps its headers.
        let r = router(a.clone())
            .layer(MockConnectInfo(
                "198.51.100.50:40000".parse::<SocketAddr>().unwrap(),
            ))
            .oneshot(HttpRequest::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    }
}
