//! `GET /v1/pages/{doc_id}/layout` (06 §6.3.12, #2026): a page's words and
//! their boxes from our own OCR's stored ALTO, so the site can redraw the
//! page in the layout of the scan.
//!
//! The ALTO is at `curated/ocr-local/alto/<doc_id>.<node>.<stamp>.xml`
//! (12 §12.5 Step 8), one create-only blob per import, so a page re-read
//! has several. One listing by the name stem `<doc_id>.` finds them (a
//! single request, with no read of the directory), and the newest stamp is
//! the one served. Everything after the listing is keyed on that blob's
//! path, which never changes content, so the parsed body is cached by path
//! and the served version (the engine comes from it), and the ETag is the
//! node, stamp and version.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::telemetry::ServedVersion;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::future::{BoxFuture, FutureExt, Shared};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Serialize;
use tokio::sync::Semaphore;
use usnm_alto::{Granularity, ParseOptions};
use usnm_core::page_id::{PageId, Source};
use usnm_store::ObjectStore;

use crate::error::ApiError;
use crate::AppState;

/// The largest ALTO file read. A newspaper page's is about 1 to 3 MB; the
/// ingest's own check allows 32 MiB.
const MAX_ALTO_BYTES: usize = 16 * 1024 * 1024;

/// The most words a response holds. A dense page has some 30,000 to 50,000.
const MAX_WORDS: usize = 150_000;

/// How many ALTO files are read and parsed at once, across all requests. A
/// file is read whole (up to `MAX_ALTO_BYTES`) and parsed into a tree, so
/// this bounds the memory layout builds take within the replica's 0.5 GiB
/// budget (06 §6.6).
const MAX_CONCURRENT_BUILDS: usize = 2;

/// How long a build waits for one of those permits before the request is
/// refused as busy.
const BUILD_WAIT: Duration = Duration::from_secs(2);

/// Where the pickup stores our OCR's ALTO.
const ALTO_DIR: &str = "ocr-local/alto";

/// How long a response may be reused: a re-read page gets a new blob, so
/// the layout a visitor has stays consistent with itself (its word ids
/// carry its stamp), and the ETag lets a client check it cheaply.
const CACHE_CONTROL: &str = "public, max-age=86400";

type Build = Shared<BoxFuture<'static, Result<Arc<Vec<u8>>, ApiError>>>;

/// The layout builds in flight, by cache key, and the permits that bound
/// them. Requests that miss the cache for the same page share one build.
pub struct Builds {
    permits: Arc<Semaphore>,
    wait: Duration,
    running: Mutex<HashMap<String, Build>>,
}

impl Default for Builds {
    fn default() -> Self {
        Self::new(MAX_CONCURRENT_BUILDS, BUILD_WAIT)
    }
}

impl Builds {
    fn new(permits: usize, wait: Duration) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(permits)),
            wait,
            running: Mutex::new(HashMap::new()),
        }
    }

    async fn acquire(&self) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
        match tokio::time::timeout(self.wait, self.permits.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(permit),
            _ => Err(ApiError::Busy),
        }
    }
}

/// A page's layout from one ALTO file.
#[derive(Serialize, JsonSchema)]
#[schemars(rename = "PageLayout")]
struct LayoutResponse {
    /// The page's id, source-qualified (`loc:` and the LoC `doc_id`).
    doc_id: String,
    /// Who read the page, as `/v1/hits` names it in `ocr.source`: the
    /// engine the served index version holds for the page. `null` when the
    /// version doesn't hold our text for it (a page read after the version
    /// was built, or one whose text LoC's kept).
    engine: Option<String>,
    /// The ALTO file's stamp. A word's id is `{stamp}:{b}:{l}:{i}`, which
    /// names that word in this file only: a later reading has another stamp.
    stamp: String,
    /// The OCR node that wrote the file.
    node: String,
    /// The page's size in pixels of the IIIF full image, the unit of every
    /// `rect`.
    width: u32,
    height: u32,
    /// The finest level at which the engine gave real boxes: `word`,
    /// `line`, `block` or `page`. Below `word`, a word's box is its line's.
    granularity: Granularity,
    /// The scan's image. `null`: the API holds no image address for a page
    /// (it never asks loc.gov for one while answering).
    image: Option<ImageOut>,
    /// The text lines in reading order, from blocks with text.
    lines: Vec<LineOut>,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "PageImage")]
struct ImageOut {
    iiif_base: String,
    full: String,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "PageLine")]
struct LineOut {
    /// The line's block, counting every block of the page in order.
    b: u32,
    /// The line's place in its block.
    l: u32,
    /// `[x, y, width, height]` in pixels from the top left.
    rect: [u32; 4],
    words: Vec<WordOut>,
}

#[derive(Serialize, JsonSchema)]
#[schemars(rename = "PageWord")]
struct WordOut {
    /// The word's place in its line.
    i: u32,
    /// The text.
    t: String,
    /// `[x, y, width, height]` in pixels from the top left.
    rect: [u32; 4],
    /// The engine's confidence, 0 to 1; `null` when it gave none.
    c: Option<f64>,
}

/// The schema of the response, for `/v1/openapi.json`.
pub(crate) fn schema(gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
    gen.subschema_for::<LayoutResponse>()
}

/// `[x, y, width, height]` of `r`, in whole pixels (the stored ALTO has
/// nothing finer), clamped to the frame.
fn rect(r: &usnm_alto::Rect, frame: (u32, u32)) -> [u32; 4] {
    let px = |v: f64, max: u32| (v.round().max(0.0) as u32).min(max);
    let (x0, y0) = (px(r.x0, frame.0), px(r.y0, frame.1));
    let (x1, y1) = (px(r.x1, frame.0), px(r.y1, frame.1));
    [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)]
}

/// The node and stamp in an ALTO path's name, given the page's stem.
fn node_and_stamp<'a>(path: &'a str, doc_id: &str) -> Option<(&'a str, &'a str)> {
    let name = path.strip_prefix(ALTO_DIR)?.strip_prefix('/')?;
    let rest = name.strip_prefix(doc_id)?.strip_prefix('.')?;
    let (node, stamp) = rest.strip_suffix(".xml")?.rsplit_once('.')?;
    (!node.is_empty() && !stamp.is_empty()).then_some((node, stamp))
}

/// The newest ALTO among `paths`: the greatest stamp (they are UTC times of
/// one width, so they order as text), then the greatest node.
fn newest<'a>(paths: &'a [String], doc_id: &str) -> Option<(&'a str, &'a str, &'a str)> {
    paths
        .iter()
        .filter_map(|p| node_and_stamp(p, doc_id).map(|(n, s)| (p.as_str(), n, s)))
        .max_by(|a, b| (a.2.len(), a.2, a.1).cmp(&(b.2.len(), b.2, b.1)))
}

/// A file read at most `max` bytes: `None` if it isn't there.
async fn read_capped(
    store: &dyn ObjectStore,
    path: &str,
    max: usize,
) -> Result<Option<Vec<u8>>, ApiError> {
    let Some(mut chunks) = store
        .get_stream(path)
        .await
        .map_err(|e| ApiError::Backend(e.to_string()))?
    else {
        return Ok(None);
    };
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        body.extend_from_slice(&chunk.map_err(|e| ApiError::Backend(e.to_string()))?);
        if body.len() > max {
            return Err(ApiError::PageTooLarge(format!(
                "this page's ALTO is over {max} bytes"
            )));
        }
    }
    Ok(Some(body))
}

/// Whether `If-None-Match` names `etag` (or is `*`).
fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|t| {
                let t = t.trim();
                t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag
            })
        })
}

fn with_cache(mut resp: Response, etag: &str, version: &str) -> Response {
    // The snapshot's version, which the ETag carries: telemetry records it.
    resp.extensions_mut()
        .insert(ServedVersion(version.to_owned()));
    let headers = resp.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    if let Ok(v) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, v);
    }
    resp
}

/// One page's layout.
pub async fn layout(
    State(state): State<Arc<AppState>>,
    Path(raw): Path<String>,
    request_headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id: PageId = raw
        .parse()
        .map_err(|e: usnm_core::page_id::PageIdError| ApiError::BadRequest(e.to_string()))?;
    // Only LoC pages have stored ALTO under their bare id.
    let not_found = || ApiError::NotFound(format!("no layout for page `{id}`"));
    if id.source() != Source::Loc {
        return Err(not_found());
    }
    let Some(store) = state.alto.clone() else {
        return Err(not_found());
    };
    let doc_id = id.local().to_owned();
    let stem = format!("{ALTO_DIR}/{doc_id}.");
    let paths = store
        .list_stem(&stem)
        .await
        .map_err(|e| ApiError::Backend(e.to_string()))?;
    let Some((path, node, stamp)) = newest(&paths, &doc_id) else {
        return Err(not_found());
    };
    // One snapshot for the whole request: the engine below comes from it,
    // so its version is part of both identities.
    let snap = state.snapshot.load_full();
    let version = snap.refdata.version();
    let etag = format!("\"{node}.{stamp}.{version}\"");
    if not_modified(&request_headers, &etag) {
        return Ok(with_cache(
            StatusCode::NOT_MODIFIED.into_response(),
            &etag,
            version,
        ));
    }
    let key = format!("layout|{version}|{path}");
    let body = match state.cache.get(&key).await {
        Some(body) => body,
        None => {
            let engine = {
                let ocr = &snap.refdata.ocr_local;
                ocr.get(&doc_id)
                    .and_then(|e| ocr.engine(e))
                    .map(|e| e.source.clone())
            };
            let job = Job {
                state: state.clone(),
                store,
                key: key.clone(),
                path: path.to_owned(),
                doc: id.to_string(),
                engine,
                stamp: stamp.to_owned(),
                node: node.to_owned(),
            };
            let build = {
                let mut running = state
                    .layouts
                    .running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                running
                    .entry(key)
                    .or_insert_with(|| {
                        let task = tokio::spawn(job.run());
                        async move { task.await.map_err(|e| ApiError::Backend(e.to_string()))? }
                            .boxed()
                            .shared()
                    })
                    .clone()
            };
            build.await?
        }
    };
    let mut resp = body.as_ref().clone().into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(with_cache(resp, &etag, version))
}

/// One layout build: it runs in a task of its own, so it finishes (and
/// frees its permit) even if every request waiting on it goes away.
struct Job {
    state: Arc<AppState>,
    store: Arc<dyn ObjectStore>,
    key: String,
    path: String,
    doc: String,
    engine: Option<String>,
    stamp: String,
    node: String,
}

impl Job {
    async fn run(self) -> Result<Arc<Vec<u8>>, ApiError> {
        let result = self.build().await;
        // The body is cached before the entry goes, so a request finds the
        // build or its result.
        if let Ok(body) = &result {
            self.state
                .cache
                .insert(self.key.clone(), body.clone())
                .await;
        }
        self.state
            .layouts
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        result
    }

    async fn build(&self) -> Result<Arc<Vec<u8>>, ApiError> {
        let permit = self.state.layouts.acquire().await?;
        // A build that finished while this one waited.
        if let Some(body) = self.state.cache.get(&self.key).await {
            return Ok(body);
        }
        let Some(xml) = read_capped(self.store.as_ref(), &self.path, MAX_ALTO_BYTES).await? else {
            // Gone since the listing.
            return Err(ApiError::NotFound(format!(
                "no layout for page `{}`",
                self.doc
            )));
        };
        let (doc, engine, stamp, node) = (
            self.doc.clone(),
            self.engine.clone(),
            self.stamp.clone(),
            self.node.clone(),
        );
        // The permit stays with the parse until it ends.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            build(&xml, doc, engine, stamp, node)
        })
        .await
        .map_err(|e| ApiError::Backend(e.to_string()))?
        .map(Arc::new)
    }
}

/// The response body for one ALTO file.
fn build(
    xml: &[u8],
    doc_id: String,
    engine: Option<String>,
    stamp: String,
    node: String,
) -> Result<Vec<u8>, ApiError> {
    let page = usnm_alto::parse(xml, &ParseOptions::default()).map_err(|e| {
        // The pickup checked the file, so this is ours to look at.
        tracing::error!(%doc_id, error = %e, "stored ALTO doesn't parse");
        ApiError::Backend("a stored ALTO file doesn't parse".to_owned())
    })?;
    let frame = (page.width.round() as u32, page.height.round() as u32);
    let words: usize = page
        .blocks
        .iter()
        .flat_map(|b| &b.lines)
        .map(|l| l.words.len())
        .sum();
    if words > MAX_WORDS {
        return Err(ApiError::PageTooLarge(format!(
            "this page has {words} words; the most served is {MAX_WORDS}"
        )));
    }
    let mut lines = Vec::new();
    for (b, block) in page.blocks.iter().enumerate() {
        for (l, line) in block.lines.iter().enumerate() {
            lines.push(LineOut {
                b: b as u32,
                l: l as u32,
                rect: rect(&line.rect, frame),
                words: line
                    .words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| WordOut {
                        i: i as u32,
                        t: w.text.clone(),
                        rect: rect(&w.rect, frame),
                        c: w.conf
                            .map(|c| (c.clamp(0.0, 1.0) * 1000.0).round() / 1000.0),
                    })
                    .collect(),
            });
        }
    }
    serde_json::to_vec(&LayoutResponse {
        doc_id,
        engine,
        stamp,
        node,
        width: frame.0,
        height: frame.1,
        granularity: page.granularity,
        image: None,
        lines,
    })
    .map_err(|e| ApiError::Backend(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builds_past_the_permits_are_refused_as_busy() {
        let builds = Builds::new(1, Duration::from_millis(50));
        let held = builds.acquire().await.unwrap();
        assert!(matches!(builds.acquire().await, Err(ApiError::Busy)));
        drop(held);
        assert!(builds.acquire().await.is_ok());
    }

    #[test]
    fn both_responses_carry_the_served_version() {
        for resp in [StatusCode::OK, StatusCode::NOT_MODIFIED] {
            let resp = with_cache(resp.into_response(), "\"n.s.v9\"", "v9");
            let served = resp.extensions().get::<ServedVersion>();
            assert_eq!(served.map(|v| v.0.as_str()), Some("v9"));
        }
    }

    fn paths(names: &[&str]) -> Vec<String> {
        names
            .iter()
            .map(|n| format!("{ALTO_DIR}/{n}.xml"))
            .collect()
    }

    #[test]
    fn the_newest_stamp_wins_whatever_the_node() {
        let ps = paths(&[
            "a_1896-01-02_ed-1_seq-3.node2.20261101T000000Z",
            "a_1896-01-02_ed-1_seq-3.node1.20261102T000000Z",
            "a_1896-01-02_ed-1_seq-30.node1.20270101T000000Z",
        ]);
        let (path, node, stamp) = newest(&ps, "a_1896-01-02_ed-1_seq-3").unwrap();
        assert_eq!((node, stamp), ("node1", "20261102T000000Z"));
        assert!(path.ends_with("node1.20261102T000000Z.xml"));
        assert!(newest(&ps, "a_1896-01-02_ed-1_seq-4").is_none());
    }

    #[test]
    fn rects_are_whole_pixels_inside_the_frame() {
        let r = usnm_alto::Rect::new(-3.0, 10.4, 120.6, 5000.0);
        assert_eq!(rect(&r, (100, 200)), [0, 10, 100, 190]);
    }

    #[test]
    fn if_none_match_lists() {
        let mut h = HeaderMap::new();
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"x\", W/\"n.s\""),
        );
        assert!(not_modified(&h, "\"n.s\""));
        assert!(!not_modified(&h, "\"n.t\""));
    }
}
