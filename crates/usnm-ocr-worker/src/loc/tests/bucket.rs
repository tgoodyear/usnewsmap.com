//! LoC's bucket: the manifest, the METS and the JP2 from it, its one gate
//! and its holds, and what goes to LoC instead.

use super::super::*;
use super::{batch_client, key, BATCH};
use crate::loc::bucket::{BucketConfig, NAME};
use crate::standin::{Page, Scripted, StandIn};

/// The batch in the bucket: LoC's path without the awardee and `batch_`.
const IN_BUCKET: &str = "/xx_test_ver01/";
const MANIFEST: &str = "/xx_test_ver01/manifest-md5.txt";
const META_MD5: &str = "x-amz-meta-md5";

/// One issue of sn1 (1890-05-03, ed. 1) in a batch at `at`.
fn issue(s: &StandIn, at: &str) {
    s.add_issue(
        at,
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

/// The issue on LoC and in the bucket, whose manifest carries its md5 as
/// S3 does.
fn both(loc: &StandIn, bucket: &StandIn) {
    issue(loc, BATCH);
    issue(bucket, IN_BUCKET);
    let md5 = batch::md5_hex(&bucket.file(MANIFEST).unwrap());
    bucket.header(MANIFEST, META_MD5, &md5);
}

fn bucket_cfg(root: &str) -> BucketConfig {
    BucketConfig {
        root: format!("{root}/"),
        gap: Duration::from_millis(10),
        backoff: Duration::from_secs(30),
        block_hold: Duration::from_secs(600),
        max_wait: Duration::from_secs(5),
    }
}

/// A client of LoC stand-in `s` with the bucket at `root`.
fn client_at(s: &StandIn, root: &str, cache: Option<PathBuf>) -> LocClient {
    let mut cfg = batch_client(s, Duration::from_millis(10), cache, false).cfg;
    cfg.bucket = Some(bucket_cfg(root));
    LocClient::new(cfg).unwrap()
}

fn client(s: &StandIn, bucket: &StandIn, cache: Option<PathBuf>) -> LocClient {
    client_at(s, &bucket.root, cache)
}

fn jp2_in_bucket(b: &StandIn, frame: &str) -> String {
    format!("{}{IN_BUCKET}data/sn1/0001A/1890050301/{frame}.jp2", b.root)
}

#[tokio::test]
async fn a_public_batch_comes_entirely_from_the_bucket_and_is_kept_as_locs() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    let cache = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = client(&s, &b, Some(cache.path().into())).cfg;
    cfg.global_pacer = Some(dir.path().join("pacer.json"));
    let loc = LocClient::new(cfg).unwrap();
    // LoC's pacers and the host's budget are held for an hour: nothing
    // the bucket does waits for them.
    loc.storage.hold(Duration::from_secs(3600));
    loc.global.as_ref().unwrap().hold(Duration::from_secs(3600));
    let (p, (scan, hit)) = tokio::time::timeout(Duration::from_secs(5), async {
        let p = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        let got = loc.bucket_jp2(&p).await.unwrap();
        (p, got)
    })
    .await
    .expect("never behind LoC's pacers");
    // Placed and recorded as LoC's.
    assert_eq!(
        p.url,
        format!(
            "{}xx/batch_xx_test_ver01/data/sn1/0001A/1890050301/0099.jp2",
            s.storage_root()
        )
    );
    assert_eq!(&scan.bytes[..], b"jp2 page two");
    assert_eq!(
        (scan.url.as_str(), scan.iiif_base.as_str(), scan.format),
        (p.url.as_str(), p.iiif_base.as_str(), ScanFormat::Jp2)
    );
    assert_eq!(
        scan.md5.as_deref(),
        Some(batch::md5_hex(b"jp2 page two").as_str())
    );
    assert_eq!(
        hit,
        mirror::Hit {
            source: NAME,
            url: jp2_in_bucket(&b, "0099")
        }
    );
    assert_eq!(s.hits(""), 0, "nothing from LoC");
    let st = loc.bucket_status().unwrap();
    assert_eq!(
        (
            st.requests,
            st.manifests,
            st.mets,
            st.jp2s,
            st.public_batches
        ),
        (3, 1, 1, 1, 1)
    );
    assert_eq!(st.held_secs, None);
    // On disk exactly as a manifest from LoC: the same file, under LoC's
    // storage URL, and the METS.
    let kept = cache.path().join("xx_test_ver01");
    assert_eq!(
        std::fs::read(kept.join("manifest-md5.txt")).unwrap(),
        b.file(MANIFEST).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(kept.join("url")).unwrap(),
        format!("{}xx/batch_xx_test_ver01/", s.storage_root())
    );
    assert!(kept.join("mets/sn1/1890050301.xml").exists());
    // A new process reads both from disk and asks the bucket for the JP2.
    let loc = client(&s, &b, Some(cache.path().into()));
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    let (scan, _) = loc.bucket_jp2(&p1).await.unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 page one");
    assert_eq!((b.hits(""), s.hits("")), (4, 0));
}

#[tokio::test]
async fn a_private_batch_goes_to_loc_and_isnt_asked_again() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    issue(&s, BATCH);
    // S3's AccessDenied for every object of a private batch.
    b.script(MANIFEST, Scripted::Status(403));
    let loc = client(&s, &b, None);
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p2).await.is_none());
    let scan = loc
        .scan_resolved(&key(2), &p2, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 page two");
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p1).await.is_none());
    assert_eq!(b.hits(""), 1, "remembered: the METS and JP2s weren't asked");
    assert_eq!(
        (
            s.hits("manifest-md5.txt"),
            s.hits("1890050301.xml"),
            s.hits(".jp2")
        ),
        (1, 1, 1)
    );
    // A 403 holds nothing.
    let st = loc.bucket_status().unwrap();
    assert_eq!(
        (
            st.private_batches,
            st.public_batches,
            st.holds,
            st.held_secs
        ),
        (1, 0, 0, None)
    );
    assert_eq!(loc.held_until(), None);
}

#[tokio::test]
async fn a_403_for_a_file_of_a_batch_on_disk_marks_it_private() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    let cache = tempfile::tempdir().unwrap();
    // An earlier run, without the bucket, kept LoC's manifest and METS.
    let first = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    first.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    b.script(
        &format!("{IN_BUCKET}data/sn1/0001A/1890050301/0099.jp2"),
        Scripted::Status(403),
    );
    let loc = client(&s, &b, Some(cache.path().into()));
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p2).await.is_none());
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p1).await.is_none());
    assert_eq!(b.hits(""), 1);
    assert_eq!(loc.bucket_status().unwrap().private_batches, 1);
}

#[tokio::test]
async fn a_bucket_jp2_that_isnt_the_bags_is_refused_and_loc_serves_it() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    b.put(
        &format!("{IN_BUCKET}data/sn1/0001A/1890050301/0099.jp2"),
        b"cut short".to_vec(),
    );
    let loc = client(&s, &b, None);
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p2).await.is_none());
    let scan = loc
        .scan_resolved(&key(2), &p2, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 page two");
    // The batch is still public: its other page comes from the bucket.
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p1).await.is_some());
    let st = loc.bucket_status().unwrap();
    assert_eq!((st.md5_mismatches, st.jp2s, st.public_batches), (1, 1, 1));
    assert_eq!(s.hits(""), 1, "only the refused page's JP2");
}

#[tokio::test]
async fn a_manifest_is_taken_only_if_it_matches_its_x_amz_meta_md5() {
    let wrong = batch::md5_hex(b"another manifest");
    for (meta, taken) in [
        (None, true),
        (Some("base64"), true),
        (Some(wrong.as_str()), false),
    ] {
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        issue(&s, BATCH);
        issue(&b, IN_BUCKET);
        let md5 = batch::md5(&b.file(MANIFEST).unwrap());
        match meta {
            Some("base64") => {
                use base64::Engine as _;
                let v = base64::engine::general_purpose::STANDARD.encode(md5);
                b.header(MANIFEST, META_MD5, &v);
            }
            Some(v) => b.header(MANIFEST, META_MD5, v),
            None => {}
        }
        let loc = client(&s, &b, None);
        loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        let st = loc.bucket_status().unwrap();
        assert_eq!(st.manifests, u64::from(taken), "{meta:?}");
        assert_eq!(st.md5_mismatches, u64::from(!taken), "{meta:?}");
        assert_eq!(s.hits("manifest-md5.txt"), usize::from(!taken), "{meta:?}");
        // The METS is the bag's either way, so the bucket serves it.
        assert_eq!((st.mets, s.hits(".xml")), (1, 0), "{meta:?}");
    }
}

#[tokio::test]
async fn a_slowdown_holds_the_bucket_only_and_the_page_goes_to_loc() {
    for (answer, held) in [
        (Scripted::Status(503), 25..=30),
        (Scripted::RetryAfter(429, 120), 100..=120),
    ] {
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        both(&s, &b);
        b.script(MANIFEST, answer);
        let loc = client(&s, &b, None);
        let t = std::time::Instant::now();
        let p = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        // Held: skipped at once, never waited out.
        assert!(loc.bucket_jp2(&p).await.is_none());
        let scan = loc
            .scan_resolved(&key(2), &p, ImageSize::Full)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&scan.bytes[..], b"jp2 page two");
        assert!(t.elapsed() < Duration::from_secs(5));
        let st = loc.bucket_status().unwrap();
        assert_eq!((st.requests, st.holds, st.skipped_held), (1, 1, 2));
        assert!(st.held_secs.is_some_and(|h| held.contains(&h)), "{st:?}");
        assert_eq!(loc.held_until(), None, "LoC isn't held");
        assert_eq!(
            (s.hits("manifest-md5.txt"), s.hits(".xml"), s.hits(".jp2")),
            (1, 1, 1)
        );
    }
}

#[tokio::test]
async fn the_bucket_gets_one_request_at_a_time_with_four_pages_in_flight() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    let pages: Vec<(u16, String, Vec<u8>)> = (1..=4)
        .map(|q| (q, format!("{:04}", 10 + q), format!("jp2 {q}").into_bytes()))
        .collect();
    let pages: Vec<Page<'_>> = pages
        .iter()
        .map(|(q, f, j)| Page {
            seq: *q,
            frame: f,
            jp2: j,
        })
        .collect();
    for (st, at) in [(&s, BATCH), (&b, IN_BUCKET)] {
        st.add_issue(at, "sn1", "0001A", "1890050301", &pages);
    }
    b.delay(Duration::from_millis(100));
    let loc = Arc::new(client(&s, &b, None));
    let calls: Vec<_> = (1..=4)
        .map(|q| {
            let loc = Arc::clone(&loc);
            tokio::spawn(async move {
                let p = loc.resolve(&key(q), "xx_test", 1).await.unwrap().unwrap();
                loc.bucket_jp2(&p).await.is_some()
            })
        })
        .collect();
    for c in calls {
        assert!(c.await.unwrap(), "each waited its turn");
    }
    assert_eq!(b.most_at_once(), 1, "never two at once");
    // One manifest, one METS, four JP2s, each after the last one ended.
    let mut t = b.times("");
    t.sort();
    assert_eq!(t.len(), 6);
    for w in t.windows(2) {
        assert!(
            w[1] - w[0] >= Duration::from_millis(100),
            "{:?}",
            w[1] - w[0]
        );
    }
    assert_eq!(s.hits(""), 0);
}

#[tokio::test]
async fn a_redirect_off_the_bucket_isnt_followed() {
    let s = StandIn::start().await;
    issue(&s, BATCH);
    let elsewhere = StandIn::start().await;
    issue(&elsewhere, IN_BUCKET);
    // A bucket that sends every request to another host and port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let to = elsewhere.root.clone();
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let to = format!("{to}{}", uri.path());
        async move {
            (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [(axum::http::header::LOCATION, to)],
            )
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let loc = client_at(&s, &root, None);
    let p = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p).await.is_none());
    assert_eq!(elsewhere.hits(""), 0, "never followed");
    let st = loc.bucket_status().unwrap();
    assert_eq!((st.requests, st.misses, st.holds), (3, 3, 0));
    assert_eq!((s.hits("manifest-md5.txt"), s.hits(".xml")), (1, 1));
}

#[tokio::test]
async fn off_by_default_nothing_changes() {
    assert!(LocConfig::default().bucket.is_none());
    let s = StandIn::start().await;
    issue(&s, BATCH);
    let loc = batch_client(&s, Duration::from_millis(10), None, false);
    assert!(!loc.has_bucket());
    assert_eq!(loc.bucket_status(), None);
    let p = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p).await.is_none());
    loc.scan_resolved(&key(2), &p, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (s.hits("manifest-md5.txt"), s.hits(".xml"), s.hits(".jp2")),
        (1, 1, 1)
    );
}
