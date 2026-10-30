//! Durable sitemap generations. The search log store is create-only and has no
//! expiry; response-cache blobs deliberately are NOT used for these dates.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usnm_store::ObjectStore;

use crate::{AppState, Snapshot};

mod recurring;

const PREFIX: &str = "sitemap/state";
const STATIC: [&str; 4] = ["/", "/privacy", "/credits", "/datasets"];
const MAX_SITEMAP_BYTES: usize = 50 * 1024 * 1024;
const ORIGIN: &str = "https://usnewsmap.com";
const XML_START: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n";
const XML_END: &str = "</urlset>\n";

#[derive(Default)]
pub(crate) struct Service {
    committed: ArcSwapOption<Checkpoint>,
}

impl Service {
    pub(super) fn search_paths(&self) -> Vec<String> {
        let mut paths: BTreeSet<String> = super::CURATED_SEARCHES.iter().cloned().collect();
        if let Some(checkpoint) = self.committed.load().as_ref() {
            paths.extend(
                checkpoint
                    .pages
                    .keys()
                    .filter(|path| path.starts_with("/search/"))
                    .cloned(),
            );
        }
        paths.into_iter().collect()
    }

    pub(super) fn contains(&self, path: &str) -> bool {
        self.committed
            .load()
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.pages.contains_key(path))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Page {
    fingerprint: String,
    lastmod: DateTime<Utc>,
    /// Rendering/copy identity without index data. Legacy checkpoints lack it,
    /// so their first changed page conservatively uses observation time.
    #[serde(default)]
    content_provenance: String,
    #[serde(default)]
    promoted: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Checkpoint {
    sequence: u64,
    version: String,
    published_at: DateTime<Utc>,
    build_at: DateTime<Utc>,
    web_at: DateTime<Utc>,
    renderer: String,
    #[serde(default)]
    snapshot: String,
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

fn search_policy(snap: &Snapshot) -> String {
    format!(
        "american_stories={}",
        snap.refdata.searches_american_stories()
    )
}

fn snapshot_identity(snap: &Snapshot) -> Result<String> {
    Ok(fingerprint(format!(
        "{}\0{}",
        fingerprint(serde_json::to_vec(&snap.refdata.current).map_err(backend)?),
        search_policy(snap),
    )))
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
    content_provenance: BTreeMap<String, String>,
    snapshot: String,
    promoted: BTreeSet<String>,
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
                let content_provenance = self
                    .content_provenance
                    .get(path)
                    .cloned()
                    .unwrap_or_default();
                let lastmod = match old {
                    Some(old) if &old.fingerprint == fingerprint => old.lastmod,
                    // Publication explains changed results only when it is a
                    // new publication, not a code-only change or bootstrap.
                    Some(old)
                        if previous.is_some_and(|p| {
                            p.version != self.version && self.published_at > p.published_at
                        }) && self.published_at > old.lastmod
                            && !content_provenance.is_empty()
                            && old.content_provenance == content_provenance
                            && (path.starts_with("/search/")
                                || matches!(path.as_str(), "/datasets" | "/credits")) =>
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
                        content_provenance,
                        promoted: self.promoted.contains(path),
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
            snapshot: self.snapshot.clone(),
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
        let mut published: crate::refdata::Current =
            serde_json::from_slice(&current).map_err(backend)?;
        // Loader intentionally projects away an unavailable optional Congress
        // set. Compare the pointer in that same serving projection.
        if snap.congress_dropped {
            published.congress = None;
        }
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
        if previous.as_ref().is_some_and(|checkpoint| {
            checkpoint
                .pages
                .iter()
                .any(|(path, page)| page.promoted && !generation.pages.contains_key(path))
        }) {
            return Err(backend(
                "concurrent search promotion; retry full generation",
            ));
        }
        let next = generation.checkpoint(previous.as_ref())?;
        if next.pages.keys().any(|path| !valid_url(path)) {
            return Err(backend("sitemap URL must be shorter than 2,048 characters"));
        }
        if next.pages.len() > 50_000 || xml_size(&next) > MAX_SITEMAP_BYTES {
            return Err(backend(
                "sitemap exceeds the 50,000 URL or 50 MiB protocol limit",
            ));
        }
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
        content_provenance: BTreeMap::new(),
        snapshot: snapshot_identity(&snap)?,
        promoted: BTreeSet::new(),
    };
    generation.check_freshness(state.sitemap.committed.load_full().as_deref())?;
    let mut pages = manifest.pages.clone();
    let policy = search_policy(&snap);
    let shell = b"<title>US News Map</title><link rel=\"canonical\" href=\"https://usnewsmap.com/\" /><meta property=\"og:title\" content=\"US News Map\" /><meta property=\"og:url\" content=\"https://usnewsmap.com/\" />";
    for (path, content) in &mut pages {
        *content = fingerprint(format!(
            "{content}\0{}",
            fingerprint(super::shell_for(path, shell.to_vec()))
        ));
        if path == "/datasets" {
            *content = fingerprint(format!("{content}\0{policy}"));
        }
        generation.content_provenance.insert(
            path.clone(),
            fingerprint(format!("{content}\0{}", renderer())),
        );
    }
    // Dataset details and OCR credits describe the pinned published snapshot,
    // not live queue/health/timing counters.
    let provenance = serde_json::to_vec(&snap.refdata.current).map_err(backend)?;
    let provenance = fingerprint(provenance);
    for path in ["/datasets", "/credits"] {
        let content = pages.get_mut(path).expect("validated manifest");
        *content = fingerprint(format!("{content}\0{provenance}"));
    }
    let mut paths: BTreeSet<String> = super::CURATED_SEARCHES.iter().cloned().collect();
    if let Some(checkpoint) = state.sitemap.committed.load_full() {
        paths.extend(
            checkpoint
                .pages
                .iter()
                .filter(|(_, page)| page.promoted)
                .map(|(path, _)| path.clone()),
        );
    }
    let retained = paths.clone();
    if let Some(url) = &state.config.search_log_url {
        let log = usnm_store::open(url).map_err(backend)?;
        let mut keys = paths
            .iter()
            .map(|path| query_key(&snap, path))
            .collect::<Result<BTreeSet<_>>>()?;
        for path in recurring::candidates(log.as_ref(), &snap, observed_at.date_naive()).await? {
            if keys.insert(query_key(&snap, &path)?) {
                paths.insert(path);
            }
        }
    }
    for path in paths {
        if !Arc::ptr_eq(&state.snapshot.load_full(), &snap) {
            return Err(backend("snapshot changed during refresh"));
        }
        let query =
            super::search_query(&path).ok_or_else(|| backend("invalid saved search path"))?;
        let preview = super::search_preview_data(state, snap.clone(), &query, true).await?;
        if !retained.contains(&path) && preview.total_hits <= 50 {
            continue;
        }
        if !super::CURATED_SEARCHES.contains(&path) {
            generation.promoted.insert(path.clone());
        }
        pages.insert(
            path.clone(),
            fingerprint(format!("{}\0{policy}", preview.html)),
        );
        generation.content_provenance.insert(
            path.clone(),
            fingerprint(format!("{}\0{policy}", renderer())),
        );
        tokio::task::yield_now().await;
    }
    generation.pages = pages;
    Ok(generation)
}

fn query_key(snap: &Snapshot, path: &str) -> Result<String> {
    let query = super::search_query(path).ok_or_else(|| backend("invalid saved search path"))?;
    let raw = usnm_core::params::RawParams::parse(&super::preview_query(&query))?;
    Ok(snap.refdata.search_request(&raw)?.query.to_string())
}

fn valid_url(path: &str) -> bool {
    ORIGIN.len() + path.len() < 2048
}

async fn refresh(
    state: &Arc<AppState>,
    store: &dyn ObjectStore,
    dir: &std::path::Path,
    finished: &mut Option<(String, DateTime<Utc>, String, chrono::NaiveDate, u32)>,
) -> Result<()> {
    if let Some(checkpoint) = latest(store).await? {
        state.sitemap.committed.store(Some(Arc::new(checkpoint)));
    }
    let bytes = tokio::fs::read(dir.join("sitemap-pages.json"))
        .await
        .map_err(backend)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(backend)?;
    let snap = state.snapshot.load_full();
    let snapshot = snapshot_identity(&snap)?;
    let identity = (
        snap.refdata.version().to_owned(),
        manifest.generated_at,
        snapshot.clone(),
        Utc::now().date_naive(),
        Utc::now().hour(),
    );
    let committed = state.sitemap.committed.load_full();
    if finished.as_ref() == Some(&identity)
        && committed.as_ref().is_some_and(|checkpoint| {
            checkpoint.version == identity.0
                && checkpoint.web_at == manifest.generated_at
                && checkpoint.build_at == build_at()
                && checkpoint.renderer == renderer()
                && checkpoint.snapshot == snapshot
        })
    {
        return Ok(());
    }
    let generation = generate(state, snap.clone(), &manifest).await?;
    if finished.as_ref().is_some_and(|previous| {
        previous.0 == identity.0 && previous.1 == identity.1 && previous.2 == identity.2
    }) && committed.as_ref().is_some_and(|checkpoint| {
        checkpoint.version == generation.version
            && checkpoint.snapshot == generation.snapshot
            && checkpoint.web_at == generation.web_at
            && checkpoint.build_at == generation.build_at
            && checkpoint.renderer == renderer()
            && checkpoint.pages.len() == generation.pages.len()
            && generation.pages.iter().all(|(path, hash)| {
                checkpoint
                    .pages
                    .get(path)
                    .is_some_and(|page| page.fingerprint == *hash)
            })
    }) {
        *finished = Some(identity);
        return Ok(());
    }
    let checkpoint = commit(state, &snap, store, &generation).await?;
    tracing::info!(
        sequence = checkpoint.sequence,
        pages = checkpoint.pages.len(),
        "sitemap generation committed"
    );
    state.sitemap.committed.store(Some(Arc::new(checkpoint)));
    *finished = Some(identity);
    Ok(())
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
                refresh(&state, store.as_ref(), &dir, &mut finished).await
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
    let mut xml = XML_START.to_owned();
    for (path, page) in &checkpoint.pages {
        xml.push_str(&xml_entry(path, page));
    }
    xml.push_str(XML_END);
    xml
}

fn xml_entry(path: &str, page: &Page) -> String {
    format!(
        "  <url><loc>{ORIGIN}{}</loc><lastmod>{}</lastmod></url>\n",
        super::escape_html(path),
        page.lastmod
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

fn xml_size(checkpoint: &Checkpoint) -> usize {
    XML_START.len()
        + XML_END.len()
        + checkpoint
            .pages
            .iter()
            .map(|(path, page)| xml_entry(path, page).len())
            .sum::<usize>()
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
            content_provenance: [
                "/privacy",
                "/datasets",
                "/search/whigs",
                "/search/no-results",
            ]
            .into_iter()
            .map(|p| (p.to_owned(), "same renderer and copy".to_owned()))
            .collect(),
            snapshot: String::new(),
            promoted: BTreeSet::new(),
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

    #[test]
    fn publication_dates_require_forward_publication_and_comparable_page_content() {
        let mut g = generation();
        let first = g.checkpoint(None).unwrap();
        g.version = "newer-index-same-results".into();
        g.published_at = at(5);
        g.observed_at = at(6);
        let newer = g.checkpoint(Some(&first)).unwrap();
        assert_eq!(newer.pages["/search/whigs"].lastmod, at(2));
        g.version = "rolled-back-index".into();
        g.published_at = at(3); // Later than this page's date, but a rollback.
        g.observed_at = at(7);
        g.pages
            .insert("/search/whigs".into(), "rolled-back results".into());
        let rollback = g.checkpoint(Some(&newer)).unwrap();
        assert_eq!(rollback.pages["/search/whigs"].lastmod, at(7));
        assert_eq!(rollback.pages["/datasets"].lastmod, at(2));

        g.version = "forward-index-and-deployment".into();
        g.published_at = at(8);
        g.observed_at = at(9);
        for path in ["/search/whigs", "/datasets"] {
            g.pages.insert(path.into(), "new rendered content".into());
            g.content_provenance
                .insert(path.into(), "new renderer or page copy".into());
        }
        let deployed = g.checkpoint(Some(&rollback)).unwrap();
        assert_eq!(deployed.pages["/search/whigs"].lastmod, at(9));
        assert_eq!(deployed.pages["/datasets"].lastmod, at(9));
        assert_eq!(deployed.pages["/privacy"].lastmod, at(2));
        // Existing pre-provenance checkpoints remain readable, but cannot
        // establish that an observed change came solely from publication.
        let mut legacy = serde_json::to_value(rollback).unwrap();
        legacy["pages"]["/datasets"]
            .as_object_mut()
            .unwrap()
            .remove("content_provenance");
        let legacy: Checkpoint = serde_json::from_value(legacy).unwrap();
        g.content_provenance
            .insert("/datasets".into(), "same renderer and copy".into());
        assert_eq!(
            g.checkpoint(Some(&legacy)).unwrap().pages["/datasets"].lastmod,
            at(9)
        );
    }

    #[tokio::test]
    async fn publish_only_changes_pinned_credits_without_changing_content_provenance() {
        let dir = Directory::new();
        let st = state(&dir, Some(Vec::new())).await;
        let mut manifest = Manifest {
            generated_at: Utc::now() - chrono::Duration::hours(1),
            pages: STATIC
                .into_iter()
                .map(|p| (p.to_owned(), fingerprint(p)))
                .collect(),
        };
        let old = generate(&st, st.snapshot.load_full(), &manifest)
            .await
            .unwrap();
        let fixtures = LocalStore::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data"),
        );
        let mut refdata = crate::refdata::RefData::load(&fixtures).await.unwrap();
        refdata.current.index_version = "new-ocr-inventory-publication".into();
        refdata.current.published_at = at(6).to_rfc3339();
        st.snapshot.store(Arc::new(Snapshot {
            refdata,
            backend: st.snapshot.load_full().backend.clone(),
            congress_dropped: false,
        }));
        let new = generate(&st, st.snapshot.load_full(), &manifest)
            .await
            .unwrap();
        assert_ne!(old.pages["/credits"], new.pages["/credits"]);
        assert_eq!(
            old.content_provenance["/credits"],
            new.content_provenance["/credits"]
        );
        assert_eq!(old.pages["/privacy"], new.pages["/privacy"]);
        let mut old = old;
        old.observed_at = at(2);
        old.published_at = at(1);
        let previous = old.checkpoint(None).unwrap();
        let current = new.checkpoint(Some(&previous)).unwrap();
        assert_eq!(current.pages["/credits"].lastmod, at(6));
        assert_eq!(current.pages["/privacy"].lastmod, at(2));
        for path in ["/datasets", "/credits"] {
            manifest.pages.insert(
                path.into(),
                "new source copy deployed with publication".into(),
            );
        }
        let deployed = generate(&st, st.snapshot.load_full(), &manifest)
            .await
            .unwrap();
        let current = deployed.checkpoint(Some(&previous)).unwrap();
        for path in ["/datasets", "/credits"] {
            assert_ne!(
                deployed.content_provenance[path],
                old.content_provenance[path]
            );
            assert_eq!(current.pages[path].lastmod, deployed.observed_at);
        }
        assert_eq!(current.pages["/privacy"].lastmod, at(2));
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
    async fn recurring_searches_promote_at_51_with_indexable_ssr_and_survive_restart_and_failure() {
        let dir = Directory::new();
        std::fs::write(
            dir.0.join("index.html"),
            std::fs::read(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/index.html"),
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.0.join("sitemap-pages.json"),
            serde_json::to_vec(&serde_json::json!({
                "generated_at": Utc::now() - chrono::Duration::hours(1),
                "pages": STATIC.into_iter().map(|p| (p, fingerprint(p))).collect::<BTreeMap<_, _>>()
            }))
            .unwrap(),
        )
        .unwrap();
        let fixtures = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/data/indexes/pages-base-fixture.jsonl");
        let text = std::fs::read_to_string(fixtures).unwrap();
        let mut doc: usnm_search::PageDoc =
            serde_json::from_str(text.lines().next().unwrap()).unwrap();
        doc.text = "Cobalt jubilee and amber jubilee. California gold diggings.".into();
        let documents: Vec<_> = (0..51)
            .map(|i| {
                let mut doc = doc.clone();
                doc.doc_id = format!("synthetic-promotion-{i}");
                if i == 50 {
                    doc.text = "Cobalt jubilee. California gold diggings.".into();
                }
                doc
            })
            .collect();
        let st = state(&dir, Some(Vec::new())).await;
        let refdata = crate::refdata::RefData::load(&LocalStore::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data"),
        ))
        .await
        .unwrap();
        let mut backend = MemoryBackend::new();
        for (i, id) in refdata.current.indexes.iter().enumerate() {
            backend.add_index(
                id,
                if i == 0 {
                    documents.clone()
                } else {
                    Vec::new()
                },
            );
        }
        st.snapshot.store(Arc::new(Snapshot {
            refdata,
            backend: Arc::new(backend),
            congress_dropped: false,
        }));
        let snap = st.snapshot.load_full();
        let store = LocalStore::new(dir.0.join("searches"));
        let example_query = |id: &str| {
            crate::prewarm::examples()
                .iter()
                .find(|example| example.id == id)
                .unwrap()
                .view["q"]
                .as_str()
                .unwrap()
        };
        let curated_gold = super::super::search_path(example_query("california-gold-rush"));
        let curated_whigs = super::super::search_path(example_query("whigs"));
        let canonical_gold = query_key(&snap, &curated_gold).unwrap();
        let alternate_gold = super::super::search_path(&canonical_gold);
        assert_ne!(
            curated_gold, alternate_gold,
            "canonical AST ordering differs from curated wording"
        );
        let today = Utc::now().date_naive();
        for day in [
            today.pred_opt().unwrap(),
            today.pred_opt().unwrap().pred_opt().unwrap(),
        ] {
            recurring::tests::write(
                &store,
                day,
                &[
                    recurring::tests::record(&snap, day, "\"cobalt jubilee\""),
                    recurring::tests::record(&snap, day, "\"amber jubilee\""),
                    recurring::tests::record(&snap, day, example_query("whigs")),
                    recurring::tests::record(&snap, day, &canonical_gold),
                ],
            )
            .await;
        }
        let response = crate::app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/search/cobalt-jubilee")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()["x-robots-tag"], "noindex");
        let mut finished = None;
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        let first = latest(&store).await.unwrap().unwrap();
        assert_eq!(
            first.pages.len(),
            STATIC.len() + super::super::CURATED_SEARCHES.len() + 1
        );
        assert!(first.pages["/search/cobalt-jubilee"].promoted);
        assert!(first.pages.contains_key(&curated_gold));
        assert!(
            !first.pages.contains_key(&alternate_gold),
            "keep the curated URL for equivalent normalized queries"
        );
        assert!(
            !first.pages.contains_key("/search/amber-jubilee"),
            "exactly 50 is excluded"
        );
        assert_eq!(
            first.pages.keys().filter(|p| *p == &curated_whigs).count(),
            1
        );
        let response = crate::app(st.clone())
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let home = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(home.contains(r#"<nav id="common-searches""#));
        for path in first
            .pages
            .keys()
            .filter(|path| path.starts_with("/search/"))
        {
            assert!(
                home.contains(&format!(r#"href="{}""#, super::super::escape_html(path))),
                "{path}"
            );
        }
        assert!(!home.contains(r#"href="/search/amber-jubilee""#));
        let response = crate::app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/search/cobalt-jubilee")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key("x-robots-tag"));
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("51 matching newspaper pages in total."));
        assert!(html.contains("href=\"https://usnewsmap.com/search/cobalt-jubilee\""));
        assert!(html.contains(r#"<div id="root"></div><div id="search-landing">"#));
        assert!(!html.contains(r#"class="visually-hidden" inert"#));
        let filtered = crate::app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/search/cobalt-jubilee?state=NY")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(filtered.headers()["x-robots-tag"], "noindex");
        let lastmod = first.pages["/search/cobalt-jubilee"].lastmod;
        // The next hourly scan with no additions does not append another checkpoint.
        finished.as_mut().unwrap().4 = (Utc::now().hour() + 23) % 24;
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        assert_eq!(
            latest(&store).await.unwrap().unwrap().sequence,
            first.sequence
        );
        // A restart reuses the saved admission, even with logging disabled and no results.
        let mut restarted = state(&dir, Some(Vec::new())).await;
        let config = &mut Arc::get_mut(&mut restarted).unwrap().config;
        config.sitemap_state_url = config.search_log_url.take();
        refresh(&restarted, &store, &dir.0, &mut None)
            .await
            .unwrap();
        let retained = latest(&store).await.unwrap().unwrap();
        assert!(retained.pages["/search/cobalt-jubilee"].promoted);
        assert!(retained.pages["/search/cobalt-jubilee"].lastmod >= lastmod);
        assert!(http(&restarted, "/search/cobalt-jubilee")
            .await
            .1
            .contains("No matching newspaper pages."));
        // Corruption of a finalized day fails the refresh without publishing partial state.
        let before = http(&st, "/sitemap.xml").await.1;
        store
            .put(
                &format!("days/{}.jsonl", today.pred_opt().unwrap()),
                b"invalid".to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        assert!(refresh(&st, &store, &dir.0, &mut None).await.is_err());
        assert_eq!(
            latest(&store).await.unwrap().unwrap().sequence,
            retained.sequence
        );
        // Loading a peer's newer committed generation is allowed before a failed refresh.
        assert_eq!(http(&st, "/sitemap.xml").await.1, xml(&retained));
        assert!(before.contains("/search/cobalt-jubilee"));
    }

    #[tokio::test]
    async fn oversized_xml_does_not_replace_the_committed_generation() {
        let dir = Directory::new();
        let st = state(&dir, Some(Vec::new())).await;
        let snap = st.snapshot.load_full();
        let store = LocalStore::new(dir.0.join("searches"));
        let mut g = generation();
        let first = commit(&st, &snap, &store, &g).await.unwrap();
        assert_eq!(xml_size(&first), xml(&first).len());
        g.observed_at = at(3);
        let encoded = "%E6%96%87".repeat(215);
        for n in 0..27_000 {
            g.pages
                .insert(format!("/search/{encoded}{n}"), "results".into());
        }
        assert!(g.pages.len() < 50_000, "URL count alone would allow this");
        let error = commit(&st, &snap, &store, &g).await.unwrap_err();
        assert!(
            matches!(error, crate::error::ApiError::Backend(message) if message.contains("50 MiB"))
        );
        assert_eq!(xml(&latest(&store).await.unwrap().unwrap()), xml(&first));
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

    fn write_manifest(dir: &Directory) {
        std::fs::write(
            dir.0.join("sitemap-pages.json"),
            serde_json::to_vec(&serde_json::json!({
                "generated_at": Utc::now() - chrono::Duration::hours(1),
                "pages": STATIC.into_iter().map(|p| (p, fingerprint(p))).collect::<BTreeMap<_, _>>()
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn unavailable_optional_congress_does_not_prevent_sitemap_bootstrap() {
        let dir = Directory::new();
        write_manifest(&dir);
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data");
        let mut current: crate::refdata::Current =
            serde_json::from_slice(&std::fs::read(fixtures.join("current.json")).unwrap()).unwrap();
        let reference = dir.0.join("reference");
        std::fs::create_dir_all(reference.join(&current.reference)).unwrap();
        for entry in std::fs::read_dir(fixtures.join(&current.reference)).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(
                entry.path(),
                reference.join(&current.reference).join(entry.file_name()),
            )
            .unwrap();
        }
        current.congress = Some(crate::refdata::CongressIndexes {
            indexes: vec!["optional-congress-unavailable".into()],
            common_grams: None,
            series: vec![crate::refdata::CongressSeries {
                series: "globe".into(),
                pages: 78,
                from: "1850-01-07".parse().unwrap(),
                to: "1850-09-30".parse().unwrap(),
            }],
        });
        std::fs::write(
            reference.join("current.json"),
            serde_json::to_vec(&current).unwrap(),
        )
        .unwrap();
        let loader = crate::Loader {
            reference: Arc::new(LocalStore::new(&reference)),
            engine: crate::Engine::Memory {
                indexes_dir: fixtures.join("indexes"),
            },
            congress_search: true,
        };
        let snap = loader.snapshot().await.unwrap();
        assert!(snap.congress_dropped);
        assert!(snap.refdata.current.congress.is_none());
        let st = Arc::new(AppState::with_loader(
            crate::config::Config::from_lookup(|_| None).unwrap(),
            snap,
            Some(loader),
        ));
        let store = LocalStore::new(dir.0.join("checkpoints"));
        let mut finished = None;
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        assert_eq!(
            latest(&store).await.unwrap().unwrap().pages.len(),
            STATIC.len() + super::super::CURATED_SEARCHES.len()
        );
        still_current(&st, &st.snapshot.load_full()).await.unwrap();
        current.indexes.push("changed-newspaper-pointer".into());
        std::fs::write(
            reference.join("current.json"),
            serde_json::to_vec(&current).unwrap(),
        )
        .unwrap();
        assert!(still_current(&st, &st.snapshot.load_full()).await.is_err());
    }

    #[tokio::test]
    async fn replica_rebuilds_after_adopting_peer_checkpoint_for_a_rolled_back_version() {
        let dir = Directory::new();
        write_manifest(&dir);
        let a = state(&dir, Some(Vec::new())).await;
        let b = state(&dir, Some(Vec::new())).await;
        let fixtures = LocalStore::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data"),
        );
        let mut refdata = crate::refdata::RefData::load(&fixtures).await.unwrap();
        refdata.current.index_version = "peer-version-B".into();
        b.snapshot.store(Arc::new(Snapshot {
            refdata,
            backend: b.snapshot.load_full().backend.clone(),
            congress_dropped: false,
        }));
        let store = LocalStore::new(dir.0.join("checkpoints"));
        let mut a_finished = None;
        refresh(&a, &store, &dir.0, &mut a_finished).await.unwrap();
        let first = latest(&store).await.unwrap().unwrap();
        refresh(&b, &store, &dir.0, &mut None).await.unwrap();
        let peer = latest(&store).await.unwrap().unwrap();
        assert_eq!(peer.version, "peer-version-B");
        assert!(peer.sequence > first.sequence);
        // A still has its original completion marker; the publisher has rolled
        // back to A, and the peer which computed B has stopped.
        refresh(&a, &store, &dir.0, &mut a_finished).await.unwrap();
        let rollback = latest(&store).await.unwrap().unwrap();
        assert_eq!(rollback.version, first.version);
        assert!(rollback.sequence > peer.sequence);
        assert_eq!(
            a.sitemap.committed.load_full().unwrap().version,
            first.version
        );
        assert_eq!(
            rollback.pages["/privacy"].lastmod,
            first.pages["/privacy"].lastmod
        );
        refresh(&a, &store, &dir.0, &mut a_finished).await.unwrap();
        assert_eq!(
            latest(&store).await.unwrap().unwrap().sequence,
            rollback.sequence,
            "matching adopted checkpoint may skip recomputation"
        );
        let mut mismatched = rollback.clone();
        mismatched.sequence += 1;
        mismatched.snapshot = "different-serving-projection-of-the-same-version".into();
        assert!(store
            .put_new(
                &format!("{PREFIX}/{:020}.json", mismatched.sequence),
                serde_json::to_vec(&mismatched).unwrap(),
                "application/json",
            )
            .await
            .unwrap());
        refresh(&a, &store, &dir.0, &mut a_finished).await.unwrap();
        let corrected = latest(&store).await.unwrap().unwrap();
        assert!(corrected.sequence > mismatched.sequence);
        assert_eq!(corrected.snapshot, rollback.snapshot);
    }

    #[tokio::test]
    async fn same_image_search_policy_changes_refresh_dates_and_cannot_use_pending_publication_time(
    ) {
        let curated =
            super::super::search_path(crate::prewarm::examples()[0].view["q"].as_str().unwrap());
        let dir = Directory::new();
        write_manifest(&dir);
        let st = state(&dir, Some(Vec::new())).await;
        let fixtures = LocalStore::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data"),
        );
        let snapshot = async |enabled: bool, version: &str, published_at| {
            let mut refdata = crate::refdata::RefData::load(&fixtures).await.unwrap();
            refdata.current.american_stories = Some(usnm_core::american_stories::VERSION);
            refdata.current.index_version = version.to_owned();
            refdata.current.published_at = published_at;
            refdata.american_stories_search = enabled;
            Arc::new(Snapshot {
                refdata,
                backend: st.snapshot.load_full().backend.clone(),
                congress_dropped: false,
            })
        };
        let on = snapshot(true, "same-image-version", at(1).to_rfc3339()).await;
        st.snapshot.store(on.clone());
        let store = LocalStore::new(dir.0.join("checkpoints"));
        let mut finished = None;
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        let first = latest(&store).await.unwrap().unwrap();
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        assert_eq!(
            latest(&store).await.unwrap().unwrap().sequence,
            first.sequence
        );

        st.snapshot
            .store(snapshot(false, "same-image-version", at(1).to_rfc3339()).await);
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        let off = latest(&store).await.unwrap().unwrap();
        assert!(
            off.sequence > first.sequence,
            "finished marker must include policy"
        );
        assert_ne!(off.snapshot, first.snapshot);
        for path in ["/datasets", curated.as_str()] {
            assert_ne!(off.pages[path].fingerprint, first.pages[path].fingerprint);
            assert_ne!(
                off.pages[path].content_provenance,
                first.pages[path].content_provenance
            );
            assert_eq!(off.pages[path].lastmod, off.observed_at);
        }
        assert_eq!(
            off.pages["/credits"].lastmod,
            first.pages["/credits"].lastmod
        );
        assert_eq!(
            off.pages["/privacy"].lastmod,
            first.pages["/privacy"].lastmod
        );
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        assert_eq!(
            latest(&store).await.unwrap().unwrap().sequence,
            off.sequence
        );

        // The next policy switch coincides with a forward index publication
        // which predates its observation. Neither affected page may inherit it.
        let published_at = Utc::now();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        st.snapshot.store(
            snapshot(
                true,
                "pending-forward-publication",
                published_at.to_rfc3339(),
            )
            .await,
        );
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        let pending = latest(&store).await.unwrap().unwrap();
        for path in ["/datasets", curated.as_str()] {
            assert_eq!(pending.pages[path].lastmod, pending.observed_at);
            assert!(pending.pages[path].lastmod > published_at);
        }

        // With a one-field layout, disabling the raw setting changes no
        // effective policy and must not invent a content modification.
        let mut ignored = snapshot(
            false,
            "pending-forward-publication",
            published_at.to_rfc3339(),
        )
        .await;
        Arc::get_mut(&mut ignored)
            .unwrap()
            .refdata
            .current
            .text_layout = Some(2);
        let mut effective = snapshot(
            true,
            "pending-forward-publication",
            published_at.to_rfc3339(),
        )
        .await;
        Arc::get_mut(&mut effective)
            .unwrap()
            .refdata
            .current
            .text_layout = Some(2);
        assert_eq!(
            snapshot_identity(&ignored).unwrap(),
            snapshot_identity(&effective).unwrap()
        );
        assert_eq!(search_policy(&ignored), search_policy(&effective));
        st.snapshot.store(effective);
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        let unchanged_policy = latest(&store).await.unwrap().unwrap();
        st.snapshot.store(ignored);
        refresh(&st, &store, &dir.0, &mut finished).await.unwrap();
        let ignored_setting = latest(&store).await.unwrap().unwrap();
        assert_eq!(ignored_setting.sequence, unchanged_policy.sequence);
        for path in ["/datasets", curated.as_str()] {
            assert_eq!(
                ignored_setting.pages[path].lastmod,
                unchanged_policy.pages[path].lastmod
            );
        }
    }
}
