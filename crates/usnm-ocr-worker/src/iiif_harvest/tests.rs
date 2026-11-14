//! Tests against a stand-in server on this machine, serving documents written by hand from
//! the IIIF Presentation 2 and Image 2 specifications (not copies of any real site's
//! responses). The pace is injected: the tests go at tens of milliseconds, and a separate
//! test checks that a real run cannot go below three seconds. Nothing here can reach another
//! host: the policy refuses non-loopback hosts when compiled for tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tokio::sync::watch;

use super::pace::{self, Pace};
use super::robots::Robots;
use super::{fetch, iiif, plan, Action, InfoJson, Options, Summary};

const GAP: Duration = Duration::from_millis(40);

#[derive(Clone)]
struct Route {
    status: u16,
    ct: &'static str,
    body: Vec<u8>,
    retry_after: Option<&'static str>,
    location: Option<&'static str>,
    /// Answer 503 with `Retry-After: 0` this many times first.
    fail_first: usize,
}

#[derive(Default)]
struct Site {
    routes: Mutex<HashMap<String, Route>>,
    hits: Mutex<Vec<(String, Instant)>>,
}

impl Site {
    fn put(&self, path: &str, ct: &'static str, body: Vec<u8>) {
        self.routes.lock().unwrap().insert(
            path.to_owned(),
            Route {
                status: 200,
                ct,
                body,
                retry_after: None,
                location: None,
                fail_first: 0,
            },
        );
    }

    fn json(&self, path: &str, v: &Value) {
        self.put(path, "application/json", serde_json::to_vec(v).unwrap());
    }

    fn tweak(&self, path: &str, f: impl FnOnce(&mut Route)) {
        f(self.routes.lock().unwrap().get_mut(path).unwrap());
    }

    fn hits_for(&self, prefix: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with(prefix))
            .count()
    }
}

async fn serve(State(site): State<Arc<Site>>, uri: Uri) -> Response {
    let path = uri.path().to_owned();
    site.hits
        .lock()
        .unwrap()
        .push((path.clone(), Instant::now()));
    let route = {
        let mut routes = site.routes.lock().unwrap();
        match routes.get_mut(&path) {
            Some(r) if r.fail_first > 0 => {
                r.fail_first -= 1;
                let mut r = r.clone();
                r.status = 503;
                r.retry_after = Some("0");
                Some(r)
            }
            other => other.cloned(),
        }
    };
    let Some(r) = route else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut resp = (
        StatusCode::from_u16(r.status).unwrap(),
        [("content-type", r.ct)],
        r.body,
    )
        .into_response();
    if let Some(v) = r.location {
        resp.headers_mut().insert("location", v.parse().unwrap());
    }
    if let Some(v) = r.retry_after {
        resp.headers_mut().insert("retry-after", v.parse().unwrap());
    }
    resp
}

async fn start() -> (Arc<Site>, String) {
    let site = Arc::new(Site::default());
    let app = axum::Router::new()
        .fallback(serve)
        .with_state(Arc::clone(&site));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (site, base)
}

fn jpeg(tag: u8) -> Vec<u8> {
    let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, tag];
    b.extend_from_slice(&[7u8; 64]);
    b.extend_from_slice(&[0xFF, 0xD9]);
    b
}

fn canvas(
    base: &str,
    m: &str,
    n: usize,
    service: Option<Value>,
    resource: Option<(&str, &str)>,
) -> Value {
    let mut res = match resource {
        Some((id, fmt)) => json!({"@id": id, "@type": "dctypes:Image", "format": fmt}),
        None => json!({"@type": "dctypes:Image", "format": "image/jpeg"}),
    };
    if let Some(s) = service {
        res["service"] = s;
    }
    json!({
        "@id": format!("{base}/{m}/canvas/{n}"), "@type": "sc:Canvas", "label": format!("p. {n}"),
        "height": 3000, "width": 2000,
        "images": [{"@type": "oa:Annotation", "motivation": "sc:painting",
                    "on": format!("{base}/{m}/canvas/{n}"), "resource": res}]
    })
}

fn manifest(base: &str, m: &str, label: &str, canvases: Vec<Value>) -> Value {
    json!({
        "@context": "http://iiif.io/api/presentation/2/context.json",
        "@id": format!("{base}/{m}/manifest"), "@type": "sc:Manifest", "label": label,
        "attribution": "Holding Partner and The Archive",
        "license": "http://example.invalid/terms",
        "metadata": [{"label": "Persistent Link", "value": format!("{base}/ark/{m}")},
                     {"label": "Date", "value": "1880-01-02"}],
        "related": format!("{base}/item/{m}"),
        "sequences": [{"@type": "sc:Sequence", "canvases": canvases}]
    })
}

fn svc(base: &str, name: &str, profile: Option<Value>) -> Value {
    let mut s = json!({"@context": "http://iiif.io/api/image/2/context.json",
                       "@id": format!("{base}/img/{name}")});
    if let Some(p) = profile {
        s["profile"] = p;
    }
    s
}

/// A paged collection of four manifests: `first` -> page 1 (`manifests`, `next`) -> page 2
/// (`members` with a manifest and a sub-collection) -> the sub-collection (`manifests`).
fn site_with_fixtures(site: &Site, base: &str) -> usize {
    site.put(
        "/robots.txt",
        "text/plain",
        b"User-agent: *\nCrawl-delay: 0\nDisallow: /private/\n".to_vec(),
    );
    site.json(
        "/top",
        &json!({
            "@context": "http://iiif.io/api/presentation/2/context.json",
            "@id": format!("{base}/top"), "@type": "sc:Collection", "viewingHint": "top",
            "attribution": "The Archive", "total": 4,
            "first": format!("{base}/page/1"), "last": format!("{base}/page/2"),
            "license": "http://example.invalid/terms"
        }),
    );
    site.json(
        "/page/1",
        &json!({
            "@id": format!("{base}/page/1"), "@type": "sc:Collection",
            "manifests": [{"@id": format!("{base}/m1/manifest"), "@type": "sc:Manifest"},
                          {"@id": "/m2/manifest", "@type": "sc:Manifest"}],
            "next": format!("{base}/page/2")
        }),
    );
    site.json(
        "/page/2",
        &json!({
            "@id": format!("{base}/page/2"), "@type": "sc:Collection",
            "members": [{"@id": format!("{base}/m3/manifest"), "@type": "sc:Manifest"},
                        {"@id": format!("{base}/sub"), "@type": "sc:Collection"}]
        }),
    );
    site.json(
        "/sub/p1",
        &json!({
            "@id": format!("{base}/sub/p1"), "@type": "sc:Collection",
            "manifests": [{"@id": format!("{base}/m4/manifest"), "@type": "sc:Manifest"}]
        }),
    );
    // A paged sub-collection: a wrapper with only `first`.
    site.json(
        "/sub",
        &json!({
            "@id": format!("{base}/sub"), "@type": "sc:Collection",
            "first": format!("{base}/sub/p1")
        }),
    );
    let big =
        json!(["http://iiif.io/api/image/2/level1.json", {"maxWidth": 1000, "maxHeight": 1500}]);
    // m1: a service with an embedded profile limiting the width; a service with no profile
    // (so its info.json is asked for).
    site.json(
        "/m1/manifest",
        &manifest(
            base,
            "m1",
            "Issue One",
            vec![
                canvas(base, "m1", 1, Some(svc(base, "a", Some(big))), None),
                canvas(base, "m1", 2, Some(svc(base, "b", None)), None),
            ],
        ),
    );
    site.json(
        "/img/b/info.json",
        &json!({
            "@context": "http://iiif.io/api/image/2/context.json", "@id": format!("{base}/img/b"),
            "protocol": "http://iiif.io/api/image", "width": 2000, "height": 3000,
            "profile": ["http://iiif.io/api/image/2/level1.json", {"maxArea": 1_500_000}]
        }),
    );
    // m2: no service, only the resource's own @id.
    site.json(
        "/m2/manifest",
        &manifest(
            base,
            "m2",
            "Issue Two",
            vec![canvas(
                base,
                "m2",
                1,
                None,
                Some((&format!("{base}/direct/two.png"), "image/png")),
            )],
        ),
    );
    // m3: one image that is not there.
    site.json(
        "/m3/manifest",
        &manifest(
            base,
            "m3",
            "Issue Three",
            vec![
                canvas(
                    base,
                    "m3",
                    1,
                    Some(svc(
                        base,
                        "gone",
                        Some(json!("http://iiif.io/api/image/2/level0.json")),
                    )),
                    None,
                ),
                canvas(
                    base,
                    "m3",
                    2,
                    Some(svc(
                        base,
                        "c",
                        Some(json!("http://iiif.io/api/image/2/level0.json")),
                    )),
                    None,
                ),
            ],
        ),
    );
    // m4: the service as an array.
    site.json(
        "/m4/manifest",
        &manifest(
            base,
            "m4",
            "Issue Four",
            vec![canvas(
                base,
                "m4",
                1,
                Some(json!([svc(
                    base,
                    "d",
                    Some(json!("http://iiif.io/api/image/2/level2.json"))
                )])),
                None,
            )],
        ),
    );
    site.put("/img/a/full/1000,/0/default.jpg", "image/jpeg", jpeg(1));
    site.put("/img/b/full/1000,/0/default.jpg", "image/jpeg", jpeg(2));
    site.put(
        "/direct/two.png",
        "image/png",
        b"\x89PNG\r\n\x1a\n-----IEND\xaeB`\x82".to_vec(),
    );
    site.put("/img/c/full/full/0/default.jpg", "image/jpeg", jpeg(4));
    site.put("/img/d/full/full/0/default.jpg", "image/jpeg", jpeg(5));
    6
}

fn opts(base: &str, out: &std::path::Path, action: Action) -> Options {
    Options {
        action,
        collection: Some(format!("{base}/top")),
        out: out.to_owned(),
        state_dir: out.join("_hoststate"),
        plan: None,
        pace: Pace::for_test(GAP),
        allow_hosts: Vec::new(),
        info_json: InfoJson::Auto,
        max_dimension: None,
        limit: None,
        max_image_bytes: 1 << 20,
    }
}

fn stop() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

async fn run(o: Options) -> anyhow::Result<Summary> {
    let (_tx, rx) = stop();
    super::run(o, rx).await
}

fn read_plan(out: &std::path::Path) -> plan::Plan {
    serde_json::from_slice(&std::fs::read(out.join("plan.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn plan_follows_paging_collections_and_both_image_paths() {
    let (site, base) = start().await;
    let images = site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    let s = run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    assert!(s.clean(), "{s:?}");
    let p = read_plan(dir.path());
    assert_eq!(p.issues_count, 4);
    assert_eq!(p.images_count, images);
    assert_eq!(p.terms_url.as_deref(), Some("http://example.invalid/terms"));
    let ids: Vec<&str> = p.issues.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m2", "m3", "m4"]);
    let urls: Vec<&str> = p
        .issues
        .iter()
        .flat_map(|i| i.canvases.iter().map(|c| c.url.as_str()))
        .collect();
    // The width limit in the embedded profile, and the area limit from info.json.
    assert!(
        urls[0].ends_with("/img/a/full/1000,/0/default.jpg"),
        "{urls:?}"
    );
    assert!(
        urls[1].ends_with("/img/b/full/1000,/0/default.jpg"),
        "{urls:?}"
    );
    // No service: the resource's own @id, with its extension.
    assert!(urls[2].ends_with("/direct/two.png"), "{urls:?}");
    assert_eq!(p.issues[1].canvases[0].ext, "png");
    // A level 0 service with no larger size to choose: `full`.
    assert!(urls[4].ends_with("/img/c/full/full/0/default.jpg"));
    // info.json was asked for only where the manifest gave no profile.
    assert_eq!(site.hits_for("/img/b/info.json"), 1);
    assert_eq!(site.hits_for("/img/a/info.json"), 0);
    // No image request in a plan; the documents are kept as served.
    assert_eq!(site.hits_for("/img/a/full"), 0);
    assert_eq!(site.hits_for("/direct"), 0);
    let kept = std::fs::read(dir.path().join("_iiif/manifests/m1.json")).unwrap();
    let served = site.routes.lock().unwrap()["/m1/manifest"].body.clone();
    assert_eq!(kept, served);
    // Counts and an estimate at the pace.
    assert_eq!(p.estimated_requests, images);
    assert_eq!(
        p.estimated_seconds,
        (images as f64 * GAP.as_secs_f64()).ceil() as u64
    );
    assert!(dir.path().join("_iiif/collections/00000.json").exists());
    assert!(dir.path().join("_iiif/robots").read_dir().unwrap().count() == 1);
}

#[tokio::test]
async fn plan_limit_and_paging_without_first() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts(&base, dir.path(), Action::Plan);
    o.limit = Some(2);
    let s = run(o).await.unwrap();
    assert_eq!(s.issues, 2);
}

#[tokio::test]
async fn fetch_writes_files_sidecars_citations_and_resumes() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    let s = run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    // m3's first image is a 404: reported, the rest go on.
    assert_eq!(s.fetched, 5, "{s:?}");
    assert_eq!(s.failed.len(), 1, "{s:?}");
    assert!(s.failed[0].contains("m3/0001"), "{s:?}");
    assert!(!s.clean());
    let jpg = dir.path().join("m1/0001.jpg");
    assert_eq!(std::fs::read(&jpg).unwrap(), jpeg(1));
    let sc: super::Sidecar =
        serde_json::from_slice(&std::fs::read(dir.path().join("m1/0001.jpg.json")).unwrap())
            .unwrap();
    assert_eq!(sc.bytes, jpeg(1).len() as u64);
    assert_eq!(sc.sha256, crate::ghn::sha256_hex(&jpeg(1)));
    assert_eq!(sc.sniffed.as_deref(), Some("jpeg"));
    assert!(sc.user_agent.contains("usnewsmap"));
    let cite = sc.citation.unwrap();
    assert_eq!(cite.manifest_id, format!("{base}/m1/manifest"));
    assert_eq!(cite.label.unwrap(), json!("Issue One"));
    assert_eq!(
        cite.attribution.unwrap(),
        json!("Holding Partner and The Archive")
    );
    assert!(cite.license.is_some() && cite.metadata.is_some());
    assert_eq!(cite.links[0], format!("{base}/item/m1"));
    assert!(cite.links.contains(&format!("{base}/ark/m1")));
    assert_eq!(
        sc.terms_url.as_deref(),
        Some("http://example.invalid/terms")
    );
    assert!(sc.accessed.ends_with('Z'));
    assert!(dir.path().join("m2/0001.png").exists());
    // The list of everything fetched, and the citations.
    let log = std::fs::read_to_string(dir.path().join("fetched.jsonl")).unwrap();
    assert!(log.lines().count() >= 5 + 4, "{log}");
    let tsv = std::fs::read_to_string(dir.path().join("CITATIONS.tsv")).unwrap();
    let rows: Vec<&str> = tsv.lines().collect();
    assert_eq!(rows[0], "issue_label\tlink\tattribution\taccessed");
    assert!(
        rows.iter().any(|r| r.starts_with(&format!(
            "Issue One\t{base}/item/m1\tHolding Partner and The Archive\t20"
        ))),
        "{tsv}"
    );
    assert_eq!(rows.len(), 1 + 4);
    // Requests were spaced at the injected pace.
    let hits = site.hits.lock().unwrap().clone();
    for w in hits.windows(2) {
        assert!(
            w[1].1 - w[0].1 >= GAP.mul_f32(0.8),
            "{:?} then {:?}",
            w[0].0,
            w[1].0
        );
    }
    // A second run fetches only what failed.
    let before = site.hits_for("/img/");
    let s2 = run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    assert_eq!(s2.fetched, 0);
    assert_eq!(s2.already_have, 5);
    assert_eq!(
        site.hits_for("/img/") - before,
        1,
        "only the 404 is asked for again"
    );
    // Nothing was removed or replaced.
    assert_eq!(std::fs::read(&jpg).unwrap(), jpeg(1));
}

#[tokio::test]
async fn a_file_without_a_matching_sidecar_is_kept_and_not_fetched() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    std::fs::create_dir_all(dir.path().join("m1")).unwrap();
    std::fs::write(dir.path().join("m1/0001.jpg"), b"someone else's bytes").unwrap();
    let s = run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    assert_eq!(s.kept_unverified, 1);
    assert_eq!(
        std::fs::read(dir.path().join("m1/0001.jpg")).unwrap(),
        b"someone else's bytes"
    );
    assert_eq!(site.hits_for("/img/a/full"), 0);
}

#[tokio::test]
async fn fetch_limit_stops_after_that_many_images() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    let mut o = opts(&base, dir.path(), Action::Fetch);
    o.limit = Some(2);
    let s = run(o).await.unwrap();
    assert_eq!(s.fetched, 2);
    assert_eq!(s.estimated_requests, 4);
}

#[tokio::test]
async fn robots_refusal_stops_before_any_image() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    // The site now bars the image paths for everyone.
    site.put(
        "/robots.txt",
        "text/plain",
        b"User-agent: *\nDisallow: /img/\n".to_vec(),
    );
    let other = tempfile::tempdir().unwrap();
    std::fs::copy(dir.path().join("plan.json"), other.path().join("plan.json")).unwrap();
    let err = run(opts(&base, other.path(), Action::Fetch))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("robots.txt bars"), "{err:#}");
    assert_eq!(site.hits_for("/img/a/full"), 0);
    // A rule naming this tool's own token refuses too.
    site.put(
        "/robots.txt",
        "text/plain",
        b"User-agent: usnewsmap-ocr-worker\nDisallow: /\n".to_vec(),
    );
    let third = tempfile::tempdir().unwrap();
    std::fs::copy(dir.path().join("plan.json"), third.path().join("plan.json")).unwrap();
    assert!(run(opts(&base, third.path(), Action::Fetch)).await.is_err());
    // A plan against a barred collection path refuses at once.
    site.put(
        "/robots.txt",
        "text/plain",
        b"User-agent: *\nDisallow: /top\n".to_vec(),
    );
    let fourth = tempfile::tempdir().unwrap();
    let tops = site.hits_for("/top");
    let err = run(opts(&base, fourth.path(), Action::Plan))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("robots.txt bars"), "{err:#}");
    assert_eq!(site.hits_for("/top"), tops);
}

#[tokio::test]
async fn a_longer_crawl_delay_than_the_pace_refuses_to_run() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    site.put(
        "/robots.txt",
        "text/plain",
        b"User-agent: *\nCrawl-delay: 9\n".to_vec(),
    );
    let dir = tempfile::tempdir().unwrap();
    let err = run(opts(&base, dir.path(), Action::Plan))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("--min-gap-secs 9"), "{err:#}");
}

#[tokio::test]
async fn retry_after_is_honoured_and_a_429_stops_the_run() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    // A 503 with Retry-After: 0 twice, then the image.
    site.tweak("/img/a/full/1000,/0/default.jpg", |r| r.fail_first = 2);
    let s = run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    assert!(dir.path().join("m1/0001.jpg").exists(), "{s:?}");
    assert_eq!(site.hits_for("/img/a/full"), 3);
    // A 429 with a long Retry-After ends the run and holds the budget for the next one.
    let other = tempfile::tempdir().unwrap();
    std::fs::copy(dir.path().join("plan.json"), other.path().join("plan.json")).unwrap();
    site.tweak("/img/a/full/1000,/0/default.jpg", |r| {
        r.status = 429;
        r.retry_after = Some("120");
    });
    let s = run(opts(&base, other.path(), Action::Fetch)).await.unwrap();
    assert!(s.stopped.is_some(), "{s:?}");
    assert_eq!(s.fetched, 0);
    assert_eq!(
        site.hits_for("/img/b/full"),
        1,
        "only the earlier image; nothing after the 429"
    );
    let held = run(opts(&base, other.path(), Action::Fetch)).await;
    assert!(held.is_err(), "the budget is held after a push-back");
}

#[tokio::test]
async fn a_host_the_documents_name_must_be_allowed() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    site.json(
        "/m1/manifest",
        &manifest(
            &base,
            "m1",
            "x",
            vec![canvas(
                &base,
                "m1",
                1,
                Some(json!({"@id": "http://localhost:1/img/z"})),
                None,
            )],
        ),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts(&base, dir.path(), Action::Plan);
    o.info_json = InfoJson::Never;
    // Plan records the URL; fetch refuses the host it has not been told to ask.
    run(o).await.unwrap();
    let err = run(opts(&base, dir.path(), Action::Fetch))
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("--allow-host localhost"),
        "{err:#}"
    );
}

#[tokio::test]
async fn plan_needs_a_collection_and_none_is_built_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts("http://127.0.0.1:9", dir.path(), Action::Plan);
    o.collection = None;
    assert!(run(o).await.is_err());
}

#[test]
fn the_pace_cannot_go_below_three_seconds() {
    assert!(Pace::new(0).is_err() && Pace::new(2).is_err());
    let p = Pace::new(3).unwrap();
    assert_eq!(p.gap(), Duration::from_secs(3));
    let pol = pace::policy(&["127.0.0.1".to_owned()], p).unwrap();
    assert_eq!(pol.min_gap_secs, 3);
    assert_eq!(pol.requests_per_minute, 20);
    assert_eq!(pol.concurrency, 1);
    let slower = pace::policy(&["127.0.0.1".to_owned()], Pace::new(7).unwrap()).unwrap();
    assert_eq!((slower.min_gap_secs, slower.requests_per_minute), (7, 8));
}

#[test]
fn the_user_agent_is_honest() {
    let ua = crate::loc::USER_AGENT.to_lowercase();
    assert!(ua.contains("usnewsmap") && ua.contains("https://usnewsmap.com"));
    for bad in ["claude", "anthropic", "googlebot", "bingbot", "mozilla"] {
        assert!(!ua.contains(bad), "{ua}");
    }
}

#[test]
fn robots_rules() {
    let r = Robots::parse(
        "User-agent: ClaudeBot\nDisallow: /\n\nUser-agent: *\nCrawl-delay: 3\nRequest-rate: 1/5\n\
         Disallow: /a/\nAllow: /a/ok\nDisallow: /*.pdf$\n",
    );
    let me = "usnewsmap-ocr-worker";
    assert!(r.allows(me, "/x") && !r.allows(me, "/a/b") && r.allows(me, "/a/ok"));
    assert!(!r.allows(me, "/f.pdf") && r.allows(me, "/f.pdf?x=1"));
    assert!(
        !r.allows("claudebot", "/x"),
        "the group naming it applies to it"
    );
    assert_eq!(r.delay_secs(me), 5.0);
    assert!(Robots::allow_all().allows(me, "/anything"));
    // A rule for this tool's token and one for `*` both apply.
    let r =
        Robots::parse("User-agent: usnewsmap-ocr-worker\nAllow: /\nUser-agent: *\nDisallow: /z\n");
    assert!(!r.allows(me, "/z"));
}

#[test]
fn image_sizes_respect_the_service_limits() {
    let d = (Some(2000), Some(3000));
    assert_eq!(iiif::size(None, d, None).unwrap(), "full");
    assert_eq!(iiif::size(None, (None, None), Some(500)).unwrap(), "full");
    let w = json!({"profile": ["x/level1.json", {"maxWidth": 1000}]});
    assert_eq!(iiif::size(Some(&w), d, None).unwrap(), "666,");
    let h = json!({"profile": ["x/level1.json", {"maxHeight": 1500}]});
    assert_eq!(iiif::size(Some(&h), d, None).unwrap(), "1000,");
    let area = json!({"profile": ["x/level2.json", {"maxArea": 600_000}]});
    assert_eq!(iiif::size(Some(&area), d, None).unwrap(), "632,");
    assert_eq!(iiif::size(None, d, Some(1500)).unwrap(), "1000,");
    let l0 = json!({"profile": "http://iiif.io/api/image/2/level0.json",
                    "sizes": [{"width": 200, "height": 300}, {"width": 800, "height": 1200}]});
    assert_eq!(iiif::size(Some(&l0), d, Some(1500)).unwrap(), "800,1200");
    assert_eq!(iiif::size(Some(&l0), d, None).unwrap(), "full");
    assert_eq!(
        iiif::size(Some(&l0), d, Some(100)),
        None,
        "nothing small enough: skipped, not full"
    );
}

#[test]
fn collection_pages_in_every_shape() {
    let doc = json!({"members": [{"@id": "m", "@type": "sc:Manifest"}, {"@id": "c", "@type": "sc:Collection"}],
                     "collections": [{"@id": "d"}], "manifests": ["n"], "next": {"@id": "p2"}, "first": "p1", "total": 3});
    let c = iiif::collection(&doc, "http://127.0.0.1/x/y");
    assert_eq!(
        c.manifests,
        ["http://127.0.0.1/x/n", "http://127.0.0.1/x/m"]
    );
    assert_eq!(
        c.collections,
        ["http://127.0.0.1/x/d", "http://127.0.0.1/x/c"]
    );
    assert_eq!(c.next.as_deref(), Some("http://127.0.0.1/x/p2"));
    assert_eq!(c.first.as_deref(), Some("http://127.0.0.1/x/p1"));
    assert_eq!(c.total, Some(3));
}

#[test]
fn issue_ids_are_unique_and_safe() {
    let urls: Vec<String> = [
        "http://h/a/b/manifest",
        "http://h/c/b/manifest",
        "http://h/ok.json",
        "http://h/_x",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let ids = plan::issue_ids(&urls);
    assert!(ids[0].starts_with("b-") && ids[1].starts_with("b-") && ids[0] != ids[1]);
    assert_eq!(ids[2], "ok");
    assert_eq!(ids[3], "i_x");
    assert_eq!(
        fetch::Have::No,
        fetch::have(std::path::Path::new("/nonexistent/x"), "", "")
    );
}

#[tokio::test]
async fn a_page_that_is_not_an_image_is_kept_aside_and_reported() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    // A 200 with an error document, and a truncated JPEG.
    site.put(
        "/img/c/full/full/0/default.jpg",
        "application/json",
        b"{\"error\": \"busy\"}".to_vec(),
    );
    site.put(
        "/img/d/full/full/0/default.jpg",
        "image/jpeg",
        vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
    );
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    let s = run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    assert_eq!(s.failed.len(), 3, "{s:?}");
    assert_eq!(s.fetched, 3);
    assert!(!dir.path().join("m3/0002.jpg").exists());
    assert!(
        std::fs::read_dir(dir.path().join("m3")).unwrap().count() >= 1,
        "kept beside the name"
    );
    let log = std::fs::read_to_string(dir.path().join("fetched.jsonl")).unwrap();
    assert!(!log.contains("m3/0002"), "no success line for a bad image");
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    site.tweak("/img/c/full/full/0/default.jpg", |r| {
        r.status = 302;
        r.location = Some("/private/secret.jpg");
    });
    site.put("/private/secret.jpg", "image/jpeg", jpeg(9));
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    let s = run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    assert!(s.failed.iter().any(|f| f.contains("redirect")), "{s:?}");
    assert_eq!(site.hits_for("/private/"), 0);
}

#[tokio::test]
async fn another_collections_saved_documents_are_not_reused() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    let (site2, base2) = start().await;
    site_with_fixtures(&site2, &base2);
    let err = run(opts(&base2, dir.path(), Action::Plan))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("not the saved copy"), "{err:#}");
}

#[tokio::test]
async fn a_replan_at_another_size_does_not_pass_old_files_for_new() {
    let (site, base) = start().await;
    site_with_fixtures(&site, &base);
    let dir = tempfile::tempdir().unwrap();
    run(opts(&base, dir.path(), Action::Plan)).await.unwrap();
    run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    let mut o = opts(&base, dir.path(), Action::Plan);
    o.max_dimension = Some(400);
    run(o).await.unwrap();
    site.put("/img/a/full/266,/0/default.jpg", "image/jpeg", jpeg(8));
    let s = run(opts(&base, dir.path(), Action::Fetch)).await.unwrap();
    assert!(s.kept_unverified >= 1, "{s:?}");
    assert_eq!(
        std::fs::read(dir.path().join("m1/0001.jpg")).unwrap(),
        jpeg(1)
    );
    assert_eq!(
        site.hits_for("/img/a/full/266"),
        0,
        "kept, not fetched over"
    );
}

#[test]
fn robots_match_percent_encoded_paths() {
    let r = Robots::parse("User-agent: *\nDisallow: /private/\nDisallow: /a%2Fb\n");
    let me = "usnewsmap-ocr-worker";
    assert!(!r.allows(me, "/%70rivate/x.jpg"));
    assert!(!r.allows(me, "/a%2fb"));
    assert!(r.allows(me, "/a/b"), "an encoded slash is not a slash");
    assert!(r.allows(me, "/public/%C3%A9"));
}

#[tokio::test]
async fn runs_against_one_host_share_one_lock_across_output_directories() {
    let shared = tempfile::tempdir().unwrap();
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_tx, rx) = stop();
    let hosts = ["127.0.0.1".to_owned()];
    let first = super::Client::new(
        a.path(),
        shared.path(),
        &hosts,
        Pace::for_test(GAP),
        rx.clone(),
    )
    .unwrap();
    let second = super::Client::new(b.path(), shared.path(), &hosts, Pace::for_test(GAP), rx);
    assert!(second.is_err(), "a second run on the same host is refused");
    drop(first);
}
