//! Item errors (#414): an instant 503 for one item while others answer.

use super::super::*;

/// How the stand-in answers an item.
#[derive(Clone, Copy)]
enum Answer {
    Ok,
    /// 503 at once, no Retry-After: a broken item.
    Bare503,
    /// 503 with `Retry-After: n`: a hold.
    RetryAfter(u32),
    /// 503 after 2.5 s, no Retry-After.
    Slow503,
    Too429,
    /// 503 at once with a `Retry-After` that can't be read.
    BadRetryAfter,
}

/// A stand-in loc.gov answering `/item/<lccn>/<date>/ed-1/` as
/// `answers` says (keyed by `<lccn>/<date>`, anything else 404), and
/// counting requests per item.
async fn items_loc(
    answers: Vec<(&'static str, Answer)>,
) -> (String, Arc<Mutex<BTreeMap<String, u32>>>) {
    use axum::http::StatusCode as S;
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let answers: Arc<BTreeMap<String, Answer>> = Arc::new(
        answers
            .into_iter()
            .map(|(k, a)| (k.to_owned(), a))
            .collect(),
    );
    let hits: Arc<Mutex<BTreeMap<String, u32>>> = Arc::default();
    let h = hits.clone();
    let app = axum::Router::new().route(
        "/item/{lccn}/{date}/{ed}/",
        axum::routing::get(
            move |axum::extract::Path((lccn, date, _ed)): axum::extract::Path<(
                String,
                String,
                String,
            )>| {
                let answers = answers.clone();
                let hits = h.clone();
                async move {
                    let k = format!("{lccn}/{date}");
                    *hits.lock().unwrap().entry(k.clone()).or_default() += 1;
                    match answers.get(&k) {
                        None => S::NOT_FOUND.into_response(),
                        Some(Answer::Ok) => axum::Json(serde_json::json!({"resources": [{"files": [
                            [{"mimetype": "image/jp2", "url": "https://tile.loc.gov/storage-services/service/ndnp/x/batch_x/data/sn1/1/1890050301/0001.jp2"}],
                        ]}]}))
                        .into_response(),
                        Some(Answer::Bare503) => S::SERVICE_UNAVAILABLE.into_response(),
                        Some(Answer::RetryAfter(n)) => {
                            (S::SERVICE_UNAVAILABLE, [("retry-after", n.to_string())])
                                .into_response()
                        }
                        Some(Answer::Slow503) => {
                            tokio::time::sleep(Duration::from_millis(2500)).await;
                            S::SERVICE_UNAVAILABLE.into_response()
                        }
                        Some(Answer::Too429) => S::TOO_MANY_REQUESTS.into_response(),
                        Some(Answer::BadRetryAfter) => {
                            (S::SERVICE_UNAVAILABLE, [("retry-after", "soon")]).into_response()
                        }
                    }
                }
            },
        ),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (root, hits)
}

fn items_client(root: &str, attempts: u32) -> LocClient {
    LocClient::new(LocConfig {
        api_gap: Duration::from_millis(10),
        tile_gap: Duration::from_millis(10),
        attempts,
        backoff: Duration::from_millis(400),
        api_root: root.to_owned(),
        iiif_root: format!("{root}/iiif/"),
        ..LocConfig::default()
    })
    .unwrap()
}

fn page(lccn: &str, date: &str) -> PageKey {
    format!("{lccn}/{date}/ed-1/seq-1").parse().unwrap()
}

/// Ask for `key`'s issue only (no image).
async fn item(loc: &LocClient, key: &PageKey) -> Result<Pages, FetchError> {
    loc.issue_pages(key).await
}

#[tokio::test]
async fn an_instant_503_for_one_item_while_others_answer_is_an_item_error() {
    let (root, hits) = items_loc(vec![
        ("sn1/1904-07-29", Answer::Ok),
        ("sn1/1904-07-30", Answer::Bare503),
        ("sn1/1904-07-31", Answer::Bare503),
    ])
    .await;
    let loc = items_client(&root, 3);
    item(&loc, &page("sn1", "1904-07-29")).await.unwrap();
    let t0 = std::time::Instant::now();
    let e = item(&loc, &page("sn1", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Item(_)), "{e:?}");
    // Nothing held and no retries.
    assert_eq!(loc.held_until(), None);
    assert!(t0.elapsed() < Duration::from_millis(400));
    assert_eq!(hits.lock().unwrap()["sn1/1904-07-30"], 1);
    // The title's next broken item: an item answered within the
    // minute, so no check is needed.
    let e = item(&loc, &page("sn1", "1904-07-31")).await.unwrap_err();
    assert!(matches!(e, FetchError::Item(_)), "{e:?}");
    assert_eq!(hits.lock().unwrap()["sn1/1904-07-29"], 1);
    assert_eq!(loc.held_until(), None);
}

#[tokio::test]
async fn real_holds_still_hold() {
    let (root, hits) = items_loc(vec![
        ("sn1/1904-07-29", Answer::Ok),
        ("sn2/1904-07-30", Answer::RetryAfter(1)),
        ("sn3/1904-07-30", Answer::Too429),
        ("sn4/1904-07-30", Answer::Slow503),
        ("sn5/1904-07-30", Answer::BadRetryAfter),
    ])
    .await;
    let loc = items_client(&root, 1);
    item(&loc, &page("sn1", "1904-07-29")).await.unwrap();
    // A 503 with Retry-After is a hold, though another item answered.
    let e = item(&loc, &page("sn2", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    item(&loc, &page("sn1", "1904-07-29")).await.unwrap();
    // A 429 is a hold.
    let e = item(&loc, &page("sn3", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
    tokio::time::sleep(Duration::from_millis(500)).await;
    // A slow 503 is a hold.
    let e = item(&loc, &page("sn4", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
    assert_eq!(hits.lock().unwrap()["sn4/1904-07-30"], 1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    item(&loc, &page("sn1", "1904-07-29")).await.unwrap();
    // A Retry-After that can't be read is still a hold.
    let e = item(&loc, &page("sn5", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
}

#[tokio::test]
async fn with_nothing_known_good_an_instant_503_holds() {
    let (root, _) = items_loc(vec![("sn1/1904-07-30", Answer::Bare503)]).await;
    let loc = items_client(&root, 1);
    let e = item(&loc, &page("sn1", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
}

#[tokio::test]
async fn an_old_known_good_item_is_asked_again_first() {
    let (root, hits) = items_loc(vec![
        ("sn1/1904-07-29", Answer::Ok),
        ("sn1/1904-07-30", Answer::Bare503),
        ("sn9/1904-07-29", Answer::Bare503),
        ("sn9/1904-07-30", Answer::Bare503),
    ])
    .await;
    // From an earlier run: checked with one request, which answers.
    let loc = items_client(&root, 1);
    loc.set_known_good(loc.item_url(&page("sn1", "1904-07-29")));
    let e = item(&loc, &page("sn1", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Item(_)), "{e:?}");
    assert_eq!(hits.lock().unwrap()["sn1/1904-07-29"], 1);
    assert_eq!(loc.held_until(), None);
    // A known-good item that fails too: a hold.
    let loc = items_client(&root, 1);
    loc.set_known_good(loc.item_url(&page("sn9", "1904-07-29")));
    let e = item(&loc, &page("sn9", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
}

/// With item probes off, a fast bare 503 sends nothing more: the known
/// good item isn't asked again, and the error is a hold.
#[tokio::test]
async fn without_item_probes_a_503_asks_nothing_more() {
    let (root, hits) = items_loc(vec![
        ("sn1/1904-07-29", Answer::Ok),
        ("sn1/1904-07-30", Answer::Bare503),
    ])
    .await;
    let loc = LocClient::new(LocConfig {
        api_gap: Duration::from_millis(10),
        tile_gap: Duration::from_millis(10),
        attempts: 1,
        backoff: Duration::from_millis(400),
        api_root: root.clone(),
        iiif_root: format!("{root}/iiif/"),
        item_probes: false,
        ..LocConfig::default()
    })
    .unwrap();
    loc.set_known_good(loc.item_url(&page("sn1", "1904-07-29")));
    let e = item(&loc, &page("sn1", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(!hits.lock().unwrap().contains_key("sn1/1904-07-29"));
}

#[tokio::test]
async fn item_errors_across_titles_become_a_hold() {
    let (root, _) = items_loc(vec![
        ("sn1/1904-07-29", Answer::Ok),
        ("sn2/1904-07-30", Answer::Bare503),
        ("sn3/1904-07-30", Answer::Bare503),
        ("sn4/1904-07-30", Answer::Bare503),
    ])
    .await;
    let loc = items_client(&root, 1);
    item(&loc, &page("sn1", "1904-07-29")).await.unwrap();
    for t in ["sn2", "sn3"] {
        let e = item(&loc, &page(t, "1904-07-30")).await.unwrap_err();
        assert!(matches!(e, FetchError::Item(_)), "{t}: {e:?}");
    }
    assert_eq!(loc.held_until(), None);
    // A third title inside the window: loc.gov, not the items.
    let e = item(&loc, &page("sn4", "1904-07-30")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
}

#[tokio::test]
async fn concurrent_item_errors_after_a_restart_still_count_titles() {
    let (root, _) = items_loc(vec![
        ("sn1/1904-07-29", Answer::Ok),
        ("sn2/1904-07-30", Answer::Bare503),
        ("sn3/1904-07-30", Answer::Bare503),
        ("sn4/1904-07-30", Answer::Bare503),
        ("sn5/1904-07-30", Answer::Bare503),
    ])
    .await;
    let loc = Arc::new(items_client(&root, 1));
    // Known good from an earlier run: each caller checks it first.
    loc.set_known_good(loc.item_url(&page("sn1", "1904-07-29")));
    let mut tasks = Vec::new();
    for t in ["sn2", "sn3", "sn4", "sn5"] {
        let loc = loc.clone();
        tasks.push(tokio::spawn(async move {
            matches!(
                item(&loc, &page(t, "1904-07-30")).await,
                Err(FetchError::Item(_))
            )
        }));
    }
    let mut items = 0;
    for t in tasks {
        items += usize::from(t.await.unwrap());
    }
    assert_eq!(items, ITEM_ERROR_TITLES - 1);
    assert!(loc.held_until().is_some());
}
