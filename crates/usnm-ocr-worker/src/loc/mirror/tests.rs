use super::*;
use crate::standin::{Scripted, StandIn};

const LOC: &str = "https://tile.loc.gov/storage-services/service/ndnp/xx/batch_xx_test_ver01/";

fn key(seq: u16) -> PageKey {
    PageKey::new("sn1", "1890-05-03".parse().unwrap(), 1, seq).unwrap()
}

/// LoC's page: frame `frame` of the issue, whose bag md5 is that of `jp2`.
fn page(frame: &str, jp2: &[u8]) -> BatchPage {
    BatchPage {
        url: format!("{LOC}data/sn1/0001A/1890050301/{frame}.jp2"),
        iiif_base: format!(
            "https://tile.loc.gov/image-services/iiif/service:ndnp:xx:batch_xx_test_ver01:data:sn1:0001A:1890050301:{frame}"
        ),
        md5: batch::md5(jp2),
    }
}

fn partner(s: &StandIn, json_names_file: bool) -> Partner {
    Partner {
        awardee: "xx",
        name: "partner",
        root: s.root.clone(),
        json_names_file,
    }
}

fn cfg() -> MirrorConfig {
    MirrorConfig {
        partner_gap: Duration::from_millis(10),
        wayback_gap: Duration::from_millis(10),
        backoff: Duration::from_secs(30),
        block_hold: Duration::from_secs(600),
        max_wait: Duration::from_secs(5),
        ..MirrorConfig::default()
    }
}

fn with_partner(s: &StandIn, json_names_file: bool) -> Mirrors {
    Mirrors::new(MirrorConfig {
        partners: vec![partner(s, json_names_file)],
        ..cfg()
    })
    .unwrap()
}

/// The page JSON an Open ONI site gives for a page that is `frame`.
fn canvas(s: &StandIn, frame: &str) -> Vec<u8> {
    let id = format!(
        "{}/images/iiif/batch_xx_test_ver01%2Fdata%2Fsn1%2F0001A%2F1890050301%2F{frame}.jp2",
        s.root
    );
    serde_json::to_vec(&serde_json::json!({
        "@id": id,
        "@type": "sc:Canvas",
        "images": [{"resource": {"@id": id, "service": {"@id": id}}}],
    }))
    .unwrap()
}

const SEQ2: &str = "/lccn/sn1/1890-05-03/ed-1/seq-2";

#[tokio::test]
async fn a_partners_jp2_that_is_the_bags_is_taken_with_locs_url() {
    let s = StandIn::start().await;
    s.put(&format!("{SEQ2}.json"), canvas(&s, "0010"));
    s.put(&format!("{SEQ2}.jp2"), b"jp2 two".to_vec());
    let m = with_partner(&s, true);
    let p = page("0010", b"jp2 two");
    let (scan, hit) = m.jp2("xx_test", &key(2), &p).await.expect("a hit");
    assert_eq!(&scan.bytes[..], b"jp2 two");
    assert_eq!(
        (scan.url.as_str(), scan.iiif_base.as_str()),
        (p.url.as_str(), p.iiif_base.as_str())
    );
    assert_eq!(
        scan.md5.as_deref(),
        Some(batch::md5_hex(b"jp2 two").as_str())
    );
    assert_eq!(scan.format, ScanFormat::Jp2);
    assert_eq!(
        hit,
        Hit {
            source: "partner",
            url: format!("{}{SEQ2}.jp2", s.root)
        }
    );
    let st = &m.status()["partner"];
    assert_eq!((st.requests, st.hits, st.misses, st.holds), (2, 1, 0, 0));
}

#[tokio::test]
async fn another_awardees_batch_isnt_asked_of_a_partner() {
    let s = StandIn::start().await;
    let m = with_partner(&s, true);
    assert!(m
        .jp2("yy_other", &key(2), &page("0010", b"x"))
        .await
        .is_none());
    assert_eq!(s.hits(""), 0);
}

#[tokio::test]
async fn a_partner_page_that_is_another_file_is_seq_drift_and_its_jp2_isnt_asked() {
    let s = StandIn::start().await;
    // The partner's seq 2 is frame 0011; LoC's is 0010.
    s.put(&format!("{SEQ2}.json"), canvas(&s, "0011"));
    s.put(&format!("{SEQ2}.jp2"), b"other".to_vec());
    let m = with_partner(&s, true);
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"jp2 two"))
        .await
        .is_none());
    assert_eq!(s.hits(".jp2"), 0);
    assert_eq!(m.status()["partner"].seq_drift, 1);
}

#[tokio::test]
async fn bytes_that_arent_the_bags_are_refused() {
    let s = StandIn::start().await;
    // South Carolina's JSON names no file, so only the md5 can tell.
    s.put(&format!("{SEQ2}.jp2"), b"not the bag's".to_vec());
    let m = with_partner(&s, false);
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"jp2 two"))
        .await
        .is_none());
    assert_eq!(s.hits(".json"), 0);
    let st = &m.status()["partner"];
    assert_eq!((st.requests, st.md5_mismatches, st.hits), (1, 1, 0));
}

#[tokio::test]
async fn a_page_the_partner_lacks_is_a_miss() {
    let s = StandIn::start().await;
    let m = with_partner(&s, true);
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"x"))
        .await
        .is_none());
    assert_eq!(m.status()["partner"].misses, 1);
    assert_eq!(s.hits(".jp2"), 0);
}

#[tokio::test]
async fn a_429_holds_that_partner_only_and_later_pages_skip_it_without_asking() {
    let s = StandIn::start().await;
    s.script(&format!("{SEQ2}.json"), Scripted::RetryAfter(429, 120));
    let m = with_partner(&s, true);
    let t = Instant::now();
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"x"))
        .await
        .is_none());
    assert!(t.elapsed() < Duration::from_secs(5), "never waited out");
    let st = &m.status()["partner"];
    assert_eq!((st.requests, st.holds), (1, 1));
    assert!(
        st.held_secs.is_some_and(|h| (100..=120).contains(&h)),
        "{st:?}"
    );
    // The next page goes on at once, with nothing sent.
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"x"))
        .await
        .is_none());
    let st = &m.status()["partner"];
    assert_eq!((st.requests, st.skipped_held), (1, 1));
    assert_eq!(s.hits(""), 1);
}

#[tokio::test]
async fn a_challenge_holds_the_partner_for_the_block_hold() {
    let s = StandIn::start().await;
    s.put(&format!("{SEQ2}.json"), canvas(&s, "0010"));
    s.script(&format!("{SEQ2}.jp2"), Scripted::Challenge(200));
    let m = with_partner(&s, true);
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"x"))
        .await
        .is_none());
    let st = &m.status()["partner"];
    assert_eq!(st.holds, 1);
    assert!(st.held_secs.is_some_and(|h| h > 500), "{st:?}");
}

#[tokio::test]
async fn a_403_holds_it_too() {
    let s = StandIn::start().await;
    s.script(&format!("{SEQ2}.json"), Scripted::Status(403));
    let m = with_partner(&s, true);
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"x"))
        .await
        .is_none());
    assert!(m.status()["partner"].held_secs.is_some_and(|h| h > 500));
}

fn wayback(s: &StandIn) -> Mirrors {
    Mirrors::new(MirrorConfig {
        wayback: Some(s.root.clone()),
        ..cfg()
    })
    .unwrap()
}

fn cdx(rows: &[(&str, &str)]) -> Vec<u8> {
    let mut v = vec![serde_json::json!(["timestamp", "original"])];
    v.extend(rows.iter().map(|(t, o)| serde_json::json!([t, o])));
    serde_json::to_vec(&v).unwrap()
}

#[tokio::test]
async fn a_wayback_capture_is_found_through_one_cdx_lookup_per_issue() {
    let s = StandIn::start().await;
    let one = format!("{LOC}data/sn1/0001A/1890050301/0010.jp2");
    let two = format!("{LOC}data/sn1/0001A/1890050301/0011.jp2");
    // An older and a newer capture of 0010, one of 0011, and a file of
    // another issue the prefix also matched.
    s.put(
        "/cdx/search/cdx",
        cdx(&[
            ("20250101000000", one.as_str()),
            ("20250201000000", &one.replace("https://", "http://")),
            ("20250301000000", two.as_str()),
            (
                "20250301000000",
                &format!("{LOC}data/sn1/0001A/1890050301x/0010.jp2"),
            ),
        ]),
    );
    s.put(
        &format!(
            "/web/20250201000000id_/{}",
            one.replace("https://", "http://")
        ),
        b"jp2 ten".to_vec(),
    );
    s.put(
        &format!("/web/20250301000000id_/{two}"),
        b"jp2 eleven".to_vec(),
    );
    let m = wayback(&s);
    let (scan, hit) = m
        .jp2("xx_test", &key(1), &page("0010", b"jp2 ten"))
        .await
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 ten");
    assert_eq!(scan.url, one, "LoC's URL, not the capture's");
    assert_eq!(hit.source, WAYBACK);
    assert!(hit.url.contains("/web/20250201000000id_/"), "{}", hit.url);
    let (scan, _) = m
        .jp2("xx_test", &key(2), &page("0011", b"jp2 eleven"))
        .await
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 eleven");
    // A page not captured is a miss, from the same lookup.
    assert!(m
        .jp2("xx_test", &key(3), &page("0012", b"x"))
        .await
        .is_none());
    assert_eq!(s.hits("/cdx/search/cdx"), 1);
    let st = &m.status()[WAYBACK];
    assert_eq!((st.requests, st.hits, st.misses), (3, 2, 1));
}

#[tokio::test]
async fn a_cdx_lookup_that_couldnt_be_made_isnt_kept() {
    let s = StandIn::start().await;
    s.script("/cdx/search/cdx", Scripted::Status(500));
    let mut c = cfg();
    c.backoff = Duration::from_millis(50);
    let m = Mirrors::new(MirrorConfig {
        wayback: Some(s.root.clone()),
        ..c
    })
    .unwrap();
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    tokio::time::sleep(Duration::from_millis(80)).await;
    // Nothing kept: asked again once the hold is over.
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    assert_eq!(s.hits("/cdx/search/cdx"), 2);
}

#[tokio::test]
async fn a_redirect_off_the_mirrors_host_isnt_followed() {
    let elsewhere = StandIn::start().await;
    elsewhere.put(&format!("{SEQ2}.jp2"), b"jp2 two".to_vec());
    // A partner that sends every request to another host and port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let to = format!("{}{SEQ2}.jp2", elsewhere.root);
    let app = axum::Router::new().fallback(move || {
        let to = to.clone();
        async move {
            (
                axum::http::StatusCode::FOUND,
                [(axum::http::header::LOCATION, to)],
            )
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let m = Mirrors::new(MirrorConfig {
        partners: vec![Partner {
            awardee: "xx",
            name: "partner",
            root,
            json_names_file: false,
        }],
        ..cfg()
    })
    .unwrap();
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"jp2 two"))
        .await
        .is_none());
    assert_eq!(elsewhere.hits(""), 0, "never followed");
    let st = &m.status()["partner"];
    assert_eq!((st.requests, st.misses, st.hits), (1, 1, 0));
}

#[test]
fn named_mirrors() {
    let c =
        MirrorConfig::named(&["nbu".into(), "scu".into(), WAYBACK.into(), "nbu".into()]).unwrap();
    assert_eq!(
        c.partners.iter().map(|p| p.awardee).collect::<Vec<_>>(),
        ["nbu", "scu"]
    );
    assert_eq!(c.wayback.as_deref(), Some(WAYBACK_ROOT));
    assert!(MirrorConfig::named(&[]).unwrap().is_empty());
    assert!(
        MirrorConfig::named(&["gu".into()]).is_err(),
        "Georgia's JP2s are behind a challenge"
    );
}

#[test]
fn reads_the_file_a_page_json_names() {
    // Nebraska's answer for sn 2010270504 1912-02-16 seq 1, trimmed.
    let body = br#"{"@id": "https://nebnewspapers.unl.edu/images/iiif/batch_nbu_irons_ver01%2Fdata%2F2010270504%2F00237284823%2F1912021601%2F0001.jp2",
        "images": [{"resource": {"service": {"@id": "https://nebnewspapers.unl.edu/images/iiif/batch_nbu_irons_ver01%2Fdata%2F2010270504%2F00237284823%2F1912021601%2F0001.jp2"}}}]}"#;
    assert_eq!(
        partner_file(body).as_deref(),
        Some("data/2010270504/00237284823/1912021601/0001.jp2")
    );
    assert_eq!(
        data_path("https://tile.loc.gov/storage-services/service/ndnp/nbu/batch_nbu_irons_ver02/data/2010270504/00237284823/1912021601/0001.jp2"),
        Some("data/2010270504/00237284823/1912021601/0001.jp2"),
        "another version of the batch names the same file"
    );
    assert_eq!(partner_file(b"not json"), None);
    assert_eq!(
        partner_file(br#"{"@id": "https://x/images/iiif/no-data-here"}"#),
        None
    );
    assert_eq!(percent_decode("a%2Fb%2fc%zz%2"), "a/b/c%zz%2");
}

/// A mirror that takes `delay` to answer every request with `body`,
/// counting how many requests it has open at once.
async fn slow_mirror(
    delay: Duration,
    body: &'static [u8],
) -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::AtomicUsize;
    let open = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let (o, m) = (Arc::clone(&open), Arc::clone(&most));
    let app = axum::Router::new().fallback(move || {
        let (o, m) = (Arc::clone(&o), Arc::clone(&m));
        async move {
            let now = o.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            o.fetch_sub(1, Ordering::SeqCst);
            body.to_vec()
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (root, open, most)
}

fn partner_at(root: String, max_wait: Duration) -> Mirrors {
    Mirrors::new(MirrorConfig {
        partners: vec![Partner {
            awardee: "xx",
            name: "partner",
            root,
            json_names_file: false,
        }],
        max_wait,
        ..cfg()
    })
    .unwrap()
}

#[tokio::test]
async fn a_mirror_gets_one_request_at_a_time() {
    let (root, _, most) = slow_mirror(Duration::from_millis(150), b"jp2 two").await;
    let m = Arc::new(partner_at(root, Duration::from_secs(5)));
    let p = page("0010", b"jp2 two");
    let calls: Vec<_> = (0..4)
        .map(|_| {
            let (m, p) = (Arc::clone(&m), p.clone());
            tokio::spawn(async move { m.jp2("xx_test", &key(2), &p).await.is_some() })
        })
        .collect();
    for c in calls {
        assert!(c.await.unwrap(), "each waited its turn and got the page");
    }
    assert_eq!(most.load(Ordering::SeqCst), 1, "never two at once");
    assert_eq!(m.status()["partner"].requests, 4);
}

#[tokio::test]
async fn a_busy_mirror_is_skipped_and_an_idle_one_asked_again() {
    let (root, _, most) = slow_mirror(Duration::from_millis(400), b"jp2 two").await;
    let m = Arc::new(partner_at(root, Duration::from_millis(100)));
    let p = page("0010", b"jp2 two");
    let calls: Vec<_> = (0..3)
        .map(|_| {
            let (m, p) = (Arc::clone(&m), p.clone());
            tokio::spawn(async move { m.jp2("xx_test", &key(2), &p).await.is_some() })
        })
        .collect();
    let mut got = 0;
    for c in calls {
        got += usize::from(c.await.unwrap());
    }
    assert_eq!(got, 1, "the others went on without it");
    let st = &m.status()["partner"];
    assert_eq!((st.requests, st.skipped_held), (1, 2));
    // Nothing was kept for the callers that gave up: once it is idle, the
    // next page is served.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(m.jp2("xx_test", &key(2), &p).await.is_some());
    assert_eq!(m.status()["partner"].requests, 2);
    assert_eq!(most.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_challenge_with_a_503_or_429_holds_for_the_block_hold() {
    for status in [503, 429] {
        let s = StandIn::start().await;
        s.script(&format!("{SEQ2}.json"), Scripted::Challenge(status));
        let m = with_partner(&s, true);
        assert!(m
            .jp2("xx_test", &key(2), &page("0010", b"x"))
            .await
            .is_none());
        let st = &m.status()["partner"];
        assert!(st.held_secs.is_some_and(|h| h > 500), "{status}: {st:?}");
    }
}

#[tokio::test]
async fn a_redirect_within_the_mirror_is_followed_and_counted() {
    let s = StandIn::start().await;
    s.redirect(&format!("{SEQ2}.jp2"), "/files/0010.jp2");
    s.put("/files/0010.jp2", b"jp2 two".to_vec());
    let m = with_partner(&s, false);
    let (scan, hit) = m
        .jp2("xx_test", &key(2), &page("0010", b"jp2 two"))
        .await
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 two");
    assert_eq!(scan.url, page("0010", b"jp2 two").url, "LoC's URL stays");
    assert_eq!(
        hit.url,
        format!("{}/files/0010.jp2", s.root),
        "the URL that sent the bytes"
    );
    assert_eq!(m.status()["partner"].requests, 2);
}

fn cdx_with_lengths(rows: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut v = vec![serde_json::json!(["timestamp", "original", "length"])];
    v.extend(rows.iter().map(|(t, o, l)| serde_json::json!([t, o, l])));
    serde_json::to_vec(&v).unwrap()
}

#[tokio::test]
async fn the_largest_capture_is_tried_first_and_a_cut_short_one_passed_over() {
    let s = StandIn::start().await;
    let one = format!("{LOC}data/sn1/0001A/1890050301/0010.jp2");
    // The largest capture is cut short (its bytes aren't the bag's); the
    // next is whole.
    s.put(
        "/cdx/search/cdx",
        cdx_with_lengths(&[
            ("20250101000000", one.as_str(), "900"),
            ("20250301000000", one.as_str(), "800"),
            ("20250201000000", one.as_str(), "100"),
        ]),
    );
    s.put(&format!("/web/20250101000000id_/{one}"), b"jp2 te".to_vec());
    s.put(
        &format!("/web/20250301000000id_/{one}"),
        b"jp2 ten".to_vec(),
    );
    s.put(&format!("/web/20250201000000id_/{one}"), b"j".to_vec());
    let m = wayback(&s);
    let (scan, hit) = m
        .jp2("xx_test", &key(1), &page("0010", b"jp2 ten"))
        .await
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 ten");
    assert!(hit.url.contains("/web/20250301000000id_/"), "{}", hit.url);
    let st = &m.status()[WAYBACK];
    assert_eq!((st.md5_mismatches, st.hits), (1, 1));
    assert_eq!(
        s.hits(&format!("20250201000000id_/{one}")),
        0,
        "at most two tried"
    );
}

#[tokio::test]
async fn pages_of_an_issue_asked_at_once_share_one_cdx_lookup() {
    let s = StandIn::start().await;
    s.put("/cdx/search/cdx", cdx_with_lengths(&[]));
    let m = Arc::new(wayback(&s));
    let calls: Vec<_> = (1..=4u16)
        .map(|q| {
            let m = Arc::clone(&m);
            tokio::spawn(async move {
                m.jp2("xx_test", &key(q), &page(&format!("{:04}", 10 + q), b"x"))
                    .await
                    .is_none()
            })
        })
        .collect();
    for c in calls {
        assert!(c.await.unwrap());
    }
    assert_eq!(s.hits("/cdx/search/cdx"), 1);
    assert_eq!(m.status()[WAYBACK].misses, 4);
}

#[tokio::test]
async fn a_cdx_answer_that_doesnt_read_isnt_kept() {
    let s = StandIn::start().await;
    s.put("/cdx/search/cdx", b"Error: not a list".to_vec());
    let mut c = cfg();
    c.backoff = Duration::from_millis(50);
    let m = Mirrors::new(MirrorConfig {
        wayback: Some(s.root.clone()),
        ..c
    })
    .unwrap();
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    assert_eq!(m.status()[WAYBACK].holds, 1);
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    assert_eq!(
        s.hits("/cdx/search/cdx"),
        2,
        "asked again, not taken as nothing captured"
    );
}

#[test]
fn reads_a_cdx_answer() {
    let dir = "/storage-services/service/ndnp/xx/batch_xx_test_ver01/data/sn1/0001A/1890050301/";
    let f = |n: &str| format!("https://tile.loc.gov{dir}{n}");
    let body = cdx_with_lengths(&[
        ("20250101000000", &f("0010.jp2"), "5"),
        (
            "20250201000000",
            &f("0010.jp2").replace("https://", "http://"),
            "9",
        ),
        ("20250301000000", &f("sub/0011.jp2"), "9"),
        ("20250301000000", &f("0012.xml"), "9"),
        ("x", &f("0013.jp2"), "9"),
    ]);
    let got = cdx_files(&body, dir).unwrap();
    assert_eq!(got.keys().collect::<Vec<_>>(), ["0010.jp2"]);
    assert_eq!(
        got["0010.jp2"]
            .iter()
            .map(|c| c.ts.as_str())
            .collect::<Vec<_>>(),
        ["20250201000000", "20250101000000"],
        "largest first"
    );
    assert_eq!(cdx_files(b"", dir), Some(BTreeMap::new()));
    assert_eq!(cdx_files(b"[]", dir), Some(BTreeMap::new()));
    assert_eq!(cdx_files(b"oops", dir), None);
}

#[tokio::test]
async fn a_hold_set_while_a_caller_waits_out_the_gap_still_holds() {
    let s = StandIn::start().await;
    s.put(&format!("{SEQ2}.jp2"), b"jp2 two".to_vec());
    let mut c = cfg();
    c.partner_gap = Duration::from_millis(200);
    let m = Arc::new(
        Mirrors::new(MirrorConfig {
            partners: vec![partner(&s, false)],
            ..c
        })
        .unwrap(),
    );
    let p = page("0010", b"jp2 two");
    // One request goes and ends; the next caller then waits out the gap.
    assert!(m.jp2("xx_test", &key(2), &p).await.is_some());
    let waiting = {
        let (m, p) = (Arc::clone(&m), p.clone());
        tokio::spawn(async move { m.jp2("xx_test", &key(2), &p).await.is_some() })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    // A hold lands while it sleeps (as one from a CDX answer that doesn't
    // read would, after its request ended).
    m.partners["xx"].1.gate.hold(Duration::from_secs(30));
    assert!(!waiting.await.unwrap(), "not sent during the hold");
    assert_eq!(m.status()["partner"].requests, 1);
}

#[tokio::test(start_paused = true)]
async fn a_request_given_up_midway_still_makes_the_next_wait_the_gap() {
    let gate = Gate::new(Duration::from_secs(5));
    let wait = Duration::from_secs(30);
    // The caller gives up while its request is out: its future drops.
    let given_up = tokio::time::timeout(Duration::from_secs(1), async {
        let _turn = gate.turn(Instant::now() + wait).await.unwrap();
        let _sending = gate.sending();
        std::future::pending::<()>().await;
    })
    .await;
    assert!(given_up.is_err());
    let t = Instant::now();
    let _turn = gate.turn(Instant::now() + wait).await.unwrap();
    assert!(t.elapsed() >= Duration::from_secs(5), "{:?}", t.elapsed());
}

fn wayback_with(s: &StandIn, f: impl FnOnce(&mut MirrorConfig)) -> Mirrors {
    let mut c = MirrorConfig {
        wayback: Some(s.root.clone()),
        ..cfg()
    };
    f(&mut c);
    Mirrors::new(c).unwrap()
}

#[test]
fn the_cdx_lookup_gets_a_minute_and_a_wayback_503_a_short_hold() {
    let c = MirrorConfig::default();
    assert_eq!(c.cdx_timeout, Duration::from_secs(60));
    assert_eq!(c.wayback_unavailable_hold, Duration::from_secs(15));
    assert_eq!(c.backoff, Duration::from_secs(60));
}

#[tokio::test]
async fn a_wayback_cdx_timeout_is_a_miss_not_a_hold() {
    let s = StandIn::start().await;
    s.script("/cdx/search/cdx", Scripted::Slow(200, Duration::from_millis(500)));
    let m = wayback_with(&s, |c| c.cdx_timeout = Duration::from_millis(100));
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    let st = &m.status()[WAYBACK];
    assert_eq!((st.requests, st.errors, st.holds), (1, 1, 0));
    assert_eq!(st.held_secs, None, "{st:?}");
    // Not kept, and not held: the next page asks again at once.
    s.put("/cdx/search/cdx", cdx(&[]));
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    assert_eq!(s.hits("/cdx/search/cdx"), 2);
    assert_eq!(m.status()[WAYBACK].skipped_held, 0);
}

#[tokio::test]
async fn the_cdx_lookup_waits_past_a_partners_page_json_timeout() {
    let s = StandIn::start().await;
    let one = format!("{LOC}data/sn1/0001A/1890050301/0010.jp2");
    s.put("/cdx/search/cdx", cdx(&[("20250101000000", one.as_str())]));
    s.script("/cdx/search/cdx", Scripted::Slow(200, Duration::from_millis(300)));
    s.put(&format!("/web/20250101000000id_/{one}"), b"jp2 ten".to_vec());
    let m = wayback_with(&s, |c| c.cdx_timeout = Duration::from_secs(5));
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"jp2 ten"))
        .await
        .is_some());
}

#[tokio::test]
async fn a_wayback_connection_error_is_a_miss_not_a_hold() {
    // Nothing listens there.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let m = Mirrors::new(MirrorConfig {
        wayback: Some(root),
        ..cfg()
    })
    .unwrap();
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    let st = &m.status()[WAYBACK];
    assert_eq!((st.errors, st.holds, st.held_secs), (1, 0, None));
}

#[tokio::test]
async fn a_partner_connection_error_still_holds_it() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let m = partner_at(root, Duration::from_secs(5));
    assert!(m
        .jp2("xx_test", &key(2), &page("0010", b"x"))
        .await
        .is_none());
    let st = &m.status()["partner"];
    assert_eq!((st.errors, st.holds), (0, 1));
    assert!(st.held_secs.is_some_and(|h| h > 20), "{st:?}");
}

#[tokio::test]
async fn a_wayback_503_holds_it_briefly_and_a_429_for_the_backoff() {
    let s = StandIn::start().await;
    s.script("/cdx/search/cdx", Scripted::Status(503));
    let m = wayback(&s);
    assert!(m
        .jp2("xx_test", &key(1), &page("0010", b"x"))
        .await
        .is_none());
    let st = &m.status()[WAYBACK];
    assert_eq!(st.holds, 1);
    assert!(st.held_secs.is_some_and(|h| (10..=15).contains(&h)), "{st:?}");

    let s = StandIn::start().await;
    s.script("/cdx/search/cdx", Scripted::RetryAfter(503, 120));
    let m = wayback(&s);
    assert!(m.jp2("xx_test", &key(1), &page("0010", b"x")).await.is_none());
    let st = &m.status()[WAYBACK];
    assert!(st.held_secs.is_some_and(|h| (100..=120).contains(&h)), "{st:?}");

    let s = StandIn::start().await;
    s.script("/cdx/search/cdx", Scripted::Status(429));
    let m = wayback(&s);
    assert!(m.jp2("xx_test", &key(1), &page("0010", b"x")).await.is_none());
    let st = &m.status()[WAYBACK];
    // cfg()'s backoff is 30 s.
    assert!(st.held_secs.is_some_and(|h| (25..=30).contains(&h)), "{st:?}");
}

#[tokio::test]
async fn a_partner_503_keeps_the_backoff() {
    let s = StandIn::start().await;
    s.script(&format!("{SEQ2}.json"), Scripted::Status(503));
    let m = with_partner(&s, true);
    assert!(m.jp2("xx_test", &key(2), &page("0010", b"x")).await.is_none());
    let st = &m.status()["partner"];
    assert!(st.held_secs.is_some_and(|h| (25..=30).contains(&h)), "{st:?}");
}

#[tokio::test]
async fn a_wayback_challenge_or_403_still_holds_for_the_block_hold() {
    for scripted in [Scripted::Challenge(200), Scripted::Status(403)] {
        let s = StandIn::start().await;
        s.script("/cdx/search/cdx", scripted);
        let m = wayback(&s);
        assert!(m.jp2("xx_test", &key(1), &page("0010", b"x")).await.is_none());
        let st = &m.status()[WAYBACK];
        assert!(st.held_secs.is_some_and(|h| h > 500), "{st:?}");
    }
}
