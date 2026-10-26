//! The pacer, the IIIF base builders and the item API route.

use super::super::*;
use std::sync::atomic::{AtomicU32, Ordering};

#[tokio::test(start_paused = true)]
async fn the_pacer_spaces_concurrent_callers() {
    let pacer = Arc::new(Pacer::new(Duration::from_millis(3500)));
    let t0 = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let p = pacer.clone();
        tasks.push(tokio::spawn(async move {
            p.wait().await;
            Instant::now() - t0
        }));
    }
    let mut at: Vec<Duration> = Vec::new();
    for t in tasks {
        at.push(t.await.unwrap());
    }
    at.sort();
    assert_eq!(
        at,
        [0, 3500, 7000, 10500].map(Duration::from_millis).to_vec()
    );
    // A caller already waiting when a hold is set waits it out too.
    let p = pacer.clone();
    let t2 = Instant::now();
    let waiting = tokio::spawn(async move {
        p.wait().await;
        Instant::now()
    });
    tokio::task::yield_now().await;
    pacer.hold(Duration::from_secs(30));
    assert!(waiting.await.unwrap() - t2 >= Duration::from_secs(30));
    // Told to hold: the next caller waits it out.
    pacer.hold(Duration::from_secs(60));
    let t1 = Instant::now();
    pacer.wait().await;
    assert_eq!(Instant::now() - t1, Duration::from_secs(60));
}

#[test]
fn builds_iiif_bases_from_the_item_json() {
    let item = serde_json::json!({"resources": [{"files": [
        [{"mimetype": "image/jp2", "url": "https://tile.loc.gov/storage-services/service/ndnp/nbu/nbu_gage_ver01/data/sn83045462/00271743700/1890050301/0365.jp2"},
         {"mimetype": "text/xml", "url": "https://tile.loc.gov/x.xml"}],
        [{"mimetype": "image/jp2", "url": "https://tile.loc.gov/storage-services/service/ndnp/nbu/nbu_gage_ver01/data/sn83045462/00271743700/1890050301/0364.jp2"}],
        [{"mimetype": "text/xml", "url": "https://tile.loc.gov/y.xml"}]
    ]}]});
    let pages = page_images("https://tile.loc.gov/image-services/iiif/", &item);
    assert_eq!(pages.len(), 2);
    assert_eq!(
        pages[&1],
        "https://tile.loc.gov/image-services/iiif/service:ndnp:nbu:nbu_gage_ver01:data:sn83045462:00271743700:1890050301:0365"
    );
    assert!(pages[&2].ends_with(":0364"));
    assert_eq!(iiif_base("r/", "https://elsewhere/x.jp2"), None);
    assert_eq!(
        iiif_base(
            "r/",
            "https://tile.loc.gov/storage-services/service/a/../b.jp2"
        ),
        None
    );
}

/// A stand-in loc.gov: one issue (two pages), counting item requests;
/// the first image request is answered 429 with Retry-After, the
/// second 408.
async fn fake_loc(items: Arc<AtomicU32>, images: Arc<AtomicU32>) -> String {
    use axum::http::StatusCode as S;
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let jp2 = |n: &str| {
        format!("https://tile.loc.gov/storage-services/service/ndnp/x/batch_x/data/sn1/1/1890050301/{n}.jp2")
    };
    let item = serde_json::json!({"resources": [{"files": [
        [{"mimetype": "image/jp2", "url": jp2("0001")}],
        [{"mimetype": "image/jp2", "url": jp2("0002")}],
    ]}]});
    let app = axum::Router::new()
        .route(
            "/item/sn1/1890-05-03/ed-1/",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let item = item.clone();
                let items = items.clone();
                async move {
                    assert!(headers["user-agent"]
                        .to_str()
                        .unwrap()
                        .starts_with("usnewsmap-ocr-worker/"));
                    items.fetch_add(1, Ordering::SeqCst);
                    axum::Json(item)
                }
            }),
        )
        .route(
            "/iiif/{service}/full/full/0/default.jpg",
            axum::routing::get(move || {
                let images = images.clone();
                async move {
                    match images.fetch_add(1, Ordering::SeqCst) {
                        0 => (S::TOO_MANY_REQUESTS, [("retry-after", "2")]).into_response(),
                        1 => S::REQUEST_TIMEOUT.into_response(),
                        _ => b"jpeg".to_vec().into_response(),
                    }
                }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    root
}

#[tokio::test]
async fn one_item_request_per_issue_and_retry_after_is_honoured() {
    let (items, images) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let root = fake_loc(items.clone(), images.clone()).await;
    let loc = LocClient::new(LocConfig {
        api_gap: Duration::from_millis(10),
        tile_gap: Duration::from_millis(10),
        attempts: 3,
        backoff: Duration::from_millis(10),
        api_root: root.clone(),
        iiif_root: format!("{root}/iiif/"),
        ..LocConfig::default()
    })
    .unwrap();
    let t0 = std::time::Instant::now();
    let p1: PageKey = "sn1/1890-05-03/ed-1/seq-1".parse().unwrap();
    let p2: PageKey = "sn1/1890-05-03/ed-1/seq-2".parse().unwrap();
    let s1 = loc.scan(&p1).await.unwrap();
    assert!(
        t0.elapsed() >= Duration::from_secs(2),
        "waited out Retry-After"
    );
    // The 408 was retried, not taken as permanent.
    assert_eq!(&s1.bytes[..], b"jpeg");
    assert_eq!(images.load(Ordering::SeqCst), 3);
    assert!(s1
        .iiif_base
        .ends_with("service:ndnp:x:batch_x:data:sn1:1:1890050301:0001"));
    loc.scan(&p2).await.unwrap();
    assert_eq!(items.load(Ordering::SeqCst), 1);
    let p3: PageKey = "sn1/1890-05-03/ed-1/seq-3".parse().unwrap();
    assert!(matches!(loc.scan(&p3).await, Err(FetchError::Permanent(_))));
    // An issue loc.gov doesn't have: permanent, and asked once.
    let other: PageKey = "sn1/1890-05-04/ed-1/seq-1".parse().unwrap();
    assert!(matches!(
        loc.scan(&other).await,
        Err(FetchError::Permanent(_))
    ));
    assert!(matches!(
        loc.scan(&other).await,
        Err(FetchError::Permanent(_))
    ));
}

/// A stand-in loc.gov whose image server answers its first request
/// with 403, and whose API refuses one issue with 404; counts both.
async fn blocking_loc(images: Arc<AtomicU32>, refused: Arc<AtomicU32>) -> String {
    use axum::http::StatusCode as S;
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let item = serde_json::json!({"resources": [{"files": [
        [{"mimetype": "image/jp2", "url": "https://tile.loc.gov/storage-services/service/ndnp/x/batch_x/data/sn1/1/1890050301/0001.jp2"}],
    ]}]});
    let app = axum::Router::new()
        .route(
            "/item/sn1/1890-05-03/ed-1/",
            axum::routing::get(move || {
                let item = item.clone();
                async move { axum::Json(item) }
            }),
        )
        .route(
            "/item/sn1/1890-05-04/ed-1/",
            axum::routing::get(move || {
                let refused = refused.clone();
                async move {
                    refused.fetch_add(1, Ordering::SeqCst);
                    S::NOT_FOUND
                }
            }),
        )
        .route(
            "/iiif/{service}/full/full/0/default.jpg",
            axum::routing::get(move || {
                let images = images.clone();
                async move {
                    match images.fetch_add(1, Ordering::SeqCst) {
                        0 => S::FORBIDDEN.into_response(),
                        _ => b"jpeg".to_vec().into_response(),
                    }
                }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    root
}

#[tokio::test]
async fn a_403_holds_the_server_and_an_issue_refusal_isnt_cached() {
    let (images, refused) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let root = blocking_loc(images.clone(), refused.clone()).await;
    let loc = LocClient::new(LocConfig {
        api_gap: Duration::from_millis(10),
        tile_gap: Duration::from_millis(10),
        attempts: 3,
        backoff: Duration::from_millis(300),
        api_root: root.clone(),
        iiif_root: format!("{root}/iiif/"),
        ..LocConfig::default()
    })
    .unwrap();
    assert_eq!(loc.held_until(), None);
    let t0 = std::time::Instant::now();
    let p1: PageKey = "sn1/1890-05-03/ed-1/seq-1".parse().unwrap();
    // The 403 held the image server and was retried: not permanent.
    let s1 = loc.scan(&p1).await.unwrap();
    assert_eq!(&s1.bytes[..], b"jpeg");
    assert_eq!(images.load(Ordering::SeqCst), 2);
    assert!(t0.elapsed() >= Duration::from_millis(300), "held");
    // A 4xx on an issue fails the page, and is asked again next time.
    let other: PageKey = "sn1/1890-05-04/ed-1/seq-1".parse().unwrap();
    for _ in 0..2 {
        assert!(matches!(
            loc.scan(&other).await,
            Err(FetchError::Permanent(_))
        ));
    }
    assert_eq!(refused.load(Ordering::SeqCst), 2);
}

#[test]
fn parses_image_sizes() {
    assert_eq!("full".parse(), Ok(ImageSize::Full));
    assert_eq!("scaled:50".parse(), Ok(ImageSize::Pct(50)));
    for bad in ["scaled:0", "scaled:100", "scaled:", "half", "pct:50"] {
        assert!(bad.parse::<ImageSize>().is_err(), "{bad}");
    }
    assert_eq!(ImageSize::Pct(50).to_string(), "scaled:50");
    assert_eq!(
        image_url(
            "https://tile.loc.gov/image-services/iiif/service:a:b",
            ImageSize::Pct(50)
        ),
        "https://tile.loc.gov/image-services/iiif/service:a:b/full/pct:50/0/default.jpg"
    );
}

#[tokio::test(start_paused = true)]
async fn the_pacer_reports_its_hold() {
    let pacer = Pacer::new(Duration::from_millis(10));
    assert_eq!(pacer.held_until(), None);
    pacer.hold(Duration::from_secs(3600));
    let until = pacer.held_until().unwrap();
    assert_eq!(until - Instant::now(), Duration::from_secs(3600));
    tokio::time::sleep(Duration::from_secs(3601)).await;
    assert_eq!(pacer.held_until(), None);
}
