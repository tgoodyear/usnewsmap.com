//! The batch route and the storage service: resolving, the md5 check, pacing,
//! holds and item errors.

use super::super::*;
use super::{batch_client, key, BATCH};
use crate::batch;
use crate::standin::{Page, Scripted, StandIn};

/// One issue of sn1 (1890-05-03, ed. 1) in batch `xx_test_ver01`,
/// whose pages' frames aren't in seq order.
fn one_issue(s: &StandIn) {
    s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &[
            Page {
                seq: 1,
                frame: "0101",
                jp2: b"jp2 page one",
            },
            Page {
                seq: 2,
                frame: "0099",
                jp2: b"jp2 page two",
            },
        ],
    );
}

#[tokio::test]
async fn resolves_through_the_batch_and_checks_the_md5() {
    let s = StandIn::start().await;
    one_issue(&s);
    let cache = tempfile::tempdir().unwrap();
    let loc = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(p2
        .url
        .ends_with(&format!("{BATCH}data/sn1/0001A/1890050301/0099.jp2")));
    assert_eq!(
        p2.iiif_base,
        format!(
            "{}service:ndnp:xx:batch_xx_test_ver01:data:sn1:0001A:1890050301:0099",
            s.iiif_root()
        )
    );
    let scan = loc
        .scan_resolved(&key(2), &p2, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 page two");
    assert_eq!(scan.format, ScanFormat::Jp2);
    assert_eq!(scan.url, p2.url);
    assert_eq!(
        scan.md5.as_deref(),
        Some(batch::md5_hex(b"jp2 page two").as_str())
    );
    // One manifest and one METS for the issue's two pages.
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert!(p1.url.ends_with("/0101.jp2"));
    assert_eq!(
        (s.hits("manifest-md5.txt"), s.hits("1890050301.xml")),
        (1, 1)
    );
    // Nothing LoC doesn't list: no seq 3, no such issue, no such batch.
    assert_eq!(loc.resolve(&key(3), "xx_test", 1).await.unwrap(), None);
    let other: PageKey = "sn1/1890-05-04/ed-1/seq-1".parse().unwrap();
    assert_eq!(loc.resolve(&other, "xx_test", 1).await.unwrap(), None);
    assert_eq!(loc.resolve(&key(1), "xx_none", 1).await.unwrap(), None);
    assert_eq!(loc.resolve(&key(1), "Bad-Batch", 1).await.unwrap(), None);
    // A JP2 whose bytes aren't the bag's: transient, never used.
    s.put(
        &format!("{BATCH}data/sn1/0001A/1890050301/0101.jp2"),
        b"changed".to_vec(),
    );
    let e = loc
        .scan_resolved(&key(1), &p1, ImageSize::Full)
        .await
        .unwrap_err();
    assert!(
        matches!(e, FetchError::Transient(ref m) if m.contains("md5")),
        "{e:?}"
    );
    // A JP2 the bag lists that answers 404: None, for the item API.
    s.put(
        &format!("{BATCH}data/sn1/0001A/1890050301/0101.jp2"),
        Vec::new(),
    );
    s.script(
        &format!("{BATCH}data/sn1/0001A/1890050301/0101.jp2"),
        Scripted::Status(404),
    );
    assert!(loc
        .scan_resolved(&key(1), &p1, ImageSize::Full)
        .await
        .unwrap()
        .is_none());
    // A scaled scan is the IIIF JPEG of the same base.
    s.put(
        "/iiif/service:ndnp:xx:batch_xx_test_ver01:data:sn1:0001A:1890050301:0099/full/pct:50/0/default.jpg",
        b"half".to_vec(),
    );
    let half = loc
        .scan_resolved(&key(2), &p2, ImageSize::Pct(50))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (&half.bytes[..], half.format),
        (&b"half"[..], ScanFormat::Jpeg)
    );

    // A new process with the same cache asks LoC for neither again.
    let loc = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    assert_eq!(
        loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap(),
        p2
    );
    assert_eq!(
        (
            s.hits(&format!("{BATCH}manifest-md5.txt")),
            s.hits("1890050301.xml")
        ),
        (1, 1)
    );
    assert!(cache.path().join("xx_test_ver01/manifest-md5.txt").exists());
    assert!(cache
        .path()
        .join("xx_test_ver01/mets/sn1/1890050301.xml")
        .exists());
    // A METS on disk that isn't the bag's is fetched again.
    std::fs::write(
        cache.path().join("xx_test_ver01/mets/sn1/1890050301.xml"),
        b"x",
    )
    .unwrap();
    let loc = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert_eq!(s.hits("1890050301.xml"), 2);
}

#[tokio::test]
async fn follows_chronicling_americas_redirect_onto_the_storage_service_only() {
    let s = StandIn::start().await;
    // The batch isn't under its name's first word.
    let elsewhere = "/storage-services/service/ndnp/yy/batch_xx_test_ver01/";
    s.add_issue(
        elsewhere,
        "sn1",
        "0001A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: b"jp2",
        }],
    );
    s.redirect(
        "/data/batches/xx_test_ver01/manifest-md5.txt",
        &format!("{elsewhere}manifest-md5.txt"),
    );
    let loc = batch_client(&s, Duration::from_millis(10), None, false);
    let p = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert!(p.url.contains("/ndnp/yy/batch_xx_test_ver01/data/"));
    // A redirect off the storage service isn't followed at all.
    s.put("/other/manifest-md5.txt", "x".to_owned());
    s.redirect(
        "/data/batches/xx_off_ver01/manifest-md5.txt",
        "/other/manifest-md5.txt",
    );
    assert_eq!(loc.resolve(&key(1), "xx_off", 1).await.unwrap(), None);
    assert_eq!(s.hits("/other/manifest-md5.txt"), 0);
}

#[tokio::test]
async fn the_storage_pacer_spaces_requests_and_honours_holds() {
    let s = StandIn::start().await;
    one_issue(&s);
    let jp2 = format!("{BATCH}data/sn1/0001A/1890050301/0099.jp2");
    s.script(&jp2, Scripted::RetryAfter(503, 1));
    let loc = batch_client(&s, Duration::from_millis(200), None, false);
    let t0 = std::time::Instant::now();
    let p = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    let scan = loc
        .scan_resolved(&key(2), &p, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 page two");
    assert!(
        t0.elapsed() >= Duration::from_secs(1),
        "waited out Retry-After"
    );
    let mut t: Vec<std::time::Instant> = ["manifest-md5.txt", ".xml", ".jp2"]
        .iter()
        .flat_map(|x| s.times(x))
        .collect();
    t.sort();
    // manifest, METS, JP2 (503), JP2: each at least the gap apart, the
    // last after the hold.
    assert_eq!(t.len(), 4);
    for w in t.windows(2) {
        assert!(
            w[1] - w[0] >= Duration::from_millis(150),
            "{:?}",
            w[1] - w[0]
        );
    }
    assert!(t[3] - t[2] >= Duration::from_secs(1));
}

#[tokio::test]
async fn a_storage_hold_holds_the_image_server_unless_it_has_its_own_budget() {
    let s = StandIn::start().await;
    one_issue(&s);
    let manifest = format!("{BATCH}manifest-md5.txt");
    for own in [false, true] {
        s.script(&manifest, Scripted::RetryAfter(429, 3600));
        let loc = batch_client(&s, Duration::from_millis(10), None, own);
        let r = tokio::time::timeout(
            Duration::from_millis(500),
            loc.resolve(&key(1), "xx_test", 1),
        )
        .await;
        assert!(r.is_err(), "still held");
        assert!(loc.storage.held_until().is_some());
        assert_eq!(loc.tile.held_until().is_some(), !own, "own budget: {own}");
        assert!(loc.held_until().is_some());
    }
}

#[tokio::test]
async fn a_403_on_the_storage_service_holds_and_isnt_sent_round_the_api() {
    let s = StandIn::start().await;
    one_issue(&s);
    let jp2 = format!("{BATCH}data/sn1/0001A/1890050301/0101.jp2");
    for _ in 0..3 {
        s.script(&jp2, Scripted::Status(403));
    }
    let loc = batch_client(&s, Duration::from_millis(10), None, false);
    let p = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    let e = loc
        .scan_resolved(&key(1), &p, ImageSize::Full)
        .await
        .unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(loc.held_until().is_some());
    assert_eq!(s.hits("/item/sn1/1890-05-03/ed-1/"), 0);
}

#[tokio::test]
async fn a_fast_bare_503_for_one_issue_on_the_storage_service_is_an_item_error() {
    let s = StandIn::start().await;
    one_issue(&s);
    s.add_issue(
        BATCH,
        "sn2",
        "0002A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: b"x",
        }],
    );
    let loc = batch_client(&s, Duration::from_millis(10), None, false);
    // sn1's METS answers; sn2's answers 503 at once, twice: the
    // first holds, the second is an item error.
    loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    for _ in 0..2 {
        s.script(
            &format!("{BATCH}data/sn2/0002A/1890050301/1890050301.xml"),
            Scripted::Status(503),
        );
    }
    let sn2: PageKey = "sn2/1890-05-03/ed-1/seq-1".parse().unwrap();
    let e = loc.resolve(&sn2, "xx_test", 1).await.unwrap_err();
    assert!(matches!(e, FetchError::Item(_)), "{e:?}");
    assert_eq!(
        s.hits(&format!("{BATCH}data/sn2/0002A/1890050301/1890050301.xml")),
        2
    );
    // Only the first one's 10 ms hold.
    assert_eq!(loc.held_until(), None);
    // A single 503 for a file that answers next time is only a hold.
    s.script(
        &format!("{BATCH}data/sn1/0001A/1890050301/0101.jp2"),
        Scripted::Status(503),
    );
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    loc.scan_resolved(&key(1), &p1, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    // A slow 503 is a hold.
    s.script(
        &format!("{BATCH}data/sn2/0002A/1890050301/1890050301.xml"),
        Scripted::Slow(503, Duration::from_millis(2500)),
    );
    let mut cfg = batch_client(&s, Duration::from_millis(10), None, false).cfg;
    cfg.attempts = 1;
    cfg.backoff = Duration::from_secs(5);
    let slow = LocClient::new(cfg).unwrap();
    slow.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    let e = slow.resolve(&sn2, "xx_test", 1).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
    assert!(slow.held_until().is_some());
    // A manifest's 503 always holds, however fast.
    s.script(&format!("{BATCH}manifest-md5.txt"), Scripted::Status(503));
    let mut cfg = batch_client(&s, Duration::from_millis(10), None, false).cfg;
    cfg.backoff = Duration::from_secs(5);
    let loc = LocClient::new(cfg).unwrap();
    let r = tokio::time::timeout(
        Duration::from_millis(300),
        loc.resolve(&key(1), "xx_test", 1),
    )
    .await;
    assert!(r.is_err(), "held");
    assert!(loc.held_until().is_some());
}

#[tokio::test]
async fn a_jp2_never_becomes_the_known_good_item() {
    let s = StandIn::start().await;
    let big = vec![7u8; PROBE_MAX_BYTES + 1];
    s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: &big,
        }],
    );
    let cache = tempfile::tempdir().unwrap();
    let loc = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    // A new process: the METS comes from the disk cache, so the only
    // storage answer it gets is the JP2.
    let loc = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    let p = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    loc.scan_resolved(&key(1), &p, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    let h = loc.storage_health.lock().unwrap();
    assert!(h.last_ok.is_some());
    assert_eq!(h.known_good, None);
}

#[tokio::test]
async fn the_storage_known_good_seed_is_a_mets_on_disk() {
    let s = StandIn::start().await;
    let cache = tempfile::tempdir().unwrap();
    let loc = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    let jp2 = format!(
        "{}xx/batch_xx_test_ver01/data/sn1/0001A/1890050301/0001.jp2",
        s.storage_root()
    );
    // Nothing on disk: no seed.
    loc.seed_storage_known_good(&jp2);
    assert_eq!(loc.storage_health.lock().unwrap().known_good, None);
    // Only the signed METS is kept: that one.
    let mets = cache.path().join("xx_test_ver01/mets/sn1");
    std::fs::create_dir_all(&mets).unwrap();
    std::fs::write(mets.join("1890050301_1.xml"), b"x").unwrap();
    loc.seed_storage_known_good(&jp2);
    assert_eq!(
        loc.storage_health.lock().unwrap().known_good.as_deref(),
        Some(jp2.replace("0001.jp2", "1890050301_1.xml").as_str())
    );
}

#[tokio::test]
async fn only_two_bare_503s_in_a_row_on_storage_make_an_item_error() {
    let s = StandIn::start().await;
    one_issue(&s);
    s.add_issue(
        BATCH,
        "sn2",
        "0002A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: b"x",
        }],
    );
    let mets2 = format!("{BATCH}data/sn2/0002A/1890050301/1890050301.xml");
    let mut cfg = batch_client(&s, Duration::from_millis(10), None, false).cfg;
    cfg.attempts = 2;
    let loc = LocClient::new(cfg).unwrap();
    loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    // A 503 with Retry-After, then a bare one: the bare one is the
    // first of its kind, so it holds; the request gives up transient.
    s.script(&mets2, Scripted::RetryAfter(503, 1));
    s.script(&mets2, Scripted::Status(503));
    let sn2: PageKey = "sn2/1890-05-03/ed-1/seq-1".parse().unwrap();
    let e = loc.resolve(&sn2, "xx_test", 1).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
}

#[tokio::test]
async fn a_mets_that_doesnt_parse_fails_rather_than_going_round() {
    let s = StandIn::start().await;
    let mets = s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: b"x",
        }],
    );
    // A broken METS with the bag's md5 for it.
    let broken = b"<mets><dmdSec".to_vec();
    s.put(&mets, broken.clone());
    s.put(
        &format!("{BATCH}manifest-md5.txt"),
        format!(
            "{}  data/sn1/0001A/1890050301/1890050301.xml\n{}  data/sn1/0001A/1890050301/0001.jp2\n",
            batch::md5_hex(&broken),
            batch::md5_hex(b"x")
        ),
    );
    let loc = batch_client(&s, Duration::from_millis(10), None, false);
    let e = loc.resolve(&key(1), "xx_test", 1).await.unwrap_err();
    assert!(
        matches!(e, FetchError::Transient(ref m) if m.contains("METS")),
        "{e:?}"
    );
}

#[tokio::test]
async fn another_failure_between_bare_503s_clears_the_first() {
    let s = StandIn::start().await;
    one_issue(&s);
    s.add_issue(
        BATCH,
        "sn2",
        "0002A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: b"x",
        }],
    );
    let mets2 = format!("{BATCH}data/sn2/0002A/1890050301/1890050301.xml");
    let mut cfg = batch_client(&s, Duration::from_millis(10), None, false).cfg;
    cfg.attempts = 3;
    let loc = LocClient::new(cfg).unwrap();
    loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    for a in [
        Scripted::Status(503),
        Scripted::Status(500),
        Scripted::Status(503),
    ] {
        s.script(&mets2, a);
    }
    let sn2: PageKey = "sn2/1890-05-03/ed-1/seq-1".parse().unwrap();
    let e = loc.resolve(&sn2, "xx_test", 1).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient(_)), "{e:?}");
}
