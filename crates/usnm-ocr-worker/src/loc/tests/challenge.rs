//! Bot challenges and other HTML answers, which hold the server.

use super::super::*;
use super::{batch_client, key, BATCH};
use crate::standin::{Scripted, StandIn};

/// The compatibility opt-out still works; production callers enable
/// the flag and hold at once.
#[tokio::test]
async fn an_html_answer_is_a_hold_only_when_asked() {
    let s = StandIn::start().await;
    let path = format!("{BATCH}data/sn1/0001A/1890050301/0001.xml");
    let url = format!("{}{path}", s.root);
    s.put(&path, b"<alto/>".to_vec());
    s.script(&path, Scripted::Challenge(200));
    let worker = batch_client(&s, Duration::from_millis(10), None, false);
    let body = worker
        .storage_file(&url, "sn1", 1 << 20)
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("Just a moment"));
    s.script(&path, Scripted::Challenge(200));
    let staging = LocClient::new(LocConfig {
        html_is_block: true,
        attempts: 1,
        ..worker.cfg.clone()
    })
    .unwrap();
    let e = staging
        .storage_file(&url, "sn1", 1 << 20)
        .await
        .unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(e.to_string().contains("Cloudflare challenge"), "{e}");
    assert!(staging.held_until().is_some());
}

#[tokio::test]
async fn challenges_hold_each_route_without_retry_or_cache() {
    let s = StandIn::start().await;
    for (server, path) in [
        (Server::Api, "/item/sn1/1890-05-03/ed-1/"),
        (Server::Tile, "/iiif/page/full/full/0/default.jpg"),
        (Server::Storage, "/scan.jp2"),
    ] {
        let loc = LocClient::new(LocConfig {
            html_is_block: true,
            backoff: Duration::from_millis(50),
            ..batch_client(&s, Duration::ZERO, None, true).cfg
        })
        .unwrap();
        s.script(path, Scripted::Challenge(200));
        let url = format!("{}{path}", s.root);
        let error = loc
            .get(&url, server, MAX_IMAGE_BYTES, None)
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::Transient(_)), "{error}");
        assert!(
            error.to_string().contains("Cloudflare challenge"),
            "{error}"
        );
        assert_eq!(s.hits(path), 1, "a challenge must not be retried");
        assert!(loc.pacer(server).held_until().is_some());
        if server == Server::Storage {
            assert!(loc.tile.held_until().is_none(), "independent budgets");
        }
    }

    let loc = LocClient::new(LocConfig {
        html_is_block: true,
        ..batch_client(&s, Duration::ZERO, None, false).cfg
    })
    .unwrap();
    let path = "/item/sn1/1890-05-03/ed-1/";
    s.script(path, Scripted::Challenge(200));
    let error = loc.issue_pages(&key(1)).await.unwrap_err();
    assert!(
        error.to_string().contains("Cloudflare challenge"),
        "{error}"
    );
    assert!(loc.items.get(&issue_id(&key(1))).await.is_none());
    let path = "/scan.jp2";
    s.script(path, Scripted::Challenge(200));
    loc.get(&format!("{}{path}", s.root), Server::Storage, 1024, None)
        .await
        .unwrap_err();
    assert!(loc.tile.held_until().is_some(), "shared image/storage hold");
}

#[tokio::test]
async fn a_challenge_probe_is_not_a_healthy_item() {
    let s = StandIn::start().await;
    let path = "/item/sn1/1890-05-03/ed-1/";
    let loc = LocClient::new(LocConfig {
        html_is_block: true,
        ..batch_client(&s, Duration::ZERO, None, false).cfg
    })
    .unwrap();
    s.script(path, Scripted::Challenge(200));
    assert!(matches!(
        loc.probe(Server::Api, &format!("{}{path}", s.root)).await,
        Err(ProbeFailure::Blocked(_))
    ));
    assert!(loc.health.lock().unwrap().last_ok.is_none());
}

#[tokio::test]
async fn a_challenge_probe_stops_the_original_fetch_and_names_the_block() {
    for status in [200, 403] {
        let s = StandIn::start().await;
        let loc = LocClient::new(LocConfig {
            html_is_block: true,
            backoff: Duration::from_millis(100),
            ..batch_client(&s, Duration::ZERO, None, false).cfg
        })
        .unwrap();
        loc.set_known_good(format!("{}/good", s.root));
        s.script("/item", Scripted::Status(503));
        s.script("/good", Scripted::Challenge(status));
        let error = loc
            .get(&format!("{}/item", s.root), Server::Api, 1024, Some("sn2"))
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::Transient(_)), "{error}");
        assert!(error.to_string().contains("known-good item"), "{error}");
        assert!(
            error.to_string().contains("Cloudflare challenge"),
            "{error}"
        );
        assert_eq!(s.hits("/item"), 1, "do not retry after a probe challenge");
        assert_eq!(s.hits("/good"), 1);
        assert!(loc.api.held_until().is_some());
        assert!(loc.health.lock().unwrap().item_errors.is_empty());
    }
}

#[tokio::test]
async fn a_503_challenge_cannot_be_an_item_error_after_a_healthy_item() {
    let s = StandIn::start().await;
    let loc = LocClient::new(LocConfig {
        html_is_block: true,
        attempts: 1,
        backoff: Duration::from_millis(100),
        ..batch_client(&s, Duration::ZERO, None, false).cfg
    })
    .unwrap();
    s.put("/good", b"{}".to_vec());
    loc.get(&format!("{}/good", s.root), Server::Api, 1024, Some("sn1"))
        .await
        .unwrap();
    s.script("/item", Scripted::Challenge(503));
    let error = loc
        .get(&format!("{}/item", s.root), Server::Api, 1024, Some("sn2"))
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Transient(_)), "{error}");
    assert!(
        error.to_string().contains("Cloudflare challenge"),
        "{error}"
    );
    assert!(loc.api.held_until().is_some());
    assert!(loc.health.lock().unwrap().item_errors.is_empty());
}

#[tokio::test]
async fn a_forbidden_challenge_names_the_block_and_holds() {
    let s = StandIn::start().await;
    for server in [Server::Api, Server::Tile, Server::Storage] {
        let loc = LocClient::new(LocConfig {
            html_is_block: true,
            attempts: 1,
            backoff: Duration::from_millis(50),
            ..batch_client(&s, Duration::ZERO, None, false).cfg
        })
        .unwrap();
        s.script("/blocked", Scripted::Challenge(403));
        let error = loc
            .get(&format!("{}/blocked", s.root), server, 1024, None)
            .await
            .unwrap_err();
        assert!(matches!(error, FetchError::Transient(_)), "{error}");
        assert!(error.to_string().contains("HTTP 403"), "{error}");
        assert!(
            error.to_string().contains("Cloudflare challenge"),
            "{error}"
        );
        assert!(loc.pacer(server).held_until().is_some());
    }
}

#[tokio::test]
async fn challenge_holds_are_counted_with_distinct_reasons() {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::{
        data::{AggregatedMetrics, MetricData},
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    };
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let s = StandIn::start().await;
    let mut loc = LocClient::new(LocConfig {
        html_is_block: true,
        attempts: 1,
        ..batch_client(&s, Duration::ZERO, None, true).cfg
    })
    .unwrap();
    loc.metrics = Metrics::new(&provider.meter("loc-test"));
    for (path, response) in [
        ("/html", Scripted::Challenge(200)),
        ("/forbidden", Scripted::Challenge(403)),
        ("/busy", Scripted::Status(429)),
    ] {
        s.script(path, response);
        loc.get(&format!("{}{path}", s.root), Server::Api, 1024, None)
            .await
            .unwrap_err();
    }
    loc.block_hold(Server::Tile, StatusCode::OK, "unreadable_body", None);
    loc.set_known_good(format!("{}/good", s.root));
    s.script("/probe-item", Scripted::Status(503));
    s.script("/good", Scripted::ChallengeRetryAfter(403, 7200));
    let error = loc
        .get(
            &format!("{}/probe-item", s.root),
            Server::Api,
            1024,
            Some("sn2"),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("Cloudflare challenge"),
        "{error}"
    );
    assert_eq!(s.hits("/probe-item"), 1);
    let remaining = loc.api.held_until().unwrap() - Instant::now();
    assert!(remaining > Duration::from_secs(3599), "{remaining:?}");
    assert!(remaining <= MAX_RETRY_AFTER, "{remaining:?}");
    provider.force_flush().unwrap();
    let mut reasons = Vec::new();
    let mut capped_probe = false;
    for resource in exporter.get_finished_metrics().unwrap() {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics().filter(|m| m.name() == "ocr.loc_slowdowns") {
                let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
                    panic!("slowdowns must remain a counter");
                };
                for point in sum.data_points() {
                    assert_eq!(point.value(), 1);
                    let attrs: Vec<_> = point.attributes().cloned().collect();
                    if attrs.contains(&KeyValue::new("capped", true)) {
                        assert!(attrs.contains(&KeyValue::new("retry_after", true)));
                        assert!(attrs.contains(&KeyValue::new("status", 403_i64)));
                        assert!(attrs.contains(&KeyValue::new("server", "api")));
                        assert!(attrs.contains(&KeyValue::new("reason", "html_challenge")));
                        capped_probe = true;
                    }
                    reasons.extend(
                        point
                            .attributes()
                            .filter(|a| a.key.as_str() == "reason")
                            .map(|a| a.value.to_string()),
                    );
                }
            }
        }
    }
    reasons.sort();
    assert!(
        capped_probe,
        "the probe must preserve capped Retry-After metrics"
    );
    assert_eq!(
        reasons,
        [
            "html_challenge",
            "html_challenge",
            "html_challenge",
            "http_status",
            "unreadable_body"
        ]
    );
}

#[tokio::test]
async fn an_unreadable_body_holds_without_retry() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/scan.jp2", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() < 4096);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort")
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });
    let loc = LocClient::new(LocConfig {
        html_is_block: true,
        attempts: 3,
        backoff: Duration::from_millis(100),
        ..LocConfig::default()
    })
    .unwrap();
    let error = loc.get(&url, Server::Tile, 1024, None).await.unwrap_err();
    assert!(matches!(error, FetchError::Transient(_)), "{error}");
    assert!(
        error.to_string().contains("body could not be read"),
        "{error}"
    );
    assert!(error.to_string().contains("challenge"), "{error}");
    assert!(!error.to_string().contains("giving up"), "{error}");
    assert!(loc.tile.held_until().is_some());
    server.await.unwrap();
}

#[tokio::test]
async fn html_content_types_and_header_only_challenges_are_blocks() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route(
            "/html",
            axum::routing::get(|| async {
                ([("content-type", "Text/HTML; charset=utf-8")], "<html/>")
            }),
        )
        .route(
            "/xhtml",
            axum::routing::get(|| async {
                ([("content-type", "application/xhtml+xml")], "<html/>")
            }),
        )
        .route(
            "/header",
            axum::routing::get(|| async { ([("cf-mitigated", "challenge")], "blocked") }),
        )
        .route(
            "/xml",
            axum::routing::get(|| async { ([("content-type", "application/xml")], "<mets/>") }),
        )
        .route(
            "/json",
            axum::routing::get(|| async { ([("content-type", "application/json")], "{}") }),
        )
        .route(
            "/text",
            axum::routing::get(|| async { ([("content-type", "text/plain")], "manifest") }),
        )
        .route(
            "/image",
            axum::routing::get(|| async { ([("content-type", "image/jpeg")], "jpeg") }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for path in ["html", "xhtml", "header", "xml", "json", "text", "image"] {
        let loc = LocClient::new(LocConfig {
            html_is_block: true,
            ..LocConfig::default()
        })
        .unwrap();
        let got = loc
            .get(&format!("{root}/{path}"), Server::Tile, 1024, None)
            .await;
        if matches!(path, "html" | "xhtml" | "header") {
            assert!(
                matches!(got, Err(FetchError::Transient(_))),
                "{path}: {got:?}"
            );
            assert!(loc.held_until().is_some(), "{path}");
        } else {
            assert!(got.is_ok(), "{path}: {got:?}");
            assert!(loc.held_until().is_none(), "{path}");
        }
    }
    server.abort();
}

/// The batch lookup through Chronicling America: for staging, a
/// challenge page or a 5xx is a hold, not an absent batch.
#[tokio::test]
async fn the_batch_lookup_holds_on_a_challenge_or_server_error_when_asked() {
    let s = StandIn::start().await;
    let via = "/data/batches/zz_none_ver01/manifest-md5.txt";
    let worker = batch_client(&s, Duration::from_millis(10), None, false);
    let staging = |s: &StandIn| {
        LocClient::new(LocConfig {
            html_is_block: true,
            attempts: 1,
            ..batch_client(s, Duration::from_millis(10), None, false)
                .cfg
                .clone()
        })
        .unwrap()
    };
    s.script(via, Scripted::Status(500));
    assert!(worker.bag("zz_none", 1).await.unwrap().is_none());
    s.script(via, Scripted::Status(500));
    let e = staging(&s).bag("zz_none", 1).await.unwrap_err();
    assert!(e.to_string().contains("HTTP 500"), "{e}");
    s.script(via, Scripted::Challenge(200));
    let e = staging(&s).bag("zz_none", 1).await.unwrap_err();
    assert!(e.to_string().contains("Cloudflare challenge"), "{e}");
    // A plain 404 is still an absent batch.
    assert!(staging(&s).bag("zz_none", 1).await.unwrap().is_none());
}

/// A 429 or a challenge drops the host's global budget to the slow rate
/// (#962); a 503 holds only its server.
#[tokio::test]
async fn push_backs_slow_the_global_pacer_and_503s_do_not() {
    let s = StandIn::start().await;
    let dir = tempfile::tempdir().unwrap();
    for (script, slows) in [
        (Scripted::Status(503), false),
        (Scripted::Status(429), true),
        (Scripted::Challenge(200), true),
    ] {
        let file = dir
            .path()
            .join(format!("pacer-{slows}-{}.json", rand_tag()));
        let loc = LocClient::new(LocConfig {
            html_is_block: true,
            attempts: 1,
            backoff: Duration::from_millis(20),
            global_pacer: Some(file.clone()),
            ..batch_client(&s, Duration::ZERO, None, true).cfg
        })
        .unwrap();
        let path = "/busy.jp2";
        s.script(path, script);
        let url = format!("{}{path}", s.root);
        let _ = loc.get(&url, Server::Storage, MAX_IMAGE_BYTES, None).await;
        let st = global::GlobalPacer::new(file).state().unwrap();
        assert_eq!(st.slowed.is_some(), slows, "{st:?}");
        assert!(st.sent >= 1, "every request passes the global pacer");
    }
}

fn rand_tag() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}
