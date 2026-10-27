//! Durable sitemap generations. The search log store is create-only and has no
//! expiry; response-cache blobs deliberately are NOT used for these dates.

use std::collections::BTreeMap;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usnm_store::ObjectStore;

use crate::{AppState, Snapshot};

const PREFIX: &str = "sitemap/state";
const STATIC: [&str; 4] = ["/", "/privacy", "/credits", "/datasets"];

#[derive(Default)]
pub(crate) struct Service {
    committed: ArcSwapOption<Checkpoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Page {
    fingerprint: String,
    lastmod: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Checkpoint {
    sequence: u64,
    version: String,
    published_at: DateTime<Utc>,
    build_at: DateTime<Utc>,
    web_at: DateTime<Utc>,
    renderer: String,
    observed_at: DateTime<Utc>,
    pages: BTreeMap<String, Page>,
}

#[derive(Deserialize)]
struct Manifest {
    generated_at: DateTime<Utc>,
    pages: BTreeMap<String, String>,
}

fn build_at() -> DateTime<Utc> {
    DateTime::from_timestamp_micros(
        env!("USNM_SITEMAP_BUILD_MICROS")
            .parse()
            .expect("build epoch"),
    )
    .expect("valid build timestamp")
}

fn fingerprint(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) fn renderer() -> &'static str {
    static HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        fingerprint(format!(
            "{}\0{}",
            include_str!("../site.rs"),
            env!("USNM_SITEMAP_BUILD_MICROS")
        ))
    });
    &HASH
}

fn backend(error: impl std::fmt::Display) -> crate::error::ApiError {
    crate::error::ApiError::Backend(format!("sitemap: {error}"))
}

type Result<T> = std::result::Result<T, crate::error::ApiError>;

async fn latest(store: &dyn ObjectStore) -> Result<Option<Checkpoint>> {
    let paths = store.list(PREFIX).await.map_err(backend)?;
    let Some(path) = paths.iter().filter(|path| sequence(path).is_some()).max() else {
        return Ok(None);
    };
    let bytes = store
        .get(path)
        .await
        .map_err(backend)?
        .ok_or_else(|| backend("checkpoint disappeared"))?;
    let checkpoint: Checkpoint = serde_json::from_slice(&bytes).map_err(backend)?;
    if Some(checkpoint.sequence) != sequence(path) || checkpoint.pages.is_empty() {
        return Err(backend("invalid checkpoint"));
    }
    Ok(Some(checkpoint))
}

fn sequence(path: &str) -> Option<u64> {
    let n = path.strip_prefix("sitemap/state/")?.strip_suffix(".json")?;
    (n.len() == 20 && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
}

struct Generation {
    version: String,
    published_at: DateTime<Utc>,
    build_at: DateTime<Utc>,
    web_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
    pages: BTreeMap<String, String>,
}

impl Generation {
    fn check_freshness(&self, previous: Option<&Checkpoint>) -> Result<()> {
        if previous.is_some_and(|p| {
            self.build_at < p.build_at || self.web_at < p.web_at || self.observed_at < p.observed_at
        }) {
            return Err(backend(
                "superseded build or observation; rebuild an older image to roll back",
            ));
        }
        Ok(())
    }

    fn checkpoint(&self, previous: Option<&Checkpoint>) -> Result<Checkpoint> {
        self.check_freshness(previous)?;
        let pages = self
            .pages
            .iter()
            .map(|(path, fingerprint)| {
                let old = previous.and_then(|p| p.pages.get(path));
                let lastmod = match old {
                    Some(old) if &old.fingerprint == fingerprint => old.lastmod,
                    // Publication explains changed results only when it is a
                    // new publication, not a code-only change or bootstrap.
                    Some(old)
                        if previous.is_some_and(|p| p.version != self.version)
                            && self.published_at > old.lastmod
                            && (path.starts_with("/search/") || path == "/datasets") =>
                    {
                        self.published_at
                    }
                    _ => self
                        .observed_at
                        .max(old.map_or(self.observed_at, |p| p.lastmod)),
                };
                (
                    path.clone(),
                    Page {
                        fingerprint: fingerprint.clone(),
                        lastmod,
                    },
                )
            })
            .collect();
        Ok(Checkpoint {
            sequence: previous.map_or(1, |p| p.sequence + 1),
            version: self.version.clone(),
            published_at: self.published_at,
            build_at: self.build_at,
            web_at: self.web_at,
            renderer: renderer().to_owned(),
            observed_at: self.observed_at,
            pages,
        })
    }
}

async fn still_current(state: &AppState, snap: &Arc<Snapshot>) -> Result<()> {
    if !Arc::ptr_eq(&state.snapshot.load_full(), snap) {
        return Err(backend("snapshot changed during refresh"));
    }
    if let Some(loader) = &state.loader {
        let current = loader
            .reference
            .get("current.json")
            .await
            .map_err(backend)?
            .ok_or_else(|| backend("published pointer missing"))?;
        let published: crate::refdata::Current =
            serde_json::from_slice(&current).map_err(backend)?;
        if serde_json::to_value(published).map_err(backend)?
            != serde_json::to_value(&snap.refdata.current).map_err(backend)?
        {
            return Err(backend("published pointer changed during refresh"));
        }
    }
    if !Arc::ptr_eq(&state.snapshot.load_full(), snap) {
        return Err(backend("snapshot changed while validating publication"));
    }
    Ok(())
}

async fn commit(
    state: &AppState,
    snap: &Arc<Snapshot>,
    store: &dyn ObjectStore,
    generation: &Generation,
) -> Result<Checkpoint> {
    // A competing replica's immutable append is our compare-and-swap failure.
    // Re-read and re-compare fingerprints/dates, never overwrite its checkpoint.
    for _ in 0..16 {
        let previous = latest(store).await?;
        let next = generation.checkpoint(previous.as_ref())?;
        still_current(state, snap).await?;
        let path = format!("{PREFIX}/{:020}.json", next.sequence);
        if store
            .put_new(
                &path,
                serde_json::to_vec(&next).map_err(backend)?,
                "application/json",
            )
            .await
            .map_err(backend)?
        {
            return Ok(next);
        }
    }
    Err(backend("checkpoint append contention; retry next refresh"))
}

async fn generate(
    state: &Arc<AppState>,
    snap: Arc<Snapshot>,
    manifest: &Manifest,
) -> Result<Generation> {
    let observed_at = Utc::now();
    let published_at = DateTime::parse_from_rfc3339(&snap.refdata.current.published_at)
        .map_err(backend)?
        .with_timezone(&Utc);
    if published_at > observed_at || build_at() > observed_at || manifest.generated_at > observed_at
    {
        return Err(backend("future publication/build timestamp"));
    }
    if manifest.pages.len() != STATIC.len()
        || STATIC.iter().any(|p| !manifest.pages.contains_key(*p))
    {
        return Err(backend("incomplete static content manifest"));
    }
    let mut generation = Generation {
        version: snap.refdata.version().to_owned(),
        published_at,
        build_at: build_at(),
        web_at: manifest.generated_at,
        observed_at,
        pages: BTreeMap::new(),
    };
    generation.check_freshness(state.sitemap.committed.load_full().as_deref())?;
    let mut pages = manifest.pages.clone();
    let shell = b"<title>US News Map</title><link rel=\"canonical\" href=\"https://usnewsmap.com/\" /><meta property=\"og:title\" content=\"US News Map\" /><meta property=\"og:url\" content=\"https://usnewsmap.com/\" />";
    for (path, content) in &mut pages {
        *content = fingerprint(format!(
            "{content}\0{}",
            fingerprint(super::shell_for(path, shell.to_vec()))
        ));
    }
    // The dataset's published content, not live queue/health/timing counters.
    let provenance = serde_json::to_vec(&snap.refdata.current).map_err(backend)?;
    let dataset = pages.get_mut("/datasets").expect("validated manifest");
    *dataset = fingerprint(format!("{dataset}\0{}", fingerprint(provenance)));
    let mut paths: Vec<_> = super::CURATED_SEARCHES.iter().collect();
    paths.sort();
    for path in paths {
        if !Arc::ptr_eq(&state.snapshot.load_full(), &snap) {
            return Err(backend("snapshot changed during refresh"));
        }
        let query = super::search_query(path).expect("curated path");
        let html = super::search_preview_snapshot(state, snap.clone(), &query, true).await?;
        pages.insert(path.clone(), fingerprint(html));
        tokio::task::yield_now().await;
    }
    generation.pages = pages;
    Ok(generation)
}

pub(crate) fn spawn(state: Arc<AppState>) {
    let (Some(url), Some(dir)) = (
        state.config.sitemap_state_url().map(str::to_owned),
        state.config.site_dir.clone(),
    ) else {
        return;
    };
    tokio::spawn(async move {
        let mut finished = None;
        loop {
            let refresh = async {
                let store = usnm_store::open(&url).map_err(backend)?;
                if let Some(checkpoint) = latest(store.as_ref()).await? {
                    state.sitemap.committed.store(Some(Arc::new(checkpoint)));
                }
                let bytes = tokio::fs::read(dir.join("sitemap-pages.json"))
                    .await
                    .map_err(backend)?;
                let manifest: Manifest = serde_json::from_slice(&bytes).map_err(backend)?;
                let snap = state.snapshot.load_full();
                let identity = (snap.refdata.version().to_owned(), manifest.generated_at);
                if finished.as_ref() == Some(&identity) {
                    return Ok::<_, crate::error::ApiError>(());
                }
                let generation = generate(&state, snap.clone(), &manifest).await?;
                let checkpoint = commit(&state, &snap, store.as_ref(), &generation).await?;
                tracing::info!(
                    sequence = checkpoint.sequence,
                    pages = checkpoint.pages.len(),
                    "sitemap generation committed"
                );
                state.sitemap.committed.store(Some(Arc::new(checkpoint)));
                finished = Some(identity);
                Ok(())
            };
            if let Err(error) = refresh.await {
                tracing::warn!(?error, "sitemap refresh failed; retaining committed dates");
            }
            tokio::time::sleep(
                state
                    .config
                    .refresh_interval
                    .max(std::time::Duration::from_secs(5)),
            )
            .await;
        }
    });
}

pub(super) fn response(state: &AppState, req: &Request) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(checkpoint) = state.sitemap.committed.load_full() else {
        tracing::warn!("sitemap awaiting first complete durable generation");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                (header::RETRY_AFTER, "30"),
                (header::CACHE_CONTROL, "no-store"),
            ],
        )
            .into_response();
    };
    let body = xml(&checkpoint);
    let length = body.len().to_string();
    let headers = [
        (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
        (header::CACHE_CONTROL, "no-cache"),
        (header::CONTENT_LENGTH, length.as_str()),
    ];
    (
        headers,
        if req.method() == Method::HEAD {
            String::new()
        } else {
            body
        },
    )
        .into_response()
}

fn xml(checkpoint: &Checkpoint) -> String {
    let mut xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n".to_owned();
    for (path, page) in &checkpoint.pages {
        xml.push_str(&format!(
            "  <url><loc>https://usnewsmap.com{}</loc><lastmod>{}</lastmod></url>\n",
            super::escape_html(path),
            page.lastmod
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ));
    }
    xml.push_str("</urlset>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use usnm_search::memory::MemoryBackend;
    use usnm_store::LocalStore;

    fn at(day: u32) -> DateTime<Utc> {
        format!("2026-10-{day:02}T00:00:00Z").parse().unwrap()
    }

    fn generation() -> Generation {
        Generation {
            version: "one".into(),
            published_at: at(1),
            build_at: at(1),
            web_at: at(1),
            observed_at: at(2),
            pages: BTreeMap::from([
                ("/privacy".into(), "privacy".into()),
                ("/datasets".into(), "data".into()),
                ("/search/whigs".into(), "results".into()),
                ("/search/no-results".into(), "empty".into()),
            ]),
        }
    }

    #[test]
    fn dates_follow_content_not_rebuilds_and_never_backdate_changes() {
        let mut g = generation();
        let first = g.checkpoint(None).unwrap();
        assert!(
            first.pages.values().all(|p| p.lastmod == at(2)),
            "bootstrap is observation, not invented history"
        );
        g.build_at = at(3);
        g.observed_at = at(4);
        let rebuild = g.checkpoint(Some(&first)).unwrap();
        assert_eq!(xml(&first), xml(&rebuild));
        g.version = "two".into();
        g.published_at = at(5);
        g.observed_at = at(6);
        g.pages.insert("/datasets".into(), "new data".into());
        g.pages
            .insert("/search/whigs".into(), "new total or snippet".into());
        let published = g.checkpoint(Some(&rebuild)).unwrap();
        assert_eq!(published.pages["/search/whigs"].lastmod, at(5));
        assert_eq!(published.pages["/datasets"].lastmod, at(5));
        assert_eq!(published.pages["/search/no-results"].lastmod, at(2));
        assert_eq!(published.pages["/privacy"].lastmod, at(2));
        g.observed_at = at(7);
        g.pages
            .insert("/search/whigs".into(), "code changes results".into());
        g.pages.insert("/privacy".into(), "new copy".into());
        let code = g.checkpoint(Some(&published)).unwrap();
        assert_eq!(code.pages["/search/whigs"].lastmod, at(7));
        assert_eq!(code.pages["/privacy"].lastmod, at(7));
        g.version = "three".into();
        g.published_at = at(6); // Published before the last code observation.
        g.observed_at = at(8);
        g.pages
            .insert("/search/whigs".into(), "different results".into());
        g.pages.remove("/search/no-results");
        g.pages.insert("/search/new-example".into(), "new".into());
        let next = g.checkpoint(Some(&code)).unwrap();
        assert_eq!(next.pages["/search/whigs"].lastmod, at(8));
        assert_eq!(next.pages["/search/new-example"].lastmod, at(8));
        assert!(!next.pages.contains_key("/search/no-results"));
    }

    #[test]
    fn stale_builds_and_observations_cannot_undo_newer_state() {
        let mut g = generation();
        let previous = g.checkpoint(None).unwrap();
        g.build_at = at(3);
        g.published_at = at(3);
        g.version = "new".into();
        g.observed_at = at(4);
        let newer = g.checkpoint(Some(&previous)).unwrap();
        assert!(generation().checkpoint(Some(&newer)).is_err());
        g.build_at = at(2);
        assert!(g.checkpoint(Some(&newer)).is_err());
        g.build_at = at(3);
        g.observed_at = at(3);
        assert!(g.checkpoint(Some(&newer)).is_err());
        g.observed_at = at(5);
        g.version = "different-at-same-publish-time".into();
        assert!(g.checkpoint(Some(&newer)).is_ok());
    }

    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../target/sitemap-tests/{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn state(dir: &Directory, documents: Option<Vec<usnm_search::PageDoc>>) -> Arc<AppState> {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data");
        let refdata = crate::refdata::RefData::load(&LocalStore::new(fixtures))
            .await
            .unwrap();
        let mut config = crate::config::Config::from_lookup(|_| None).unwrap();
        config.site_dir = Some(dir.0.clone());
        config.search_log_url = Some(dir.0.join("searches").display().to_string());
        config.rate_limit = None;
        let mut backend = MemoryBackend::new();
        if let Some(documents) = documents {
            for id in &refdata.current.indexes {
                backend.add_index(id, documents.clone());
            }
        }
        Arc::new(AppState::new(config, Arc::new(backend), refdata))
    }

    async fn http(state: &Arc<AppState>, path: &str) -> (StatusCode, String) {
        let response = crate::app(state.clone())
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn wait_for_sitemap(state: &Arc<AppState>) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let (status, xml) = http(state, "/sitemap.xml").await;
                if status == StatusCode::OK {
                    break xml;
                }
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("background sitemap complete")
    }

    #[tokio::test]
    async fn production_background_bootstrap_restart_and_failed_refresh_keep_durable_dates() {
        let dir = Directory::new();
        std::fs::write(dir.0.join("index.html"), "<main></main>").unwrap();
        std::fs::write(dir.0.join("sitemap.xml"), "undated build fallback").unwrap();
        std::fs::write(
            dir.0.join("sitemap-pages.json"),
            serde_json::to_vec(&serde_json::json!({
                "generated_at": Utc::now() - chrono::Duration::hours(1),
                "pages": STATIC.into_iter().map(|p| (p, fingerprint(p))).collect::<BTreeMap<_, _>>()
            }))
            .unwrap(),
        )
        .unwrap();
        let mut first = state(&dir, Some(Vec::new())).await;
        let cfg = &mut Arc::get_mut(&mut first).unwrap().config;
        cfg.sitemap_state_url = cfg.search_log_url.take();
        assert!(
            cfg.search_log_url.is_none(),
            "dates do not enable visitor logging"
        );
        assert_eq!(
            http(&first, "/sitemap.xml").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let unavailable = crate::app(first.clone())
            .oneshot(
                Request::builder()
                    .uri("/sitemap.xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(unavailable.headers()[header::RETRY_AFTER], "30");
        crate::spawn_background(first.clone());
        assert_eq!(http(&first, "/readyz").await.0, StatusCode::OK);
        let original = wait_for_sitemap(&first).await;
        let head = crate::app(first.clone())
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/sitemap.xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(
            head.headers()[header::CONTENT_TYPE],
            "application/xml; charset=utf-8"
        );
        assert_eq!(
            head.headers()[header::CONTENT_LENGTH],
            original.len().to_string()
        );
        assert!(head
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty());
        let count = STATIC.len() + super::super::CURATED_SEARCHES.len();
        assert_eq!(original.matches("<lastmod>").count(), count);
        assert!(original.contains("<loc>https://usnewsmap.com/datasets</loc>"));
        let store = LocalStore::new(dir.0.join("searches"));
        let checkpoint = latest(&store).await.unwrap().unwrap();
        assert_eq!(checkpoint.pages.len(), count);
        assert!(first.responses.is_none(), "not response-cache state");
        // A fresh replica can serve the complete saved sitemap even when its
        // very first preview computation fails (the indexes are unavailable).
        let restarted = state(&dir, None).await;
        crate::spawn_background(restarted.clone());
        assert_eq!(wait_for_sitemap(&restarted).await, original);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            latest(&store).await.unwrap().unwrap().sequence,
            checkpoint.sequence
        );
        assert_eq!(http(&restarted, "/sitemap.xml").await.1, original);
        let healthy_restart = state(&dir, Some(Vec::new())).await;
        crate::spawn_background(healthy_restart.clone());
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if latest(&store).await.unwrap().unwrap().sequence > checkpoint.sequence {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(http(&healthy_restart, "/sitemap.xml").await.1, original);
        // Explicit fixture mode still serves Vite's undated fallback.
        let mut fixture = state(&dir, None).await;
        Arc::get_mut(&mut fixture).unwrap().config.search_log_url = None;
        assert_eq!(
            http(&fixture, "/sitemap.xml").await,
            (StatusCode::OK, "undated build fallback".into())
        );
    }

    #[tokio::test]
    async fn fingerprint_is_exact_preview_including_zero_counts_top_city_and_snippets() {
        let dir = Directory::new();
        let fixtures = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/data/indexes/pages-base-fixture.jsonl");
        let text = std::fs::read_to_string(fixtures).unwrap();
        let mut doc: usnm_search::PageDoc =
            serde_json::from_str(text.lines().next().unwrap()).unwrap();
        doc.text = "Whigs debated the first proposal.".into();
        let render = async |documents| {
            let state = state(&dir, Some(documents)).await;
            super::super::search_preview_snapshot(&state, state.snapshot.load_full(), "whigs", true)
                .await
                .unwrap()
        };
        let empty = render(Vec::new()).await;
        assert!(empty.contains("No matching newspaper pages."));
        let first = render(vec![doc.clone()]).await;
        assert_ne!(fingerprint(&empty), fingerprint(&first));
        assert_eq!(
            first,
            render(vec![doc.clone()]).await,
            "no timing/version noise"
        );
        let mut other = doc.clone();
        other.doc_id.push_str("-other");
        assert_ne!(
            fingerprint(&first),
            fingerprint(render(vec![doc.clone(), other]).await)
        );
        doc.text = "Whigs debated an entirely different proposal.".into();
        let snippet = render(vec![doc.clone()]).await;
        assert_ne!(fingerprint(&first), fingerprint(&snippet));
        doc.place_id = "P00002".into();
        assert_ne!(fingerprint(&snippet), fingerprint(render(vec![doc]).await));
    }

    #[tokio::test]
    async fn background_retry_reuses_successful_immutable_previews() {
        let dir = Directory::new();
        let st = state(&dir, Some(Vec::new())).await;
        let render = async || {
            super::super::search_preview_snapshot(&st, st.snapshot.load_full(), "whigs", true).await
        };
        let successful = render().await.unwrap();
        let unavailable = state(&dir, None).await;
        st.snapshot.store(unavailable.snapshot.load_full());
        assert_eq!(
            render().await.unwrap(),
            successful,
            "retry reuses partial success instead of recomputing against a failed backend"
        );
        st.cache.invalidate_all();
        assert!(
            render().await.is_err(),
            "without reusable progress the backend fails"
        );
    }

    #[derive(Debug)]
    struct RacingStore {
        inner: LocalStore,
        barrier: tokio::sync::Barrier,
        attempts: std::sync::atomic::AtomicUsize,
        collisions: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ObjectStore for RacingStore {
        async fn get(
            &self,
            path: &str,
        ) -> std::result::Result<Option<Vec<u8>>, usnm_store::StoreError> {
            self.inner.get(path).await
        }

        async fn list(
            &self,
            prefix: &str,
        ) -> std::result::Result<Vec<String>, usnm_store::StoreError> {
            self.inner.list(prefix).await
        }

        async fn put_new(
            &self,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> std::result::Result<bool, usnm_store::StoreError> {
            use std::sync::atomic::Ordering;
            if self.attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                self.barrier.wait().await;
            }
            let inserted = self.inner.put_new(path, body, content_type).await?;
            if !inserted {
                self.collisions.fetch_add(1, Ordering::SeqCst);
            }
            Ok(inserted)
        }

        async fn put(
            &self,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<(), usnm_store::StoreError> {
            panic!("sitemap must never overwrite durable state");
        }
    }

    #[tokio::test]
    async fn append_conflicts_recompare_and_snapshot_changes_prevent_commits() {
        let dir = Directory::new();
        let st = state(&dir, Some(Vec::new())).await;
        let snap = st.snapshot.load_full();
        let store = RacingStore {
            inner: LocalStore::new(dir.0.join("checkpoints")),
            barrier: tokio::sync::Barrier::new(2),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            collisions: std::sync::atomic::AtomicUsize::new(0),
        };
        let g = generation();
        let (a, b) = tokio::join!(
            commit(&st, &snap, &store, &g),
            commit(&st, &snap, &store, &g)
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.sequence, b.sequence);
        assert_eq!(
            store.collisions.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            xml(&a),
            xml(&b),
            "winner's original dates survive a competing append"
        );
        assert_eq!(latest(&store).await.unwrap().unwrap().sequence, 2);
        let replacement = state(&dir, Some(Vec::new())).await;
        st.snapshot.store(replacement.snapshot.load_full());
        assert!(commit(&st, &snap, &store, &g).await.is_err());
        assert_eq!(latest(&store).await.unwrap().unwrap().sequence, 2);
    }

    #[tokio::test]
    async fn published_pointer_change_before_local_reload_prevents_commit() {
        let dir = Directory::new();
        let mut st = state(&dir, Some(Vec::new())).await;
        let snap = st.snapshot.load_full();
        let reference = dir.0.join("reference");
        std::fs::create_dir_all(&reference).unwrap();
        let mut current = snap.refdata.current.clone();
        current.index_version = "newly-published-not-yet-loaded".into();
        std::fs::write(
            reference.join("current.json"),
            serde_json::to_vec(&current).unwrap(),
        )
        .unwrap();
        Arc::get_mut(&mut st).unwrap().loader = Some(crate::Loader {
            reference: Arc::new(LocalStore::new(reference)),
            engine: crate::Engine::Shared(snap.backend.clone()),
            congress_search: true,
        });
        let store = LocalStore::new(dir.0.join("checkpoints"));
        assert!(commit(&st, &snap, &store, &generation()).await.is_err());
        assert!(store.list(PREFIX).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn authoritative_pointer_rollback_advances_only_changed_pages() {
        let dir = Directory::new();
        let mut st = state(&dir, Some(Vec::new())).await;
        let fixtures = LocalStore::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data"),
        );
        let mut refdata = crate::refdata::RefData::load(&fixtures).await.unwrap();
        refdata.current.index_version = "newer-publication".into();
        refdata.current.published_at = at(5).to_rfc3339();
        let newer = Arc::new(Snapshot {
            refdata,
            backend: st.snapshot.load_full().backend.clone(),
            congress_dropped: false,
        });
        let reference = Arc::new(LocalStore::new(dir.0.join("reference")));
        reference
            .put(
                "current.json",
                serde_json::to_vec(&newer.refdata.current).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        Arc::get_mut(&mut st).unwrap().loader = Some(crate::Loader {
            reference: reference.clone(),
            engine: crate::Engine::Shared(newer.backend.clone()),
            congress_search: true,
        });
        st.snapshot.store(newer.clone());
        let store = LocalStore::new(dir.0.join("checkpoints"));
        let mut g = generation();
        g.version = newer.refdata.version().into();
        g.published_at = at(5);
        g.observed_at = at(6);
        let first = commit(&st, &newer, &store, &g).await.unwrap();

        let mut refdata = crate::refdata::RefData::load(&fixtures).await.unwrap();
        refdata.current.published_at = at(1).to_rfc3339();
        let older = Arc::new(Snapshot {
            refdata,
            backend: newer.backend.clone(),
            congress_dropped: false,
        });
        reference
            .put(
                "current.json",
                serde_json::to_vec(&older.refdata.current).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        g.version = older.refdata.version().into();
        g.published_at = at(1);
        g.observed_at = at(7);
        g.pages
            .insert("/search/whigs".into(), "rolled-back results".into());
        // The pointer is already rolled back, but this replica has not yet
        // swapped snapshots. Its old captured generation must not append.
        assert!(commit(&st, &newer, &store, &g).await.is_err());
        assert_eq!(
            latest(&store).await.unwrap().unwrap().sequence,
            first.sequence
        );
        st.snapshot.store(older.clone());
        let rolled_back = commit(&st, &older, &store, &g).await.unwrap();
        assert_eq!(rolled_back.version, older.refdata.version());
        assert_eq!(rolled_back.published_at, at(1));
        assert_eq!(rolled_back.pages["/search/whigs"].lastmod, at(7));
        for path in ["/privacy", "/datasets", "/search/no-results"] {
            assert_eq!(rolled_back.pages[path].lastmod, first.pages[path].lastmod);
        }
        assert!(commit(&st, &newer, &store, &g).await.is_err());
    }
}
