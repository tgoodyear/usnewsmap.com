//! LoC's bucket: the manifest, the METS and the JP2 from it, its one gate
//! and its holds, and what goes to LoC instead.

use super::super::*;
use super::{batch_client, key, BATCH};
use crate::loc::bucket::{BucketConfig, NAME};
use crate::standin::{Page, Scripted, StandIn};

/// The batch in the bucket: LoC's path without the awardee and `batch_`.
const IN_BUCKET: &str = "/xx_test_ver01/";
const MANIFEST: &str = "/xx_test_ver01/manifest-md5.txt";
const METS: &str = "/xx_test_ver01/data/sn1/0001A/1890050301/1890050301.xml";
const META_MD5: &str = "x-amz-meta-md5";
/// The batch on LoC under another awardee's directory than its name's
/// first part, and Chronicling America's redirect that says so.
const ELSEWHERE: &str = "/storage-services/service/ndnp/yy/batch_xx_test_ver01/";
const REDIRECT: &str = "/data/batches/xx_test_ver01/manifest-md5.txt";

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

/// The issue in the bucket, whose manifest carries its md5 as S3 does.
fn in_bucket(bucket: &StandIn) {
    issue(bucket, IN_BUCKET);
    let md5 = batch::md5_hex(&bucket.file(MANIFEST).unwrap());
    bucket.header(MANIFEST, META_MD5, &md5);
}

/// The issue on LoC and in the bucket.
fn both(loc: &StandIn, bucket: &StandIn) {
    issue(loc, BATCH);
    in_bucket(bucket);
}

fn bucket_cfg(root: &str) -> BucketConfig {
    BucketConfig {
        root: format!("{root}/"),
        gap: Duration::from_millis(10),
        backoff: Duration::from_secs(30),
        block_hold: Duration::from_secs(600),
        max_wait: Duration::from_secs(5),
        timeout: Duration::from_secs(120),
    }
}

/// A client of LoC stand-in `s` with the bucket `bucket`.
fn client_with(s: &StandIn, bucket: BucketConfig, cache: Option<PathBuf>) -> LocClient {
    let mut cfg = batch_client(s, Duration::from_millis(10), cache, false).cfg;
    cfg.bucket = Some(bucket);
    LocClient::new(cfg).unwrap()
}

/// A client of LoC stand-in `s` with the bucket at `root`.
fn client_at(s: &StandIn, root: &str, cache: Option<PathBuf>) -> LocClient {
    client_with(s, bucket_cfg(root), cache)
}

fn client(s: &StandIn, bucket: &StandIn, cache: Option<PathBuf>) -> LocClient {
    client_at(s, &bucket.root, cache)
}

fn jp2_in_bucket(b: &StandIn, frame: &str) -> String {
    format!("{}{IN_BUCKET}data/sn1/0001A/1890050301/{frame}.jp2", b.root)
}

/// A file of the issue in the batch at `at` (LoC's [`BATCH`] or
/// [`ELSEWHERE`], or [`IN_BUCKET`]).
fn file(at: &str, name: &str) -> String {
    format!("{at}data/sn1/0001A/1890050301/{name}")
}

/// The batch's URL on LoC's storage service under awardee `aw`.
fn batch_url(s: &StandIn, aw: &str) -> String {
    format!("{}{aw}/batch_xx_test_ver01/", s.storage_root())
}

fn iiif_of(s: &StandIn, aw: &str, frame: &str) -> String {
    format!(
        "{}service:ndnp:{aw}:batch_xx_test_ver01:data:sn1:0001A:1890050301:{frame}",
        s.iiif_root()
    )
}

/// Where the batch's files are kept on disk.
fn kept(cache: &tempfile::TempDir) -> PathBuf {
    cache.path().join("xx_test_ver01")
}

/// The checked URL and the guess kept on disk.
/// A JP2 URL under the guessed (wrong) batch directory `xx`.
fn p1_guessed_url(s: &StandIn) -> String {
    format!("{}data/sn1/0001A/1890050301/0101.jp2", batch_url(s, "xx"))
}

fn urls_kept(cache: &tempfile::TempDir) -> (Option<String>, Option<String>) {
    let read = |n: &str| std::fs::read_to_string(kept(cache).join(n)).ok();
    (read("url"), read("url.guessed"))
}

/// The bucket's hold, in whole seconds, if it is held.
fn held(loc: &LocClient) -> Option<u64> {
    loc.bucket_status().unwrap().held_secs
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
    // On disk: the same file as LoC's, and the METS; the URL its name
    // gives on LoC is kept as a guess, not as a URL LoC served.
    let kept = kept(&cache);
    assert_eq!(
        std::fs::read(kept.join("manifest-md5.txt")).unwrap(),
        b.file(MANIFEST).unwrap()
    );
    assert_eq!(urls_kept(&cache), (None, Some(batch_url(&s, "xx"))));
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
async fn a_manifest_is_taken_only_if_it_matches_its_x_amz_meta_md5_or_etag() {
    use base64::Engine as _;
    /// What the bucket sends with the manifest, and what becomes of it.
    enum Taken {
        Yes,
        Mismatch,
        NoMd5,
    }
    let (wrong, wrong_hex) = (
        batch::md5(b"another manifest"),
        batch::md5_hex(b"another manifest"),
    );
    for (case, taken) in [
        ("hex", Taken::Yes),
        ("base64", Taken::Yes),
        ("etag", Taken::Yes),
        ("hex and a wrong etag", Taken::Yes),
        ("wrong hex", Taken::Mismatch),
        ("unreadable", Taken::Mismatch),
        ("wrong etag", Taken::Mismatch),
        ("nothing", Taken::NoMd5),
        ("multipart etag", Taken::NoMd5),
    ] {
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        issue(&s, BATCH);
        issue(&b, IN_BUCKET);
        let md5 = batch::md5(&b.file(MANIFEST).unwrap());
        let (meta, etag) = match case {
            "hex" => (Some(batch::hex(&md5)), None),
            "base64" => (
                Some(base64::engine::general_purpose::STANDARD.encode(md5)),
                None,
            ),
            "etag" => (None, Some(format!("\"{}\"", batch::hex(&md5)))),
            "hex and a wrong etag" => (Some(batch::hex(&md5)), Some(format!("\"{wrong_hex}\""))),
            "wrong hex" => (Some(wrong_hex.clone()), None),
            "unreadable" => (Some("nonsense".to_owned()), None),
            "wrong etag" => (None, Some(format!("\"{}\"", batch::hex(&wrong)))),
            "nothing" => (None, None),
            "multipart etag" => (None, Some(format!("\"{}-2\"", batch::hex(&md5)))),
            _ => unreachable!(),
        };
        if let Some(v) = meta {
            b.header(MANIFEST, META_MD5, &v);
        }
        if let Some(v) = etag {
            b.header(MANIFEST, "etag", &v);
        }
        let loc = client(&s, &b, None);
        loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        let st = loc.bucket_status().unwrap();
        let (yes, mismatch, no_md5) = match taken {
            Taken::Yes => (1, 0, 0),
            Taken::Mismatch => (0, 1, 0),
            Taken::NoMd5 => (0, 0, 1),
        };
        assert_eq!(
            (st.manifests, st.md5_mismatches, st.no_md5),
            (yes, mismatch, no_md5),
            "{case}"
        );
        assert_eq!(s.hits("manifest-md5.txt"), 1 - yes as usize, "{case}");
        // The METS is the bag's either way, so the bucket serves it.
        assert_eq!((st.mets, s.hits(".xml")), (1, 0), "{case}");
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
    let md5 = batch::md5_hex(&b.file(MANIFEST).unwrap());
    b.header(MANIFEST, META_MD5, &md5);
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
async fn a_redirect_off_the_bucket_isnt_followed_and_holds_it() {
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
    // A wrong root: held for the block hold, so nothing more waits a turn
    // for it.
    let st = loc.bucket_status().unwrap();
    assert_eq!(
        (st.requests, st.misses, st.holds, st.skipped_held),
        (1, 0, 1, 2)
    );
    assert!(
        st.held_secs.is_some_and(|h| (595..=600).contains(&h)),
        "{st:?}"
    );
    assert_eq!(loc.held_until(), None, "LoC isn't held");
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

#[tokio::test]
async fn a_403_for_one_file_of_a_public_batch_is_that_file_only() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    // One object with an ACL of its own, or a missing key where listing
    // isn't allowed: S3 answers 403.
    b.script(&file(IN_BUCKET, "0099.jp2"), Scripted::Status(403));
    let loc = client(&s, &b, None);
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p2).await.is_none());
    // The batch's other JP2 is still asked, and served.
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    let (scan, _) = loc.bucket_jp2(&p1).await.unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 page one");
    let st = loc.bucket_status().unwrap();
    assert_eq!(
        (
            st.public_batches,
            st.private_batches,
            st.misses,
            st.jp2s,
            st.holds
        ),
        (1, 0, 1, 1, 0)
    );
    assert_eq!(s.hits(""), 0);
}

#[tokio::test]
async fn an_html_page_or_a_challenge_holds_the_bucket_for_the_block_hold() {
    for case in ["html", "challenge 200", "challenge 403"] {
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        both(&s, &b);
        match case {
            "html" => b.header(MANIFEST, "content-type", "text/html; charset=utf-8"),
            "challenge 200" => b.script(MANIFEST, Scripted::Challenge(200)),
            _ => b.script(MANIFEST, Scripted::Challenge(403)),
        }
        let loc = client(&s, &b, None);
        let p = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        assert!(loc.bucket_jp2(&p).await.is_none(), "{case}");
        let st = loc.bucket_status().unwrap();
        assert_eq!(
            (st.requests, st.holds, st.skipped_held, st.private_batches),
            (1, 1, 2, 0),
            "{case}"
        );
        assert!(
            held(&loc).is_some_and(|h| (595..=600).contains(&h)),
            "{case}: {st:?}"
        );
        assert_eq!(loc.held_until(), None, "{case}: LoC isn't held");
        assert_eq!(
            (s.hits("manifest-md5.txt"), s.hits(".xml")),
            (1, 1),
            "{case}"
        );
    }
}

#[tokio::test]
async fn an_error_holds_the_bucket_for_the_backoff() {
    for case in ["500", "502", "timeout", "network", "over-size METS"] {
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        both(&s, &b);
        let mut cfg = bucket_cfg(&b.root);
        match case {
            "500" => b.script(MANIFEST, Scripted::Status(500)),
            "502" => b.script(MANIFEST, Scripted::Status(502)),
            "timeout" => {
                cfg.timeout = Duration::from_millis(200);
                b.delay(Duration::from_secs(2));
            }
            "network" => {
                // A port nothing listens on any more: refused.
                let addr = std::net::TcpListener::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap();
                cfg.root = format!("http://{addr}/");
            }
            _ => b.put(METS, vec![b'x'; MAX_METS_BYTES + 1]),
        }
        let loc = client_with(&s, cfg, None);
        let t = std::time::Instant::now();
        let p = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        assert!(loc.bucket_jp2(&p).await.is_none(), "{case}");
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "{case}: never waited out"
        );
        let st = loc.bucket_status().unwrap();
        // The over-size METS is the second request, after the manifest.
        let (requests, skipped, manifests) = if case == "over-size METS" {
            (2, 1, 1)
        } else {
            (1, 2, 0)
        };
        assert_eq!(
            (st.requests, st.holds, st.skipped_held, st.manifests),
            (requests, 1, skipped, manifests),
            "{case}"
        );
        assert!(
            held(&loc).is_some_and(|h| (25..=30).contains(&h)),
            "{case}: {st:?}"
        );
        assert_eq!(loc.held_until(), None, "{case}: LoC isn't held");
        assert_eq!(s.hits(".xml"), 1, "{case}");
    }
}

#[tokio::test]
async fn a_404_for_a_file_of_a_public_batch_is_a_miss_only() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    b.script(METS, Scripted::Status(404));
    b.script(&file(IN_BUCKET, "0099.jp2"), Scripted::Status(404));
    let loc = client(&s, &b, None);
    // The METS from LoC, the bag from the bucket.
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p2).await.is_none());
    // The batch is still public: its next file is asked, and served.
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert!(loc.bucket_jp2(&p1).await.is_some());
    let st = loc.bucket_status().unwrap();
    assert_eq!(
        (
            st.requests,
            st.manifests,
            st.mets,
            st.jp2s,
            st.misses,
            st.public_batches,
            st.private_batches,
            st.holds
        ),
        (4, 1, 0, 1, 2, 1, 0, 0)
    );
    assert_eq!(
        (s.hits("manifest-md5.txt"), s.hits(".xml"), s.hits(".jp2")),
        (0, 1, 0)
    );
}

#[tokio::test]
async fn a_bucket_busy_past_the_wait_sends_the_request_to_loc_and_holds_nothing() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    let loc = Arc::new(client_with(
        &s,
        BucketConfig {
            max_wait: Duration::from_millis(300),
            ..bucket_cfg(&b.root)
        },
        None,
    ));
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    // One JP2 takes the bucket for a second; the other can't wait that
    // long.
    b.delay(Duration::from_secs(1));
    let first = {
        let (loc, p1) = (Arc::clone(&loc), p1.clone());
        tokio::spawn(async move { loc.bucket_jp2(&p1).await.is_some() })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t = std::time::Instant::now();
    assert!(loc.bucket_jp2(&p2).await.is_none());
    assert!(
        t.elapsed() < Duration::from_millis(800),
        "{:?}",
        t.elapsed()
    );
    assert!(first.await.unwrap());
    let st = loc.bucket_status().unwrap();
    assert_eq!(
        (
            st.requests,
            st.jp2s,
            st.skipped_held,
            st.holds,
            st.held_secs
        ),
        (3, 1, 1, 0, None)
    );
    // LoC serves it.
    let scan = loc
        .scan_resolved(&key(2), &p2, ImageSize::Full)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&scan.bytes[..], b"jp2 page two");
    assert_eq!(s.hits(""), 1);
}

#[tokio::test]
async fn a_batch_found_private_while_a_request_waited_its_turn_isnt_asked() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    let cache = tempfile::tempdir().unwrap();
    // An earlier run, without the bucket, kept LoC's manifest and METS:
    // nothing is known of the batch.
    batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    )
    .resolve(&key(1), "xx_test", 1)
    .await
    .unwrap()
    .unwrap();
    b.script(&file(IN_BUCKET, "0099.jp2"), Scripted::Status(403));
    b.delay(Duration::from_millis(300));
    let loc = Arc::new(client(&s, &b, Some(cache.path().into())));
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    let first = {
        let (loc, p2) = (Arc::clone(&loc), p2.clone());
        tokio::spawn(async move { loc.bucket_jp2(&p2).await.is_none() })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Waits for the 403's turn to end, and then isn't sent.
    assert!(loc.bucket_jp2(&p1).await.is_none());
    assert!(first.await.unwrap());
    assert_eq!(b.hits(""), 1);
    let st = loc.bucket_status().unwrap();
    assert_eq!(
        (st.requests, st.private_batches, st.skipped_held, st.holds),
        (1, 1, 0, 0)
    );
}

#[tokio::test]
async fn a_batch_elsewhere_on_loc_is_moved_where_chronicling_america_says() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    // LoC keeps the batch under another awardee's directory; the bucket
    // has its manifest and JP2s but not the METS, so LoC is asked for it
    // under the guess.
    issue(&s, ELSEWHERE);
    s.redirect(REDIRECT, &format!("{ELSEWHERE}manifest-md5.txt"));
    in_bucket(&b);
    b.script(METS, Scripted::Status(404));
    let cache = tempfile::tempdir().unwrap();
    let loc = client(&s, &b, Some(cache.path().into()));
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    let at = batch_url(&s, "yy");
    assert_eq!(
        (p2.url.as_str(), p2.iiif_base.as_str()),
        (
            format!("{at}data/sn1/0001A/1890050301/0099.jp2").as_str(),
            iiif_of(&s, "yy", "0099").as_str()
        )
    );
    // The guess's 404, the redirect, the METS where it points; never the
    // manifest from LoC.
    assert_eq!(
        (
            s.hits(&file(BATCH, "1890050301.xml")),
            s.hits(REDIRECT),
            s.hits(&file(ELSEWHERE, "1890050301.xml")),
            s.hits(&format!("{ELSEWHERE}manifest-md5.txt")),
        ),
        (1, 1, 1, 0)
    );
    // Kept as checked, in place of the guess, and the wrong guess kept as
    // the record of which sidecars name it.
    assert_eq!(urls_kept(&cache), (Some(at.clone()), None));
    assert_eq!(
        std::fs::read_to_string(kept(&cache).join("url.wrong")).ok(),
        Some(batch_url(&s, "xx"))
    );
    // A JP2 under the wrong guess seeds no known-good item: the one LoC's
    // own answers set stays.
    let before = loc.storage_health.lock().unwrap().known_good.clone();
    loc.seed_storage_known_good(&p1_guessed_url(&s));
    let after = loc.storage_health.lock().unwrap().known_good.clone();
    assert_eq!(after, before);
    assert!(!after.unwrap_or_default().contains("/xx/batch_"));
    // The JP2 still comes from the bucket, with the URLs where the batch is.
    let (scan, hit) = loc.bucket_jp2(&p2).await.unwrap();
    assert_eq!(
        (scan.url.as_str(), scan.iiif_base.as_str()),
        (p2.url.as_str(), p2.iiif_base.as_str())
    );
    assert_eq!(hit.url, jp2_in_bucket(&b, "0099"));
    // The other page too; and a process without the bucket reads the
    // checked URL from disk and asks LoC for nothing.
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert!(p1.url.starts_with(&at), "{}", p1.url);
    let n = s.hits("");
    let off = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    assert_eq!(off.resolve(&key(1), "xx_test", 1).await.unwrap(), Some(p1));
    assert_eq!(s.hits(""), n);
}

#[tokio::test]
async fn a_404_for_a_page_under_a_guess_moves_its_batch_before_the_item_api() {
    for size in [ImageSize::Full, ImageSize::Pct(50)] {
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        issue(&s, ELSEWHERE);
        s.redirect(REDIRECT, &format!("{ELSEWHERE}manifest-md5.txt"));
        for (aw, frame, body) in [("yy", "0099", "half two"), ("yy", "0101", "half one")] {
            s.put(
                &format!(
                    "/iiif/service:ndnp:{aw}:batch_xx_test_ver01:data:sn1:0001A:1890050301:{frame}/full/pct:50/0/default.jpg"
                ),
                body.as_bytes().to_vec(),
            );
        }
        // The bucket has the bag and the METS: the page is placed under the
        // guess.
        in_bucket(&b);
        let cache = tempfile::tempdir().unwrap();
        let loc = client(&s, &b, Some(cache.path().into()));
        let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        assert!(p2.url.starts_with(&batch_url(&s, "xx")), "{}", p2.url);
        // The bucket can't serve the page (held, say): LoC is asked under
        // the guess, answers 404, and is asked again where the batch is.
        let scan = loc
            .scan_resolved(&key(2), &p2, size)
            .await
            .unwrap()
            .unwrap();
        let at = batch_url(&s, "yy");
        assert_eq!(scan.iiif_base, iiif_of(&s, "yy", "0099"), "{size}");
        match size {
            ImageSize::Full => {
                assert_eq!(scan.url, format!("{at}data/sn1/0001A/1890050301/0099.jp2"));
                assert_eq!(&scan.bytes[..], b"jp2 page two");
            }
            ImageSize::Pct(_) => {
                assert_eq!(scan.url, image_url(&scan.iiif_base, size));
                assert_eq!(&scan.bytes[..], b"half two");
            }
        }
        assert_eq!(s.hits(REDIRECT), 1, "{size}");
        assert_eq!(urls_kept(&cache), (Some(at.clone()), None), "{size}");
        // Moved for every page now, and a page placed under the guess
        // before the move is asked where the batch is, with no new lookup.
        let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
        assert!(p1.url.starts_with(&at), "{size}: {}", p1.url);
        let stale = BatchPage {
            url: file(&batch_url(&s, "xx"), "0101.jp2"),
            iiif_base: iiif_of(&s, "xx", "0101"),
            md5: p1.md5,
        };
        let scan = loc
            .scan_resolved(&key(1), &stale, size)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scan.iiif_base, p1.iiif_base, "{size}");
        assert_eq!(s.hits(REDIRECT), 1, "{size}");
    }
}

#[tokio::test]
async fn a_guess_chronicling_america_confirms_is_kept_and_the_404_stands() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    s.redirect(REDIRECT, &format!("{BATCH}manifest-md5.txt"));
    let jp2 = file(BATCH, "0099.jp2");
    s.script(&jp2, Scripted::Status(404));
    let cache = tempfile::tempdir().unwrap();
    let loc = client(&s, &b, Some(cache.path().into()));
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    assert_eq!(urls_kept(&cache), (None, Some(batch_url(&s, "xx"))));
    assert!(loc
        .scan_resolved(&key(2), &p2, ImageSize::Full)
        .await
        .unwrap()
        .is_none());
    assert_eq!(s.hits(REDIRECT), 1);
    assert_eq!(urls_kept(&cache), (Some(batch_url(&s, "xx")), None));
    // Checked now: another 404 asks nothing more.
    s.script(&jp2, Scripted::Status(404));
    assert!(loc
        .scan_resolved(&key(2), &p2, ImageSize::Full)
        .await
        .unwrap()
        .is_none());
    assert_eq!((s.hits(REDIRECT), s.hits(&jp2)), (1, 2));
}

#[tokio::test]
async fn a_guess_chronicling_america_cant_place_is_kept_and_asked_about_once() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    // No redirect: Chronicling America answers 404.
    let jp2 = file(BATCH, "0099.jp2");
    let cache = tempfile::tempdir().unwrap();
    let loc = client(&s, &b, Some(cache.path().into()));
    let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
    for _ in 0..2 {
        s.script(&jp2, Scripted::Status(404));
        assert!(loc
            .scan_resolved(&key(2), &p2, ImageSize::Full)
            .await
            .unwrap()
            .is_none());
    }
    assert_eq!((s.hits(REDIRECT), s.hits(&jp2)), (1, 2));
    assert_eq!(urls_kept(&cache), (None, Some(batch_url(&s, "xx"))));
}

#[tokio::test]
async fn a_guess_on_disk_is_used_only_while_the_bucket_is() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    let cache = tempfile::tempdir().unwrap();
    let p1 = client(&s, &b, Some(cache.path().into()))
        .resolve(&key(1), "xx_test", 1)
        .await
        .unwrap()
        .unwrap();
    // Another process with the bucket reads the bag and its guess from
    // disk; the guess seeds no known-good item for LoC's storage service.
    let on = client(&s, &b, Some(cache.path().into()));
    assert_eq!(
        on.resolve(&key(1), "xx_test", 1).await.unwrap(),
        Some(p1.clone())
    );
    assert_eq!((b.hits(MANIFEST), s.hits("")), (1, 0));
    on.seed_storage_known_good(&p1.url);
    assert_eq!(on.storage_health.lock().unwrap().known_good, None);
    // Without the bucket, the batch is looked up on LoC as if nothing were
    // on disk, and its URL kept as checked.
    let off = batch_client(
        &s,
        Duration::from_millis(10),
        Some(cache.path().into()),
        false,
    );
    assert_eq!(
        off.resolve(&key(1), "xx_test", 1).await.unwrap(),
        Some(p1.clone())
    );
    assert_eq!(s.hits("manifest-md5.txt"), 1);
    assert_eq!(urls_kept(&cache), (Some(batch_url(&s, "xx")), None));
    off.seed_storage_known_good(&p1.url);
    assert_eq!(
        off.storage_health.lock().unwrap().known_good.as_deref(),
        Some(p1.url.replace("0101.jp2", "1890050301.xml").as_str())
    );
}

#[tokio::test]
async fn a_mets_loc_serves_under_a_guess_confirms_it() {
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    both(&s, &b);
    // The bucket has the bag but not this METS: LoC serves it under the
    // guess, with the bag's md5.
    b.script(METS, Scripted::Status(404));
    let cache = tempfile::tempdir().unwrap();
    let loc = client(&s, &b, Some(cache.path().into()));
    let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
    assert_eq!(s.hits(".xml"), 1);
    assert_eq!(urls_kept(&cache), (Some(batch_url(&s, "xx")), None));
    assert!(!kept(&cache).join("url.wrong").exists());
    // Checked: the batch's JP2s seed the known-good item now.
    loc.seed_storage_known_good(&p1.url);
    assert!(loc.storage_health.lock().unwrap().known_good.is_some());
}

#[tokio::test]
async fn a_jp2_loc_serves_under_a_guess_confirms_it_and_one_that_isnt_the_bags_doesnt() {
    for good in [true, false] {
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        both(&s, &b);
        if !good {
            s.put(&file(BATCH, "0099.jp2"), b"cut short".to_vec());
        }
        let cache = tempfile::tempdir().unwrap();
        let loc = client(&s, &b, Some(cache.path().into()));
        // Bag and METS from the bucket; the JP2 from LoC (the bucket held,
        // say).
        let p2 = loc.resolve(&key(2), "xx_test", 1).await.unwrap().unwrap();
        assert_eq!(urls_kept(&cache), (None, Some(batch_url(&s, "xx"))));
        let scan = loc.scan_resolved(&key(2), &p2, ImageSize::Full).await;
        assert_eq!(scan.is_ok(), good, "{scan:?}");
        let kept = urls_kept(&cache);
        if good {
            assert_eq!(kept, (Some(batch_url(&s, "xx")), None));
            // The bag in memory is checked too: another page from LoC asks
            // nothing more of the disk.
            let p1 = loc.resolve(&key(1), "xx_test", 1).await.unwrap().unwrap();
            loc.scan_resolved(&key(1), &p1, ImageSize::Full)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(urls_kept(&cache), kept);
        } else {
            assert_eq!(kept, (None, Some(batch_url(&s, "xx"))));
        }
        assert_eq!(s.hits(REDIRECT), 0);
    }
}
