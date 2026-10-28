//! Shared prefetch fixtures and unchanged regression tests.

use super::*;
use crate::batch;
use crate::loc::LocConfig;
use chrono::NaiveDate;
use std::sync::atomic::AtomicUsize;
use usnm_core::ids::PageKey;

fn row(lccn: &str, y: i32, seq: u16, lang: &str) -> ManifestRow {
    let key = PageKey::new(lccn, NaiveDate::from_ymd_opt(y, 5, 3).unwrap(), 1, seq).unwrap();
    ManifestRow::new(&key, "batch_x", 1, Some(lang.into()), "detected", 0.5)
}

#[test]
fn samples_every_stratum_and_the_rest_in_proportion() {
    let mut rows = Vec::new();
    for i in 1..=80 {
        rows.push(row("sn1", 1885, i, "deu"));
    }
    for i in 1..=15 {
        rows.push(row("sn2", 1912, i, "eng"));
    }
    for i in 1..=5 {
        rows.push(row("sn3", 1907, i, "srp"));
    }
    let s = sample(&rows, 20);
    assert_eq!(s.len(), 20);
    let n = |l: &str| s.iter().filter(|r| r.lang_or_und() == l).count();
    // One each, then 17 shared 80:15:5 (13.6, 2.55, 0.85; the two
    // largest remainders get the last two).
    assert_eq!((n("deu"), n("eng"), n("srp")), (15, 3, 2));
    assert_eq!(sample(&rows, 20), s, "the same sample every time");
    // Fewer than the strata: the largest ones.
    let s = sample(&rows, 2);
    assert_eq!((n2(&s, "deu"), n2(&s, "eng"), n2(&s, "srp")), (1, 1, 0));
    // More than there are: everything.
    assert_eq!(sample(&rows, 500).len(), 100);
}

#[test]
fn fills_the_sample_when_small_strata_are_full() {
    // One 100-row stratum and 100 one-row strata: 150 asked, 150 given.
    let mut rows: Vec<ManifestRow> = (1..=100).map(|i| row("sn1", 1885, i, "deu")).collect();
    for y in 0..100 {
        rows.push(row("sn2", 1700 + y * 10, 1, "eng"));
    }
    assert_eq!(sample(&rows, 150).len(), 150);
}

#[test]
fn priority_reads_marc_and_iso_codes() {
    let mut r = row("sn1", 1890, 1, "ger");
    assert_eq!(priority(&r), 0);
    r.lang = Some("deu".into());
    assert_eq!(priority(&r), 0);
    r.date = NaiveDate::from_ymd_opt(1925, 1, 1).unwrap();
    assert_eq!(priority(&r), 5);
    for (code, want) in [("cze", 2), ("ces", 2), ("srp", 3), ("rus", 4), ("eng", 5)] {
        assert_eq!(priority(&row("sn1", 1890, 1, code)), want, "{code}");
    }
    let mut r = row("sn1", 1890, 1, "eng");
    r.batch = "nn_kant".into();
    assert_eq!(priority(&r), 1);
}

#[test]
fn priority_order_keeps_an_issue_together() {
    let mut rows = vec![
        row("sn1", 1890, 1, "pol"),
        row("sn1", 1890, 2, "deu"),
        row("sn2", 1890, 1, "deu"),
        row("sn3", 1890, 1, "ces"),
    ];
    // The German page came from another batch, which sorts first.
    rows[1].batch = "aaa_other".into();
    prefetch::sort(&mut rows);
    let picked = select(rows, &Options::default());
    let ids: Vec<String> = picked.iter().map(|r| r.doc_id.clone()).collect();
    // sn1's Polish page goes with its German one, in the German group.
    assert_eq!(
        ids,
        [
            "sn1_1890-05-03_ed-1_seq-1",
            "sn1_1890-05-03_ed-1_seq-2",
            "sn2_1890-05-03_ed-1_seq-1",
            "sn3_1890-05-03_ed-1_seq-1"
        ]
    );
}

#[test]
fn only_the_awardees_asked_for() {
    let mut rows = vec![
        row("sn1", 1890, 1, "deu"),
        row("sn2", 1890, 1, "deu"),
        row("sn3", 1890, 1, "eng"),
    ];
    rows[0].batch = "nbu_irons".into();
    rows[1].batch = "nbuu_other".into();
    rows[2].batch = "scu_jinx".into();
    let o = Options {
        awardees: vec!["nbu".into(), "scu".into()],
        ..Options::default()
    };
    let ids: Vec<String> = select(rows.clone(), &o)
        .into_iter()
        .map(|r| r.lccn)
        .collect();
    assert_eq!(ids, ["sn1", "sn3"], "the awardee is the whole first part");
    assert_eq!(select(rows, &Options::default()).len(), 3);
}

fn n2(s: &[ManifestRow], l: &str) -> usize {
    s.iter().filter(|r| r.lang_or_und() == l).count()
}

#[test]
fn lays_scans_out_by_set_batch_title_issue_and_page() {
    let r = row("sn83045462", 1890, 7, "ger");
    let p = scan_path(
        Path::new("/tank/ocr/scans"),
        &r,
        ImageSize::Full,
        ScanFormat::Jpeg,
    );
    assert_eq!(
        p,
        Path::new("/tank/ocr/scans/keep/batch_x/sn83045462/1890-05-03/1/7.jpg")
    );
    let e = row("sn83045462", 1890, 7, "eng");
    let p = scan_path(Path::new("/t"), &e, ImageSize::Pct(50), ScanFormat::Jpeg);
    assert_eq!(
        p,
        Path::new("/t/buffer/batch_x/sn83045462/1890-05-03/1/7.pct50.jpg")
    );
    assert_eq!(
        sidecar_path(&p),
        Path::new("/t/buffer/batch_x/sn83045462/1890-05-03/1/7.pct50.jpg.json")
    );
}

#[test]
fn keeps_non_english_and_the_worst_english_batches() {
    assert_eq!(set_of(&row("sn1", 1890, 1, "ger")), Set::Keep);
    assert_eq!(set_of(&row("sn1", 1890, 1, "yid")), Set::Keep);
    assert_eq!(set_of(&row("sn1", 1890, 1, "eng")), Set::Buffer);
    let mut r = row("sn1", 1890, 1, "eng");
    r.batch = "txdn_japan".into();
    assert_eq!(set_of(&r), Set::Keep);
    // Keep pages first, whatever the order; one set alone on request.
    let rows = vec![row("sn1", 1890, 1, "eng"), row("sn2", 1890, 1, "pol")];
    let ids = |o: &Options| -> Vec<String> {
        select(rows.clone(), o)
            .into_iter()
            .map(|r| r.lccn)
            .collect()
    };
    let issue = Options {
        order: Order::Issue,
        ..Options::default()
    };
    assert_eq!(ids(&issue), ["sn2", "sn1"]);
    let only = Options {
        sets: Sets::Buffer,
        ..Options::default()
    };
    assert_eq!(ids(&only), ["sn1"]);
}

#[tokio::test]
async fn a_buffer_at_its_cap_doesnt_block_pages_already_there() {
    let dir = tempfile::tempdir().unwrap();
    let r = row("sn1", 1890, 1, "eng");
    let manifest = dir.path().join("m.parquet");
    std::fs::write(
        &manifest,
        prefetch::encode(std::slice::from_ref(&r)).unwrap(),
    )
    .unwrap();
    let out = dir.path().join("scans");
    let path = scan_path(&out, &r, ImageSize::Full, ScanFormat::Jpeg);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"jpeg").unwrap();
    let side = Sidecar {
        doc_id: r.doc_id.clone(),
        batch: r.batch.clone(),
        batch_version: 1,
        loc_url: String::new(),
        iiif_base: String::new(),
        resolution: "full".into(),
        fetched_at: Utc::now(),
        bytes: 4,
        sha256: sha256(b"jpeg"),
        download_ms: 0,
        user_agent: String::new(),
        format: "jpeg".into(),
        md5: None,
    };
    std::fs::write(sidecar_path(&path), serde_json::to_vec(&side).unwrap()).unwrap();
    let opts = Options {
        manifest,
        out,
        buffer_cap: 1,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let s = run(
        opts,
        client("http://127.0.0.1:9", Duration::from_millis(1)),
        rx,
    )
    .await
    .unwrap();
    assert_eq!((s.state.as_str(), s.already_there), ("done", 1));
}

#[tokio::test]
async fn a_consumed_buffer_page_isnt_fetched_again() {
    let dir = tempfile::tempdir().unwrap();
    let r = row("sn1", 1890, 1, "eng");
    let manifest = dir.path().join("m.parquet");
    std::fs::write(
        &manifest,
        prefetch::encode(std::slice::from_ref(&r)).unwrap(),
    )
    .unwrap();
    let out = dir.path().join("scans");
    let path = scan_path(&out, &r, ImageSize::Full, ScanFormat::Jpeg);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(consumed_path(&path), b"{}").unwrap();
    let opts = Options {
        manifest,
        out,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    // Nothing listens: a fetch would fail.
    let s = run(
        opts,
        client("http://127.0.0.1:9", Duration::from_millis(1)),
        rx,
    )
    .await
    .unwrap();
    assert_eq!((s.already_there, s.fetched, s.failed_transient), (1, 0, 0));
}

#[test]
fn counts_only_scans_and_a_missing_buffer_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(dir_scan_bytes(&dir.path().join("none")).unwrap(), 0);
    std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
    std::fs::write(dir.path().join("a/b/1.jpg"), b"1234").unwrap();
    std::fs::write(dir.path().join("a/b/1.jpg.json"), b"{}").unwrap();
    std::fs::write(dir.path().join("a/2.jpg.done"), b"{}").unwrap();
    assert_eq!(dir_scan_bytes(dir.path()).unwrap(), 4);
}

#[tokio::test]
async fn waits_at_the_buffer_cap_until_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<ManifestRow> = (1..=2).map(|i| row("sn1", 1890, i, "eng")).collect();
    let manifest = dir.path().join("m.parquet");
    std::fs::write(&manifest, prefetch::encode(&rows).unwrap()).unwrap();
    let out = dir.path().join("scans");
    // Something in the buffer already, over a cap of 1 byte.
    std::fs::create_dir_all(out.join("buffer/x")).unwrap();
    std::fs::write(out.join("buffer/x/1.jpg"), b"jpeg").unwrap();
    let opts = Options {
        manifest,
        out: out.clone(),
        buffer_cap: 1,
        ..Options::default()
    };
    let (tx, rx) = watch::channel(false);
    let run = tokio::spawn(run(
        opts,
        client("http://127.0.0.1:9", Duration::from_millis(1)),
        rx,
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let status: Status =
        serde_json::from_slice(&std::fs::read(out.join("status-full.json")).unwrap()).unwrap();
    assert_eq!(status.state, "waiting");
    assert_eq!(status.buffer_bytes, 4);
    tx.send_replace(true);
    let s = run.await.unwrap().unwrap();
    assert_eq!((s.state.as_str(), s.fetched), ("stopped", 0));
}

#[test]
fn projects_by_stratum() {
    let manifest: Vec<ManifestRow> = (1..=10)
        .map(|i| row("sn1", 1885, i, "deu"))
        .chain((1..=10).map(|i| row("sn2", 1912, i, "eng")))
        .chain((1..=4).map(|i| row("sn3", 1907, i, "srp")))
        .collect();
    let line = |r: &ManifestRow, bytes: u64| Sidecar {
        doc_id: r.doc_id.clone(),
        batch: r.batch.clone(),
        batch_version: 1,
        loc_url: String::new(),
        iiif_base: String::new(),
        resolution: "full".into(),
        fetched_at: Utc::now(),
        bytes,
        sha256: String::new(),
        download_ms: 3000,
        user_agent: USER_AGENT.into(),
        format: "jpeg".into(),
        md5: None,
    };
    let index = vec![
        line(&manifest[0], 4_000_000),
        line(&manifest[1], 2_000_000),
        line(&manifest[10], 1_000_000),
    ];
    let e = estimate(
        &manifest,
        &index,
        Duration::from_millis(3500),
        Duration::from_secs(1),
    )
    .unwrap();
    // deu: 10 × 3 MB; eng: 10 × 1 MB; srp unsampled: 4 × the mean (7/3 MB).
    let want = 30e6 + 10e6 + 4.0 * 7e6 / 3.0;
    assert!(
        (e.projected_bytes - want).abs() < 1.0,
        "{}",
        e.projected_bytes
    );
    assert_eq!((e.unsampled_strata, e.unsampled_pages), (1, 4));
    assert_eq!(e.manifest_issues, 3);
    // 3 s downloads: 3 s a page alone, 1.5 s with two in flight.
    assert!((e.image_hours_c1 - 24.0 * 3.0 / 3600.0).abs() < 1e-9);
    assert!((e.image_hours_c2 - 24.0 * 1.5 / 3600.0).abs() < 1e-9);
    assert!((e.api_hours - 3.0 * 3.5 / 3600.0).abs() < 1e-9);
}

/// A stand-in loc.gov with one issue of three pages. The image server
/// answers its first request 429 with `Retry-After: 2`, serves page 3's
/// image as missing (404), and records when each image request came.
async fn fake_loc(times: Arc<Mutex<Vec<std::time::Instant>>>, hits: Arc<AtomicUsize>) -> String {
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
        [{"mimetype": "image/jp2", "url": jp2("0003")}],
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
            "/iiif/{service}/full/{size}/0/default.jpg",
            axum::routing::get(
                move |axum::extract::Path((service, size)): axum::extract::Path<(
                    String,
                    String,
                )>,
                      headers: axum::http::HeaderMap| {
                    let times = times.clone();
                    let hits = hits.clone();
                    async move {
                        assert!(headers["user-agent"]
                            .to_str()
                            .unwrap()
                            .starts_with("usnewsmap-ocr-worker/"));
                        times.lock().unwrap().push(std::time::Instant::now());
                        if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                            return (S::TOO_MANY_REQUESTS, [("retry-after", "2")]).into_response();
                        }
                        if service.ends_with("0003") {
                            return S::NOT_FOUND.into_response();
                        }
                        format!("jpeg {service} {size}")
                            .into_bytes()
                            .into_response()
                    }
                },
            ),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    root
}

fn client(root: &str, tile_gap: Duration) -> Arc<LocClient> {
    Arc::new(
        LocClient::new(LocConfig {
            api_gap: Duration::from_millis(10),
            tile_gap,
            storage_gap: tile_gap,
            attempts: 3,
            backoff: Duration::from_millis(10),
            api_root: root.to_owned(),
            iiif_root: format!("{root}/iiif/"),
            storage_root: format!("{root}/storage-services/service/ndnp/"),
            batches_root: format!("{root}/data/batches/"),
            ..LocConfig::default()
        })
        .unwrap(),
    )
}

async fn manifest(dir: &Path) -> PathBuf {
    let rows: Vec<ManifestRow> = (1..=3).map(|i| row("sn1", 1890, i, "deu")).collect();
    let p = dir.join("manifest.parquet");
    std::fs::write(&p, prefetch::encode(&rows).unwrap()).unwrap();
    p
}

#[tokio::test]
async fn fetches_paced_honours_retry_after_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let times = Arc::new(Mutex::new(Vec::new()));
    let hits = Arc::new(AtomicUsize::new(0));
    let root = fake_loc(times.clone(), hits.clone()).await;
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: manifest(dir.path()).await,
        out: out.clone(),
        size: ImageSize::Pct(50),
        concurrency: 2,
        progress_every: 1,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let t0 = std::time::Instant::now();
    let s = run(
        opts.clone(),
        client(&root, Duration::from_millis(300)),
        rx.clone(),
    )
    .await
    .unwrap();
    assert!(
        t0.elapsed() >= Duration::from_secs(2),
        "waited out Retry-After"
    );
    assert_eq!(s.state, "done");
    assert_eq!((s.fetched, s.failed_permanent, s.already_there), (2, 1, 0));
    // Every image request after the first waited for the hold, and they
    // were at least the gap apart.
    let t = times.lock().unwrap().clone();
    assert_eq!(t.len(), 4);
    assert!(t[1] - t[0] >= Duration::from_secs(2));
    for w in t[1..].windows(2) {
        assert!(
            w[1] - w[0] >= Duration::from_millis(290),
            "{:?}",
            w[1] - w[0]
        );
    }
    let r = row("sn1", 1890, 1, "deu");
    let scan = scan_path(&out, &r, ImageSize::Pct(50), ScanFormat::Jpeg);
    let body = std::fs::read(&scan).unwrap();
    assert!(String::from_utf8_lossy(&body).ends_with("0001 pct:50"));
    let side: Sidecar =
        serde_json::from_slice(&std::fs::read(sidecar_path(&scan)).unwrap()).unwrap();
    assert_eq!(side.sha256, sha256(&body));
    assert_eq!(side.bytes, body.len() as u64);
    assert!(side.loc_url.ends_with(":0001/full/pct:50/0/default.jpg"));
    assert_eq!(side.resolution, "scaled:50");
    let index =
        read_index(&std::fs::read_to_string(out.join("index-pct50.jsonl")).unwrap()).unwrap();
    assert_eq!(index.len(), 2);
    let failures = std::fs::read_to_string(out.join("failures-pct50.jsonl")).unwrap();
    assert!(failures.contains("sn1_1890-05-03_ed-1_seq-3") && failures.contains("permanent"));
    let status: Status =
        serde_json::from_slice(&std::fs::read(out.join("status-pct50.json")).unwrap()).unwrap();
    assert_eq!(status.state, "done");

    // Again: the two scans are there and verified, so only page 3 is asked for.
    let before = hits.load(Ordering::SeqCst);
    let s = run(
        opts.clone(),
        client(&root, Duration::from_millis(10)),
        rx.clone(),
    )
    .await
    .unwrap();
    assert_eq!((s.already_there, s.fetched, s.failed_permanent), (2, 0, 1));
    assert_eq!(hits.load(Ordering::SeqCst), before + 1);

    // A scan that no longer matches its sidecar is fetched again.
    std::fs::write(&scan, b"changed").unwrap();
    let s = run(opts, client(&root, Duration::from_millis(10)), rx)
        .await
        .unwrap();
    assert_eq!((s.already_there, s.fetched), (1, 1));
}

#[tokio::test]
async fn one_process_at_a_time_and_a_run_of_failures_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("scans");
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join(".prefetch.lock");
    let held = lock(&path).unwrap();
    // flock is per open file description, so a second open file is refused.
    assert!(lock(&path).is_err());
    drop(held);
    // Nothing listens here: every page fails, transiently.
    let opts = Options {
        manifest: manifest(dir.path()).await,
        out: out.clone(),
        concurrency: 1,
        max_failures_in_a_row: 2,
        stop_min_titles: 1,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let err = run(
        opts,
        client("http://127.0.0.1:9", Duration::from_millis(1)),
        rx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("failed in a row"), "{err}");
    let status: Status =
        serde_json::from_slice(&std::fs::read(out.join("status-full.json")).unwrap()).unwrap();
    assert_eq!(status.state, "failed");
    assert_eq!(status.failed_transient, 2);
}

fn dated(lccn: &str, date: &str, seq: u16) -> ManifestRow {
    let key = PageKey::new(lccn, date.parse().unwrap(), 1, seq).unwrap();
    ManifestRow::new(&key, "batch_x", 1, Some("deu".into()), "detected", 0.5)
}

fn write_manifest(dir: &Path, mut rows: Vec<ManifestRow>) -> PathBuf {
    prefetch::sort(&mut rows);
    let p = dir.join("manifest.parquet");
    std::fs::write(&p, prefetch::encode(&rows).unwrap()).unwrap();
    p
}

/// A stand-in loc.gov whose items answer 200 (one page each) unless
/// their `<lccn>/<date>` is in `bare_503` (503 at once, no
/// Retry-After) or `err_500`; counts item requests by `<lccn>/<date>`.
async fn titles_loc(
    bare_503: &'static [&'static str],
    err_500: &'static [&'static str],
    hits: Arc<Mutex<BTreeMap<String, u32>>>,
) -> String {
    use axum::http::StatusCode as S;
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route(
            "/item/{lccn}/{date}/{ed}/",
            axum::routing::get(
                move |axum::extract::Path((lccn, date, _)): axum::extract::Path<(
                    String,
                    String,
                    String,
                )>| {
                    let hits = hits.clone();
                    async move {
                        let k = format!("{lccn}/{date}");
                        *hits.lock().unwrap().entry(k.clone()).or_default() += 1;
                        if bare_503.contains(&k.as_str()) {
                            return S::SERVICE_UNAVAILABLE.into_response();
                        }
                        if err_500.contains(&k.as_str()) {
                            return S::INTERNAL_SERVER_ERROR.into_response();
                        }
                        let jp2 = |n: &str| format!("https://tile.loc.gov/storage-services/service/ndnp/x/batch_x/data/{lccn}/1/{date}/{n}.jp2");
                        axum::Json(serde_json::json!({"resources": [{"files": [
                            [{"mimetype": "image/jp2", "url": jp2("0001")}],
                            [{"mimetype": "image/jp2", "url": jp2("0002")}],
                        ]}]}))
                        .into_response()
                    }
                },
            ),
        )
        .route(
            "/iiif/{service}/full/{size}/0/default.jpg",
            axum::routing::get(|| async { b"jpeg".to_vec() }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    root
}

fn failures_of(out: &Path) -> Vec<Failure> {
    std::fs::read_to_string(out.join("failures-full.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn one_titles_broken_items_are_skipped_cooled_off_and_dont_stop_the_run() {
    let dir = tempfile::tempdir().unwrap();
    // sn1 answers, sn2's three issues are broken, sn3 answers.
    let mut rows = vec![dated("sn1", "1904-07-29", 1), dated("sn1", "1904-07-29", 2)];
    for d in ["1904-07-30", "1904-07-31", "1904-08-01"] {
        rows.push(dated("sn2", d, 1));
        rows.push(dated("sn2", d, 2));
    }
    rows.push(dated("sn3", "1904-08-02", 1));
    let hits: Arc<Mutex<BTreeMap<String, u32>>> = Arc::default();
    let root = titles_loc(
        &["sn2/1904-07-30", "sn2/1904-07-31", "sn2/1904-08-01"],
        &[],
        hits.clone(),
    )
    .await;
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows),
        out: out.clone(),
        concurrency: 1,
        // Without the titles rule, sn2 alone would stop the run.
        max_failures_in_a_row: 2,
        item_cooloff: Duration::from_millis(300),
        item_max_cooloffs: 1,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let t0 = std::time::Instant::now();
    let s = run(
        opts.clone(),
        client(&root, Duration::from_millis(1)),
        rx.clone(),
    )
    .await
    .unwrap();
    assert_eq!(s.state, "done");
    assert_eq!(s.fetched, 3);
    // Each sn2 page skipped once while cooling (6), then permanent (6).
    assert_eq!(
        (s.failed_item_error, s.failed_permanent, s.failed_transient),
        (6, 6, 0)
    );
    assert_eq!((s.issues_cooling, s.issues_item_permanent), (0, 3));
    // One item request per issue per try, nothing held: the client's
    // backoff (10 ms) and the cool-off are all the waiting there was.
    {
        let h = hits.lock().unwrap();
        for d in ["1904-07-30", "1904-07-31", "1904-08-01"] {
            assert_eq!(h[&format!("sn2/{d}")], 2, "{d}");
        }
        assert_eq!(h["sn3/1904-08-02"], 1);
    }
    assert!(t0.elapsed() < Duration::from_secs(5));
    let f = failures_of(&out);
    let kinds = |k: &str| f.iter().filter(|x| x.kind == k).count();
    assert_eq!((kinds("loc_item_error"), kinds("permanent")), (6, 6));
    let errs: BTreeMap<String, ItemError> =
        serde_json::from_slice(&std::fs::read(out.join("item-errors.json")).unwrap()).unwrap();
    assert_eq!(errs.len(), 3);
    assert!(errs
        .values()
        .all(|e| e.errors == 2 && e.retry_after.is_none()));

    // Again: the permanent issues aren't asked for.
    let before = hits.lock().unwrap().clone();
    let s = run(opts, client(&root, Duration::from_millis(1)), rx)
        .await
        .unwrap();
    assert_eq!((s.already_there, s.failed_permanent, s.fetched), (3, 6, 0));
    assert_eq!(*hits.lock().unwrap(), before);
}

#[tokio::test]
async fn waits_for_cooling_issues_and_keeps_them_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let rows = vec![
        dated("sn1", "1904-07-29", 1),
        dated("sn2", "1904-07-30", 1),
        dated("sn2", "1904-07-30", 2),
    ];
    let hits: Arc<Mutex<BTreeMap<String, u32>>> = Arc::default();
    let root = titles_loc(&["sn2/1904-07-30"], &[], hits.clone()).await;
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows),
        out: out.clone(),
        concurrency: 1,
        ..Options::default()
    };
    let (tx, rx) = watch::channel(false);
    let r = tokio::spawn(run(
        opts.clone(),
        client(&root, Duration::from_millis(1)),
        rx,
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let status: Status =
        serde_json::from_slice(&std::fs::read(out.join("status-full.json")).unwrap()).unwrap();
    assert_eq!(status.state, "cooling");
    assert_eq!(status.issues_cooling, 1);
    tx.send_replace(true);
    let s = r.await.unwrap().unwrap();
    assert_eq!((s.state.as_str(), s.fetched), ("stopped", 1));

    // A new process: the issue is still cooling, so it isn't asked for,
    // and the known-good item comes from the index.
    let asked = hits.lock().unwrap()["sn2/1904-07-30"];
    let (tx, rx) = watch::channel(false);
    let r = tokio::spawn(run(opts, client(&root, Duration::from_millis(1)), rx));
    tokio::time::sleep(Duration::from_millis(500)).await;
    tx.send_replace(true);
    let s = r.await.unwrap().unwrap();
    assert_eq!((s.already_there, s.failed_item_error), (1, 2));
    assert_eq!(hits.lock().unwrap()["sn2/1904-07-30"], asked);
}

#[tokio::test]
async fn the_stop_guard_needs_failures_from_several_titles() {
    let dir = tempfile::tempdir().unwrap();
    // A fails twice, B answers (the run of failures starts again),
    // then C, D and E fail: 3 pages from 3 titles. F is never reached.
    let rows = vec![
        dated("sna", "1890-01-01", 1),
        dated("sna", "1890-01-02", 1),
        dated("snb", "1890-01-01", 1),
        dated("snc", "1890-01-01", 1),
        dated("snd", "1890-01-01", 1),
        dated("sne", "1890-01-01", 1),
        dated("snf", "1890-01-01", 1),
    ];
    let hits: Arc<Mutex<BTreeMap<String, u32>>> = Arc::default();
    let root = titles_loc(
        &[],
        &[
            "sna/1890-01-01",
            "sna/1890-01-02",
            "snc/1890-01-01",
            "snd/1890-01-01",
            "sne/1890-01-01",
            "snf/1890-01-01",
        ],
        hits.clone(),
    )
    .await;
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows),
        out: out.clone(),
        concurrency: 1,
        max_failures_in_a_row: 3,
        stop_min_titles: 3,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let err = run(opts, client(&root, Duration::from_millis(1)), rx)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("3 pages from 3 titles failed in a row"),
        "{err}"
    );
    let status: Status =
        serde_json::from_slice(&std::fs::read(out.join("status-full.json")).unwrap()).unwrap();
    assert_eq!((status.fetched, status.failed_transient), (1, 5));
    assert!(!hits.lock().unwrap().contains_key("snf/1890-01-01"));

    // Many pages of one title failing don't stop it.
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<ManifestRow> = (1..=6).map(|s| dated("sna", "1890-01-01", s)).collect();
    let opts = Options {
        manifest: write_manifest(dir.path(), rows),
        out: dir.path().join("scans"),
        concurrency: 1,
        max_failures_in_a_row: 3,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let s = run(opts, client(&root, Duration::from_millis(1)), rx)
        .await
        .unwrap();
    assert_eq!((s.state.as_str(), s.failed_transient), ("done", 6));
}

use crate::standin::{Page, Scripted, StandIn};

const BATCH: &str = "/storage-services/service/ndnp/xx/batch_xx_test_ver01/";

fn batch_row(lccn: &str, date: &str, seq: u16) -> ManifestRow {
    let key = PageKey::new(lccn, date.parse().unwrap(), 1, seq).unwrap();
    ManifestRow::new(&key, "xx_test", 1, Some("deu".into()), "detected", 0.5)
}

fn batch_cfg(s: &StandIn, cache: &Path) -> LocConfig {
    LocConfig {
        api_gap: Duration::from_millis(10),
        tile_gap: Duration::from_millis(10),
        storage_gap: Duration::from_millis(10),
        attempts: 3,
        backoff: Duration::from_millis(10),
        api_root: s.root.clone(),
        iiif_root: s.iiif_root(),
        storage_root: s.storage_root(),
        batches_root: s.batches_root(),
        batch_cache: Some(cache.to_owned()),
        ..LocConfig::default()
    }
}

fn batch_loc(s: &StandIn, cache: &Path) -> Arc<LocClient> {
    Arc::new(LocClient::new(batch_cfg(s, cache)).unwrap())
}

#[test]
fn no_loc_goes_by_the_manifests_directory_name() {
    let p = |s: &str| Path::new(s).to_path_buf();
    let pre = vec!["queue-quality-".to_owned(), " ".to_owned()];
    assert!(no_loc_for(
        &p("/m/queue-quality-20261025/manifest.parquet"),
        &pre
    ));
    assert!(!no_loc_for(
        &p("/m/queue-ocr-20261025/manifest.parquet"),
        &pre
    ));
    // The directory's name, not the file's or a parent's further up.
    assert!(!no_loc_for(&p("/queue-quality-x/m/manifest.parquet"), &pre));
    // A relative path is read from the current directory.
    let here = std::env::current_dir().unwrap();
    let here = here.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(no_loc_for(
        &p("manifest.parquet"),
        std::slice::from_ref(&here)
    ));
    assert!(no_loc_for(&p("./manifest.parquet"), &[here]));
    // Off by default; a blank prefix matches nothing.
    assert!(!no_loc_for(&p("/m/queue-quality-1/manifest.parquet"), &[]));
    assert!(!no_loc_for(&p("/m/x/manifest.parquet"), &[" ".into()]));
}

#[tokio::test]
async fn a_no_loc_manifest_defers_what_only_loc_has_and_a_later_run_fetches_it() {
    let dir = tempfile::tempdir().unwrap();
    let s = StandIn::start().await;
    s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0011",
            jp2: b"jp2 one",
        }],
    );
    let item = serde_json::json!({"resources": [{"files": [
        [],
        [{"mimetype": "image/jp2", "url": format!("{}{BATCH}data/sn1/0001A/1890050301/0012.jp2", s.root)}],
    ]}]});
    s.put(
        "/item/sn1/1890-05-03/ed-1/",
        serde_json::to_vec(&item).unwrap(),
    );
    s.put(
        "/iiif/service:ndnp:xx:batch_xx_test_ver01:data:sn1:0001A:1890050301:0012/full/full/0/default.jpg",
        b"iiif jpeg two".to_vec(),
    );
    // Page 1 is in the batch, page 2 only through the item API.
    let rows: Vec<ManifestRow> = (1..=2).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
    let qdir = dir.path().join("queue-quality-20261025");
    std::fs::create_dir_all(&qdir).unwrap();
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(&qdir, rows.clone()),
        out: out.clone(),
        concurrency: 1,
        // One failure would stop it, if deferring were a failure.
        max_failures_in_a_row: 1,
        stop_min_titles: 1,
        no_loc_prefixes: vec!["queue-quality-".into()],
        ..Options::default()
    };
    let cache = dir.path().join("batches");
    let (_tx, rx) = watch::channel(false);
    let st = run(opts.clone(), batch_loc(&s, &cache), rx.clone())
        .await
        .unwrap();
    assert_eq!(st.state, "done");
    assert!(st.no_loc);
    assert_eq!(
        (
            st.finished,
            st.fetched,
            st.deferred_needs_loc,
            st.failed_transient,
            st.failed_permanent,
            st.issues_cooling
        ),
        (2, 0, 2, 0, 0, 0)
    );
    // The batch manifest and METS came from LoC; no JP2, item or IIIF.
    assert_eq!(s.hits(&format!("{BATCH}manifest-md5.txt")), 1);
    assert_eq!(s.hits(".xml"), 1);
    assert_eq!(
        (s.hits(".jp2"), s.hits("/ed-1/"), s.hits(".jpg")),
        (0, 0, 0)
    );
    // Nothing on disk, no failure lines, no cool-off for either.
    let failures = std::fs::read_to_string(out.join("failures-full.jsonl")).unwrap();
    assert!(failures.is_empty(), "{failures}");
    for r in &rows {
        assert_eq!(there(&out, r, ImageSize::Full).await, None);
    }
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["deferred_needs_loc"], 2);

    // A run that may ask LoC takes both.
    let st = run(
        Options {
            no_loc_prefixes: Vec::new(),
            ..opts
        },
        batch_loc(&s, &cache),
        rx,
    )
    .await
    .unwrap();
    assert_eq!(
        (st.fetched, st.deferred_needs_loc, st.no_loc),
        (2, 0, false)
    );
}

#[tokio::test]
async fn a_no_loc_manifest_asks_loc_for_no_scaled_scan() {
    let dir = tempfile::tempdir().unwrap();
    let s = StandIn::start().await;
    let rows = vec![batch_row("sn1", "1890-05-03", 1)];
    let qdir = dir.path().join("queue-quality-1");
    std::fs::create_dir_all(&qdir).unwrap();
    let opts = Options {
        manifest: write_manifest(&qdir, rows),
        out: dir.path().join("scans"),
        size: ImageSize::Pct(50),
        buffer_size: ImageSize::Pct(50),
        concurrency: 1,
        // A full buffer: a page that waited for room would never end.
        buffer_cap: 0,
        no_loc_prefixes: vec!["queue-quality-".into()],
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let st = run(opts, batch_loc(&s, &dir.path().join("batches")), rx)
        .await
        .unwrap();
    assert_eq!((st.deferred_needs_loc, st.fetched), (1, 0));
    assert_eq!(s.hits(""), 0, "nothing asked of LoC");
}

#[tokio::test]
async fn fetches_jp2s_through_the_batch_keeps_old_jpegs_and_falls_back_to_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let s = StandIn::start().await;
    // Pages 1 and 2 are in the batch; page 3 isn't in the METS.
    s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &[
            Page {
                seq: 1,
                frame: "0011",
                jp2: b"jp2 one",
            },
            Page {
                seq: 2,
                frame: "0010",
                jp2: b"jp2 two",
            },
        ],
    );
    let item = serde_json::json!({"resources": [{"files": [
        [], [],
        [{"mimetype": "image/jp2", "url": format!("{}{BATCH}data/sn1/0001A/1890050301/0012.jp2", s.root)}],
    ]}]});
    s.put(
        "/item/sn1/1890-05-03/ed-1/",
        serde_json::to_vec(&item).unwrap(),
    );
    s.put(
        "/iiif/service:ndnp:xx:batch_xx_test_ver01:data:sn1:0001A:1890050301:0012/full/full/0/default.jpg",
        b"iiif jpeg three".to_vec(),
    );
    let rows: Vec<ManifestRow> = (1..=3).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
    let out = dir.path().join("scans");
    // Page 1 is on disk already as a JPEG, with a sidecar from before
    // the batch route (no format, no md5).
    let old = scan_path(&out, &rows[0], ImageSize::Full, ScanFormat::Jpeg);
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"old jpeg").unwrap();
    let side = serde_json::json!({
        "doc_id": rows[0].doc_id, "batch": "xx_test", "batch_version": 1,
        "loc_url": "https://tile.loc.gov/image-services/iiif/service:x/full/full/0/default.jpg",
        "iiif_base": "https://tile.loc.gov/image-services/iiif/service:x",
        "resolution": "full", "fetched_at": "2026-10-11T00:00:00Z", "bytes": 8,
        "sha256": sha256(b"old jpeg"), "download_ms": 1, "user_agent": "x"
    });
    std::fs::write(sidecar_path(&old), serde_json::to_vec(&side).unwrap()).unwrap();
    let opts = Options {
        manifest: write_manifest(dir.path(), rows.clone()),
        out: out.clone(),
        concurrency: 1,
        ..Options::default()
    };
    let cache = dir.path().join("batches");
    let (_tx, rx) = watch::channel(false);
    let st = run(opts.clone(), batch_loc(&s, &cache), rx.clone())
        .await
        .unwrap();
    assert_eq!(
        (
            st.already_there,
            st.fetched,
            st.fetched_jp2,
            st.fetched_item_api
        ),
        (1, 2, 1, 1)
    );
    assert_eq!(std::fs::read(&old).unwrap(), b"old jpeg", "kept");
    // Page 2: the JP2, with the storage URL, the IIIF base and the bag's md5.
    let jp2 = scan_path(&out, &rows[1], ImageSize::Full, ScanFormat::Jp2);
    assert_eq!(std::fs::read(&jp2).unwrap(), b"jp2 two");
    let side: Sidecar =
        serde_json::from_slice(&std::fs::read(sidecar_path(&jp2)).unwrap()).unwrap();
    assert_eq!(side.format, "jp2");
    assert_eq!(
        side.loc_url,
        format!("{}{BATCH}data/sn1/0001A/1890050301/0010.jp2", s.root)
    );
    assert_eq!(
        side.iiif_base,
        format!(
            "{}service:ndnp:xx:batch_xx_test_ver01:data:sn1:0001A:1890050301:0010",
            s.iiif_root()
        )
    );
    assert_eq!(
        side.md5.as_deref(),
        Some(batch::md5_hex(b"jp2 two").as_str())
    );
    assert_eq!(side.sha256, sha256(b"jp2 two"));
    // Page 3: through the item API, as the IIIF JPEG.
    let p3 = scan_path(&out, &rows[2], ImageSize::Full, ScanFormat::Jpeg);
    assert_eq!(std::fs::read(&p3).unwrap(), b"iiif jpeg three");
    let side3: Sidecar =
        serde_json::from_slice(&std::fs::read(sidecar_path(&p3)).unwrap()).unwrap();
    assert_eq!(
        (side3.format.as_str(), side3.md5.as_deref()),
        ("jpeg", None)
    );
    // The index holds both formats.
    let index =
        read_index(&std::fs::read_to_string(out.join("index-full.jsonl")).unwrap()).unwrap();
    assert_eq!(index.len(), 2);
    // LoC's bucket is off by default: nothing about it is written.
    assert_eq!((st.fetched_bucket, st.bucket.as_ref()), (0, None));
    assert!(serde_json::to_value(&st).unwrap().get("bucket").is_none());
    assert!(!out.join("sources-full.jsonl").exists());
    // One manifest, one METS, one JP2, one item, one IIIF image.
    assert_eq!(
        (
            s.hits(&format!("{BATCH}manifest-md5.txt")),
            s.hits(".xml"),
            s.hits(".jp2"),
            s.hits("/ed-1/"),
            s.hits(".jpg")
        ),
        (1, 1, 1, 1, 1)
    );

    // Again, in a new process: everything is there, in either format,
    // and nothing is asked of LoC.
    let before = s.hits("");
    let st = run(opts, batch_loc(&s, &cache), rx).await.unwrap();
    assert_eq!((st.already_there, st.fetched), (3, 0));
    assert_eq!(s.hits(""), before);
    // A JP2 that no longer matches its bag md5 isn't taken as there.
    let mut changed = side.clone();
    changed.md5 = Some(batch::md5_hex(b"other"));
    std::fs::write(sidecar_path(&jp2), serde_json::to_vec(&changed).unwrap()).unwrap();
    assert_eq!(there(&out, &rows[1], ImageSize::Full).await, None);
}

#[tokio::test]
async fn a_storage_item_error_cools_off_that_route_only() {
    let dir = tempfile::tempdir().unwrap();
    let s = StandIn::start().await;
    s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: b"a",
        }],
    );
    s.add_issue(
        BATCH,
        "sn2",
        "0002A",
        "1890050301",
        &[Page {
            seq: 1,
            frame: "0001",
            jp2: b"b",
        }],
    );
    // sn2's METS answers 503 at once, while sn1's answered.
    for _ in 0..2 {
        s.script(
            &format!("{BATCH}data/sn2/0002A/1890050301/1890050301.xml"),
            Scripted::Status(503),
        );
    }
    let rows = vec![
        batch_row("sn1", "1890-05-03", 1),
        batch_row("sn2", "1890-05-03", 1),
    ];
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows),
        out: out.clone(),
        concurrency: 1,
        item_cooloff: Duration::from_secs(3600),
        ..Options::default()
    };
    let (tx, rx) = watch::channel(false);
    let r = tokio::spawn(run(opts, batch_loc(&s, &dir.path().join("b")), rx));
    tokio::time::sleep(Duration::from_millis(500)).await;
    tx.send_replace(true);
    let st = r.await.unwrap().unwrap();
    assert_eq!(
        (st.fetched, st.failed_item_error, st.issues_cooling),
        (1, 1, 1)
    );
    let errs: BTreeMap<String, ItemError> =
        serde_json::from_slice(&std::fs::read(out.join("item-errors.json")).unwrap()).unwrap();
    assert_eq!(
        errs.keys().collect::<Vec<_>>(),
        ["storage:sn2/1890-05-03/ed-1"]
    );
    // Nothing went round the item API.
    assert_eq!(s.hits("/ed-1/"), 0);
    assert_eq!(st.loc_hold_secs, None);
}

fn partner_mirrors(partner: &StandIn) -> Arc<Mirrors> {
    Arc::new(
        Mirrors::new(mirror::MirrorConfig {
            partners: vec![mirror::Partner {
                awardee: "xx",
                name: "partner",
                root: partner.root.clone(),
                json_names_file: false,
            }],
            partner_gap: Duration::from_millis(10),
            ..mirror::MirrorConfig::default()
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn a_mirrors_jp2_that_is_the_bags_saves_loc_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let s = StandIn::start().await;
    s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &[
            Page {
                seq: 1,
                frame: "0011",
                jp2: b"jp2 one",
            },
            Page {
                seq: 2,
                frame: "0010",
                jp2: b"jp2 two",
            },
        ],
    );
    // The partner has page 1 as LoC does, and something else for page 2.
    let partner = StandIn::start().await;
    partner.put("/lccn/sn1/1890-05-03/ed-1/seq-1.jp2", b"jp2 one".to_vec());
    partner.put(
        "/lccn/sn1/1890-05-03/ed-1/seq-2.jp2",
        b"not the bag's".to_vec(),
    );
    let rows: Vec<ManifestRow> = (1..=2).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows.clone()),
        out: out.clone(),
        concurrency: 1,
        mirrors: Some(partner_mirrors(&partner)),
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let st = run(opts, batch_loc(&s, &dir.path().join("batches")), rx)
        .await
        .unwrap();
    assert_eq!(
        (
            st.fetched,
            st.fetched_jp2,
            st.fetched_mirror,
            st.fetched_item_api
        ),
        (2, 2, 1, 0)
    );
    let m = &st.mirrors["partner"];
    assert_eq!((m.hits, m.md5_mismatches), (1, 1));
    // LoC placed both pages and sent only page 2's JP2.
    assert_eq!(
        (
            s.hits(&format!("{BATCH}manifest-md5.txt")),
            s.hits(".xml"),
            s.hits("/0011.jp2"),
            s.hits("/0010.jp2")
        ),
        (1, 1, 0, 1)
    );
    // Page 1 is recorded as LoC's file, as if LoC had sent it.
    let jp2 = scan_path(&out, &rows[0], ImageSize::Full, ScanFormat::Jp2);
    assert_eq!(std::fs::read(&jp2).unwrap(), b"jp2 one");
    let side: Sidecar =
        serde_json::from_slice(&std::fs::read(sidecar_path(&jp2)).unwrap()).unwrap();
    assert_eq!(
        side.loc_url,
        format!("{}{BATCH}data/sn1/0001A/1890050301/0011.jp2", s.root)
    );
    assert_eq!(
        side.md5.as_deref(),
        Some(batch::md5_hex(b"jp2 one").as_str())
    );
    // Page 2 is LoC's own.
    let jp2 = scan_path(&out, &rows[1], ImageSize::Full, ScanFormat::Jp2);
    assert_eq!(std::fs::read(&jp2).unwrap(), b"jp2 two");
    // Where page 1's bytes came from.
    let lines = std::fs::read_to_string(out.join("sources-full.jsonl")).unwrap();
    let got: Vec<SourceLine> = lines
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].doc_id.as_str(), got[0].source.as_str()),
        (rows[0].doc_id.as_str(), "partner")
    );
    assert_eq!(
        got[0].url,
        format!("{}/lccn/sn1/1890-05-03/ed-1/seq-1.jp2", partner.root)
    );
}

#[tokio::test]
async fn a_mirrors_pages_dont_end_a_run_of_loc_failures() {
    let dir = tempfile::tempdir().unwrap();
    let s = StandIn::start().await;
    let pages: Vec<(u16, String, Vec<u8>)> = (1..=3)
        .map(|q| (q, format!("{:04}", 10 + q), format!("jp2 {q}").into_bytes()))
        .collect();
    s.add_issue(
        BATCH,
        "sn1",
        "0001A",
        "1890050301",
        &pages
            .iter()
            .map(|(q, f, b)| Page {
                seq: *q,
                frame: f,
                jp2: b,
            })
            .collect::<Vec<_>>(),
    );
    // LoC's JP2s for pages 1 and 3 don't match their bag md5 (each a
    // failure); the partner has page 2.
    for f in ["0011", "0013"] {
        s.put(
            &format!("{BATCH}data/sn1/0001A/1890050301/{f}.jp2"),
            b"damaged".to_vec(),
        );
    }
    let partner = StandIn::start().await;
    partner.put("/lccn/sn1/1890-05-03/ed-1/seq-2.jp2", b"jp2 2".to_vec());
    let rows: Vec<ManifestRow> = (1..=3).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows),
        out: out.clone(),
        concurrency: 1,
        order: Order::Issue,
        max_failures_in_a_row: 2,
        stop_min_titles: 1,
        mirrors: Some(partner_mirrors(&partner)),
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    // Page 2 came from the partner, and the two LoC failures still
    // count as two in a row.
    let err = run(opts, batch_loc(&s, &dir.path().join("batches")), rx)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("2 pages from 1 titles failed in a row"),
        "{err}"
    );
    let st: Status =
        serde_json::from_slice(&std::fs::read(out.join("status-full.json")).unwrap()).unwrap();
    assert_eq!((st.state.as_str(), st.fetched_mirror), ("failed", 1));
}

/// The batch in LoC's bucket.
const IN_BUCKET: &str = "/xx_test_ver01/";

fn bucket_loc(s: &StandIn, b: &StandIn, cache: &Path) -> Arc<LocClient> {
    Arc::new(
        LocClient::new(LocConfig {
            bucket: Some(bucket::BucketConfig {
                root: format!("{}/", b.root),
                gap: Duration::from_millis(10),
                ..bucket::BucketConfig::default()
            }),
            ..batch_cfg(s, cache)
        })
        .unwrap(),
    )
}

/// Two pages of sn1 in a batch at `at`.
fn two_pages(s: &StandIn, at: &str) {
    s.add_issue(
        at,
        "sn1",
        "0001A",
        "1890050301",
        &[
            Page {
                seq: 1,
                frame: "0011",
                jp2: b"jp2 one",
            },
            Page {
                seq: 2,
                frame: "0010",
                jp2: b"jp2 two",
            },
        ],
    );
}

/// The two pages in LoC's bucket, whose manifest carries its md5 as S3
/// does.
fn in_bucket(b: &StandIn) {
    two_pages(b, IN_BUCKET);
    let manifest = format!("{IN_BUCKET}manifest-md5.txt");
    let md5 = batch::md5_hex(&b.file(&manifest).unwrap());
    b.header(&manifest, "x-amz-meta-md5", &md5);
}

fn sources(out: &Path) -> Vec<SourceLine> {
    std::fs::read_to_string(out.join("sources-full.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn locs_bucket_serves_a_public_batch_with_nothing_from_loc() {
    let dir = tempfile::tempdir().unwrap();
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    two_pages(&s, BATCH);
    in_bucket(&b);
    // A partner has page 1 too: the bucket is asked first.
    let partner = StandIn::start().await;
    partner.put("/lccn/sn1/1890-05-03/ed-1/seq-1.jp2", b"jp2 one".to_vec());
    let rows: Vec<ManifestRow> = (1..=2).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows.clone()),
        out: out.clone(),
        concurrency: 1,
        mirrors: Some(partner_mirrors(&partner)),
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let st = run(opts, bucket_loc(&s, &b, &dir.path().join("batches")), rx)
        .await
        .unwrap();
    assert_eq!(
        (
            st.fetched,
            st.fetched_jp2,
            st.fetched_bucket,
            st.fetched_mirror,
            st.fetched_item_api
        ),
        (2, 2, 2, 0, 0)
    );
    assert_eq!((s.hits(""), partner.hits("")), (0, 0), "nothing from LoC");
    let bs = st.bucket.unwrap();
    assert_eq!((bs.manifests, bs.mets, bs.jp2s, bs.requests), (1, 1, 2, 4));
    // The sidecar is LoC's.
    let jp2 = scan_path(&out, &rows[0], ImageSize::Full, ScanFormat::Jp2);
    assert_eq!(std::fs::read(&jp2).unwrap(), b"jp2 one");
    let side: Sidecar =
        serde_json::from_slice(&std::fs::read(sidecar_path(&jp2)).unwrap()).unwrap();
    assert_eq!(
        side.loc_url,
        format!("{}{BATCH}data/sn1/0001A/1890050301/0011.jp2", s.root)
    );
    assert_eq!(
        side.iiif_base,
        format!(
            "{}service:ndnp:xx:batch_xx_test_ver01:data:sn1:0001A:1890050301:0011",
            s.iiif_root()
        )
    );
    assert_eq!(
        (side.md5.as_deref(), side.sha256.as_str()),
        (
            Some(batch::md5_hex(b"jp2 one").as_str()),
            sha256(b"jp2 one").as_str()
        )
    );
    // Where the bytes came from.
    let got = sources(&out);
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|l| l.source == bucket::NAME), "{got:?}");
    let one = got.iter().find(|l| l.doc_id == rows[0].doc_id).unwrap();
    assert_eq!(
        one.url,
        format!("{}{IN_BUCKET}data/sn1/0001A/1890050301/0011.jp2", b.root)
    );
}

#[tokio::test]
async fn a_bucket_jp2_that_isnt_the_bags_goes_to_loc() {
    let dir = tempfile::tempdir().unwrap();
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    two_pages(&s, BATCH);
    in_bucket(&b);
    b.put(
        &format!("{IN_BUCKET}data/sn1/0001A/1890050301/0010.jp2"),
        b"cut short".to_vec(),
    );
    let rows: Vec<ManifestRow> = (1..=2).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows.clone()),
        out: out.clone(),
        concurrency: 1,
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let st = run(opts, bucket_loc(&s, &b, &dir.path().join("batches")), rx)
        .await
        .unwrap();
    assert_eq!((st.fetched, st.fetched_jp2, st.fetched_bucket), (2, 2, 1));
    assert_eq!(st.bucket.unwrap().md5_mismatches, 1);
    // Only page 2's JP2 came from LoC, and it is LoC's.
    assert_eq!((s.hits(""), s.hits("/0010.jp2")), (1, 1));
    let jp2 = scan_path(&out, &rows[1], ImageSize::Full, ScanFormat::Jp2);
    assert_eq!(std::fs::read(&jp2).unwrap(), b"jp2 two");
    let got = sources(&out);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].doc_id, rows[0].doc_id);
}

#[tokio::test]
async fn a_batch_the_bucket_doesnt_hold_still_gets_its_mirrors() {
    let dir = tempfile::tempdir().unwrap();
    // The bucket has nothing of the batch.
    let (s, b) = (StandIn::start().await, StandIn::start().await);
    two_pages(&s, BATCH);
    let partner = StandIn::start().await;
    partner.put("/lccn/sn1/1890-05-03/ed-1/seq-1.jp2", b"jp2 one".to_vec());
    let rows: Vec<ManifestRow> = (1..=2).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
    let out = dir.path().join("scans");
    let opts = Options {
        manifest: write_manifest(dir.path(), rows),
        out: out.clone(),
        concurrency: 1,
        mirrors: Some(partner_mirrors(&partner)),
        ..Options::default()
    };
    let (_tx, rx) = watch::channel(false);
    let st = run(opts, bucket_loc(&s, &b, &dir.path().join("batches")), rx)
        .await
        .unwrap();
    assert_eq!(
        (st.fetched, st.fetched_bucket, st.fetched_mirror),
        (2, 0, 1)
    );
    let bs = st.bucket.unwrap();
    assert_eq!((bs.requests, bs.private_batches), (1, 1));
    assert_eq!(
        (
            s.hits("manifest-md5.txt"),
            s.hits(".xml"),
            s.hits("/0011.jp2"),
            s.hits("/0010.jp2")
        ),
        (1, 1, 0, 1)
    );
    let got = sources(&out);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].source, "partner");
}

#[tokio::test]
async fn a_batch_loc_keeps_elsewhere_has_the_redirects_urls_in_its_sidecars() {
    // LoC keeps the batch under another awardee's directory than its
    // name's first part; Chronicling America's redirect says where.
    const ELSEWHERE: &str = "/storage-services/service/ndnp/yy/batch_xx_test_ver01/";
    const REDIRECT: &str = "/data/batches/xx_test_ver01/manifest-md5.txt";
    let issue = |at: &str, f: &str| format!("{at}data/sn1/0001A/1890050301/{f}");
    // LoC is asked under the guessed URL for the METS (not in the
    // bucket) or for the JP2s (the bucket's aren't the bag's).
    for case in ["mets", "jp2"] {
        let dir = tempfile::tempdir().unwrap();
        let (s, b) = (StandIn::start().await, StandIn::start().await);
        two_pages(&s, ELSEWHERE);
        s.redirect(REDIRECT, &format!("{ELSEWHERE}manifest-md5.txt"));
        in_bucket(&b);
        if case == "mets" {
            b.script(&issue(IN_BUCKET, "1890050301.xml"), Scripted::Status(404));
        } else {
            for f in ["0010.jp2", "0011.jp2"] {
                b.put(&issue(IN_BUCKET, f), b"cut short".to_vec());
            }
        }
        let rows: Vec<ManifestRow> = (1..=2).map(|q| batch_row("sn1", "1890-05-03", q)).collect();
        let out = dir.path().join("scans");
        let opts = Options {
            manifest: write_manifest(dir.path(), rows.clone()),
            out: out.clone(),
            concurrency: 1,
            ..Options::default()
        };
        let (_tx, rx) = watch::channel(false);
        let cache = dir.path().join("batches");
        let st = run(opts, bucket_loc(&s, &b, &cache), rx).await.unwrap();
        let from_bucket = if case == "mets" { 2 } else { 0 };
        assert_eq!(
            (
                st.fetched,
                st.fetched_jp2,
                st.fetched_bucket,
                st.fetched_item_api
            ),
            (2, 2, from_bucket, 0),
            "{case}"
        );
        // Every sidecar names the batch where it is.
        for (row, frame) in rows.iter().zip(["0011", "0010"]) {
            let jp2 = scan_path(&out, row, ImageSize::Full, ScanFormat::Jp2);
            let side: Sidecar =
                serde_json::from_slice(&std::fs::read(sidecar_path(&jp2)).unwrap()).unwrap();
            assert_eq!(
                side.loc_url,
                format!("{}{}", s.root, issue(ELSEWHERE, &format!("{frame}.jp2"))),
                "{case}"
            );
            assert_eq!(
                side.iiif_base,
                format!(
                    "{}service:ndnp:yy:batch_xx_test_ver01:data:sn1:0001A:1890050301:{frame}",
                    s.iiif_root()
                ),
                "{case}"
            );
        }
        // One lookup; the manifest never from LoC; the checked URL kept.
        assert_eq!(
            (
                s.hits(REDIRECT),
                s.hits(&format!("{ELSEWHERE}manifest-md5.txt")),
                s.hits(&format!("{BATCH}manifest-md5.txt"))
            ),
            (1, 0, 0),
            "{case}"
        );
        assert_eq!(
            std::fs::read_to_string(cache.join("xx_test_ver01/url")).unwrap(),
            format!("{}{ELSEWHERE}", s.root),
            "{case}"
        );
        assert!(!cache.join("xx_test_ver01/url.guessed").exists(), "{case}");
    }
}
