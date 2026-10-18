use super::*;
use axum::http::Request;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tower::ServiceExt;

const BLOB: &str = "keep/batch/sn123/1900-01-01/ed-1/seq-1.jp2";
const DATA: &[u8] = b"scan-image";

struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn unix_seconds(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Good,
    Missing,
    Failed,
    WrongMetadata,
    Corrupt,
    Short,
    Long,
    Changed,
    Slow,
}
struct Reader {
    mode: Mode,
    properties_calls: AtomicUsize,
    reads: Mutex<Vec<(String, ByteRange, String)>>,
}

#[async_trait]
impl ArchiveReader for Reader {
    async fn properties(&self, blob: &str) -> Result<Properties, ArchiveError> {
        assert_eq!(blob, BLOB);
        self.properties_calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            Mode::Missing => return Err(ArchiveError::NotFound),
            Mode::Failed => return Err(ArchiveError::Unavailable),
            _ => (),
        }
        let scan = scan();
        Ok(Properties {
            bytes: scan.bytes,
            sha256: if matches!(self.mode, Mode::WrongMetadata) {
                "bad".into()
            } else {
                scan.sha256
            },
            md5: scan.md5,
            etag: "\"immutable-v1\"".into(),
        })
    }
    async fn read(
        &self,
        blob: &str,
        range: ByteRange,
        etag: &str,
    ) -> Result<ByteStream, ArchiveError> {
        self.reads
            .lock()
            .unwrap()
            .push((blob.into(), range, etag.into()));
        if matches!(self.mode, Mode::Changed) {
            return Err(ArchiveError::Unavailable);
        }
        if matches!(self.mode, Mode::Slow) {
            return Ok(Box::pin(futures::stream::pending()));
        }
        let mut data = DATA[range.start as usize..=range.end as usize].to_vec();
        match self.mode {
            Mode::Corrupt => data[0] ^= 1,
            Mode::Short => {
                data.pop();
            }
            Mode::Long => data.push(0),
            _ => (),
        }
        let chunks = data
            .chunks(3)
            .map(Bytes::copy_from_slice)
            .map(Ok)
            .collect::<Vec<_>>();
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[42; 32])
}
fn scan() -> Scan {
    Scan {
        bytes: DATA.len() as u64,
        md5: STANDARD.encode(Md5::digest(DATA)),
        sha256: hex(&Sha256::digest(DATA)),
    }
}
fn ticket() -> Ticket {
    Ticket {
        app_name: "ocr".into(),
        app_version: "1".into(),
        blobs: BTreeMap::from([(BLOB.into(), scan())]),
        expires_at: 2000,
        issued_at: 900,
        key_id: "test-v1".into(),
        ticket_id: "ticket-1".into(),
        workunit: "wu-1".into(),
    }
}
fn sign_payload(payload: &[u8]) -> String {
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(payload),
        URL_SAFE_NO_PAD.encode(key().sign(payload).to_bytes())
    )
}
fn token(t: &Ticket) -> String {
    sign_payload(&t.canonical_json().unwrap())
}
fn setup(mode: Mode, limits: Limits) -> (Arc<Service>, Arc<Reader>, Arc<TestClock>) {
    let reader = Arc::new(Reader {
        mode,
        properties_calls: AtomicUsize::new(0),
        reads: Mutex::default(),
    });
    let clock = Arc::new(TestClock(AtomicU64::new(1000)));
    let service = Arc::new(
        Service::new(
            reader.clone(),
            BTreeMap::from([("test-v1".into(), key().verifying_key())]),
            limits,
            clock.clone(),
        )
        .unwrap(),
    );
    (service, reader, clock)
}
fn request(
    auth: &str,
    blob: &str,
    workunit: &str,
    extra: &[(&str, &str)],
    ip: &str,
) -> Request<Body> {
    let mut request = Request::builder()
        .uri(format!("/v1/scans/{blob}"))
        .header(header::AUTHORIZATION, format!("Bearer {auth}"))
        .header("x-usnm-workunit", workunit);
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let mut request = request.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
    request
}
async fn send(service: &Arc<Service>, extra: &[(&str, &str)]) -> Response {
    service
        .clone()
        .router()
        .oneshot(request(
            &token(&ticket()),
            BLOB,
            "wu-1",
            extra,
            "127.0.0.1:1234",
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn streams_full_scan_with_integrity_headers_and_conditional_private_read() {
    let (s, r, _) = setup(Mode::Good, Limits::default());
    let response = send(&s, &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        DATA.len().to_string()
    );
    assert_eq!(response.headers()["content-md5"], scan().md5);
    assert_eq!(response.headers()[header::ETAG], "\"immutable-v1\"");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/jp2");
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        DATA
    );
    assert_eq!(
        *r.reads.lock().unwrap(),
        vec![(
            BLOB.into(),
            ByteRange { start: 0, end: 9 },
            "\"immutable-v1\"".into()
        )]
    );
}

#[tokio::test]
async fn authentication_allowlist_workunit_and_expiry_fail_before_archive_io() {
    let (s, r, clock) = setup(Mode::Good, Limits::default());
    let mut cases: Vec<(String, String, String, StatusCode)> = vec![(
        "".into(),
        BLOB.to_owned(),
        "wu-1".into(),
        StatusCode::UNAUTHORIZED,
    )];
    let mut bad_signature = token(&ticket());
    let signature_start = bad_signature.find('.').unwrap() + 1;
    bad_signature.replace_range(signature_start..signature_start + 1, "A");
    cases.push((
        bad_signature,
        BLOB.into(),
        "wu-1".into(),
        StatusCode::UNAUTHORIZED,
    ));
    for field in [
        "key", "expired", "future", "long", "empty", "path", "md5", "sha", "size", "app", "pages",
    ] {
        let mut t = ticket();
        match field {
            "key" => t.key_id = "unknown".into(),
            "expired" => t.expires_at = 1000,
            "future" => t.issued_at = 1001,
            "long" => t.expires_at = 1000000,
            "empty" => t.blobs.clear(),
            "path" => {
                let scan = t.blobs.remove(BLOB).unwrap();
                t.blobs.insert("keep/../escape.jpg".into(), scan);
            }
            "md5" => t.blobs.get_mut(BLOB).unwrap().md5 = "bad".into(),
            "sha" => t.blobs.get_mut(BLOB).unwrap().sha256 = "bad".into(),
            "size" => t.blobs.get_mut(BLOB).unwrap().bytes = 0,
            "app" => t.app_version.clear(),
            "pages" => {
                for n in 0..33 {
                    t.blobs.insert(format!("keep/b/sn/1900/ed/{n}.jp2"), scan());
                }
            }
            _ => unreachable!(),
        }
        cases.push((
            token(&t),
            BLOB.into(),
            "wu-1".into(),
            StatusCode::UNAUTHORIZED,
        ));
    }
    cases.push((
        token(&ticket()),
        BLOB.into(),
        "different-workunit".into(),
        StatusCode::FORBIDDEN,
    ));
    cases.push((
        token(&ticket()),
        BLOB.replace("seq-1", "seq-2"),
        "wu-1".into(),
        StatusCode::FORBIDDEN,
    ));
    for (auth, blob, workunit, expected) in cases {
        let response = s
            .clone()
            .router()
            .oneshot(request(&auth, &blob, &workunit, &[], "127.0.0.1:1"))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{workunit} {blob}");
    }
    clock.0.store(2000, Ordering::SeqCst);
    assert_eq!(send(&s, &[]).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejects_noncanonical_unknown_fields_and_oversized_tickets() {
    let (s, r, _) = setup(Mode::Good, Limits::default());
    let canonical = ticket().canonical_json().unwrap();
    let text = String::from_utf8(canonical.clone()).unwrap();
    let bad = [
        format!(" {text}"),
        text.replacen('{', "{\"unknown\":true,", 1),
        text.replacen('{', "{\"app_name\":\"ocr\",", 1),
    ];
    for payload in bad {
        let response = s
            .clone()
            .router()
            .oneshot(request(
                &sign_payload(payload.as_bytes()),
                BLOB,
                "wu-1",
                &[],
                "127.0.0.1:1",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = s
        .clone()
        .router()
        .oneshot(request(
            &"a".repeat(16385),
            BLOB,
            "wu-1",
            &[],
            "127.0.0.1:1",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 0);
    // This independent expected JSON proves field/key order, rather than round-tripping
    // through the serializer used by the verifier.
    assert_eq!(String::from_utf8(canonical).unwrap(), format!("{{\"app_name\":\"ocr\",\"app_version\":\"1\",\"blobs\":{{\"{BLOB}\":{{\"bytes\":10,\"md5\":\"{}\",\"sha256\":\"{}\"}}}},\"expires_at\":2000,\"issued_at\":900,\"key_id\":\"test-v1\",\"ticket_id\":\"ticket-1\",\"workunit\":\"wu-1\"}}", scan().md5, scan().sha256));
}

#[tokio::test]
async fn single_ranges_and_if_range_behavior() {
    for (header, expected_range, expected_data) in [
        ("bytes=1-3", "bytes 1-3/10", &DATA[1..4]),
        ("bytes=7-", "bytes 7-9/10", &DATA[7..]),
        ("bytes=-2", "bytes 8-9/10", &DATA[8..]),
        ("bytes=8-100", "bytes 8-9/10", &DATA[8..]),
    ] {
        let (s, _, _) = setup(Mode::Good, Limits::default());
        let response = send(&s, &[("range", header)]).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], expected_range);
        assert!(!response.headers().contains_key("content-md5"));
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            expected_data
        );
    }
    for (validator, status, bytes) in [
        ("\"immutable-v1\"", StatusCode::PARTIAL_CONTENT, &DATA[1..4]),
        ("\"old\"", StatusCode::OK, DATA),
    ] {
        let (s, _, _) = setup(Mode::Good, Limits::default());
        let response = send(&s, &[("range", "bytes=1-3"), ("if-range", validator)]).await;
        assert_eq!(response.status(), status);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            bytes
        );
    }
}

#[tokio::test]
async fn malformed_ranges_consume_requests_but_never_read_archive() {
    for value in [
        "bytes=",
        "bytes=10-",
        "bytes=4-2",
        "bytes=-0",
        "bytes=1-2,5-6",
        "items=0-1",
        "bytes=+1-3",
        "bytes=0-18446744073709551616",
    ] {
        let (s, r, _) = setup(Mode::Good, Limits::default());
        let response = send(&s, &[("range", value)]).await;
        assert_eq!(
            response.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "{value}"
        );
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
        assert_eq!(r.properties_calls.load(Ordering::SeqCst), 0);
    }
    let (s, _, _) = setup(Mode::Good, Limits::default());
    for _ in 0..3 {
        assert_eq!(
            send(&s, &[("range", "bad")]).await.status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
    }
    assert_eq!(send(&s, &[]).await.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn ticket_request_limit_is_shared_across_ips_and_ranges() {
    let (s, r, _) = setup(Mode::Good, Limits::default());
    for n in 1..=3 {
        let response = s
            .clone()
            .router()
            .oneshot(request(
                &token(&ticket()),
                BLOB,
                "wu-1",
                &[("range", "bytes=0-0")],
                &format!("127.0.0.{n}:1"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    }
    let response = send(&s, &[]).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "1000");
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn ip_request_and_byte_budgets_ignore_spoofed_forwarding_headers() {
    for limits in [
        Limits {
            ip_requests_per_minute: 1,
            ..Limits::default()
        },
        Limits {
            ip_bytes_per_minute: 10,
            ..Limits::default()
        },
    ] {
        let (s, r, clock) = setup(Mode::Good, limits);
        assert_eq!(send(&s, &[]).await.status(), StatusCode::OK);
        let mut different = ticket();
        different.ticket_id = "other".into();
        let response = s
            .clone()
            .router()
            .oneshot(request(
                &token(&different),
                BLOB,
                "wu-1",
                &[("x-forwarded-for", "198.51.100.9")],
                "127.0.0.1:3456",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "20");
        assert_eq!(r.properties_calls.load(Ordering::SeqCst), 1);
        clock.0.store(1020, Ordering::SeqCst);
        assert_eq!(send(&s, &[]).await.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn daily_budget_is_per_service_and_resets_only_on_next_day() {
    let limits = Limits {
        replica_bytes_per_day: 10,
        ..Limits::default()
    };
    let (s, r, clock) = setup(Mode::Good, limits.clone());
    assert_eq!(send(&s, &[]).await.status(), StatusCode::OK);
    let response = send(&s, &[]).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "85400");
    clock.0.store(950, Ordering::SeqCst);
    assert_eq!(send(&s, &[]).await.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 1);
    // A fresh service really resets the brake: this is deliberately NOT a cost cap.
    let (replica, _, _) = setup(Mode::Good, limits);
    assert_eq!(send(&replica, &[]).await.status(), StatusCode::OK);
    clock.0.store(86401, Ordering::SeqCst);
    let mut new = ticket();
    new.issued_at = 86400;
    new.expires_at = 87000;
    let response = s
        .clone()
        .router()
        .oneshot(request(&token(&new), BLOB, "wu-1", &[], "127.0.0.1:1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn capacity_and_atomic_concurrent_reservations_are_bounded() {
    for limits in [
        Limits {
            max_live_ips: 1,
            ..Limits::default()
        },
        Limits {
            max_live_tickets: 1,
            ..Limits::default()
        },
    ] {
        let (s, _, _) = setup(Mode::Good, limits);
        assert_eq!(send(&s, &[]).await.status(), StatusCode::OK);
        let mut other = ticket();
        other.ticket_id = "other".into();
        let response = s
            .clone()
            .router()
            .oneshot(request(&token(&other), BLOB, "wu-1", &[], "127.0.0.2:1"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    let (s, r, _) = setup(Mode::Good, Limits::default());
    let responses = futures::future::join_all((0..12).map(|_| send(&s, &[]))).await;
    assert_eq!(
        responses
            .iter()
            .filter(|r| r.status() == StatusCode::OK)
            .count(),
        3
    );
    assert_eq!(
        responses
            .iter()
            .filter(|r| r.status() == StatusCode::TOO_MANY_REQUESTS)
            .count(),
        9
    );
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn misses_errors_and_metadata_mismatch_never_fall_back_or_refund() {
    for (mode, expected, reads) in [
        (Mode::Missing, StatusCode::NOT_FOUND, 0),
        (Mode::Failed, StatusCode::BAD_GATEWAY, 0),
        (Mode::WrongMetadata, StatusCode::BAD_GATEWAY, 0),
        (Mode::Changed, StatusCode::BAD_GATEWAY, 3),
    ] {
        let (s, r, _) = setup(mode, Limits::default());
        for _ in 0..3 {
            assert_eq!(send(&s, &[]).await.status(), expected);
        }
        assert_eq!(send(&s, &[]).await.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.properties_calls.load(Ordering::SeqCst), 3);
        assert_eq!(r.reads.lock().unwrap().len(), reads);
    }
}

#[tokio::test]
async fn corrupt_truncated_and_extra_bodies_fail_stream_integrity() {
    for mode in [Mode::Corrupt, Mode::Short, Mode::Long] {
        let (s, _, _) = setup(mode, Limits::default());
        let response = send(&s, &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.into_body().collect().await.is_err());
    }
    // Ranges use the immutable ETag + metadata and exact length. They must be checked
    // against the job's whole-file hashes by the client after reassembly.
    let (s, _, _) = setup(Mode::Short, Limits::default());
    assert!(send(&s, &[("range", "bytes=0-4")])
        .await
        .into_body()
        .collect()
        .await
        .is_err());
}

#[tokio::test]
async fn stalled_stream_times_out_and_head_never_reads() {
    let (s, r, _) = setup(
        Mode::Slow,
        Limits {
            archive_timeout_seconds: 1,
            ..Limits::default()
        },
    );
    let mut req = request(&token(&ticket()), BLOB, "wu-1", &[], "127.0.0.1:1");
    *req.method_mut() = Method::HEAD;
    assert_eq!(
        s.clone().router().oneshot(req).await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 0);
    let response = send(&s, &[]).await;
    assert!(response.into_body().collect().await.is_err());
}

#[tokio::test]
async fn if_range_full_body_reservation_cannot_bypass_byte_budget() {
    let (s, r, _) = setup(
        Mode::Good,
        Limits {
            replica_bytes_per_day: 4,
            ..Limits::default()
        },
    );
    let response = send(&s, &[("range", "bytes=0-2"), ("if-range", "\"other\"")]).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ticket_byte_bound_is_independent_of_request_bound() {
    let (s, r, _) = setup(
        Mode::Good,
        Limits {
            retry_multiple: 1,
            ..Limits::default()
        },
    );
    let mut t = ticket();
    let mut small = scan();
    small.bytes = 1;
    t.blobs.insert(BLOB.replace("seq-1", "seq-2"), small);
    let auth = token(&t);
    for expected in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
        let response = s
            .clone()
            .router()
            .oneshot(request(&auth, BLOB, "wu-1", &[], "127.0.0.1:1"))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn duplicate_range_headers_fail_closed_and_key_revocation_is_immediate() {
    let (s, r, _) = setup(Mode::Good, Limits::default());
    let response = send(&s, &[("range", "bytes=0-1"), ("range", "bytes=4-5")]).await;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(r.properties_calls.load(Ordering::SeqCst), 0);
    let (mut replacement, _, _) = setup(Mode::Good, Limits::default());
    Arc::get_mut(&mut replacement).unwrap().keys =
        BTreeMap::from([("new-key-id".into(), key().verifying_key())]);
    assert_eq!(
        send(&replacement, &[]).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn unsatisfiable_range_obeys_if_range_before_deciding_416() {
    for (validator, status) in [
        ("\"old\"", StatusCode::OK),
        ("\"immutable-v1\"", StatusCode::RANGE_NOT_SATISFIABLE),
    ] {
        let (s, r, _) = setup(Mode::Good, Limits::default());
        let response = send(&s, &[("range", "bytes=10-"), ("if-range", validator)]).await;
        assert_eq!(response.status(), status);
        if status == StatusCode::OK {
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                DATA
            );
            assert_eq!(r.reads.lock().unwrap().len(), 1);
        } else {
            assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
            assert!(r.reads.lock().unwrap().is_empty());
        }
    }
    let (s, r, _) = setup(
        Mode::Good,
        Limits {
            replica_bytes_per_day: 9,
            ..Limits::default()
        },
    );
    assert_eq!(
        send(&s, &[("range", "bytes=10-"), ("if-range", "\"old\"")])
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert!(r.reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn expiry_during_pending_properties_cannot_recreate_pruned_ticket_allowance() {
    use futures::FutureExt;
    struct PausingReader {
        reader: Arc<Reader>,
        started: futures::channel::mpsc::UnboundedSender<()>,
        release: futures::future::Shared<futures::future::BoxFuture<'static, ()>>,
    }
    #[async_trait]
    impl ArchiveReader for PausingReader {
        async fn properties(&self, blob: &str) -> Result<Properties, ArchiveError> {
            self.started.unbounded_send(()).unwrap();
            self.release.clone().await;
            self.reader.properties(blob).await
        }
        async fn read(
            &self,
            blob: &str,
            range: ByteRange,
            etag: &str,
        ) -> Result<ByteStream, ArchiveError> {
            self.reader.read(blob, range, etag).await
        }
    }
    let (_, reader, clock) = setup(Mode::Good, Limits::default());
    let (started_tx, mut started_rx) = futures::channel::mpsc::unbounded();
    let (release_tx, release_rx) = futures::channel::oneshot::channel();
    let backend = Arc::new(PausingReader {
        reader: reader.clone(),
        started: started_tx,
        release: release_rx.map(|_| ()).boxed().shared(),
    });
    let service = Arc::new(
        Service::new(
            backend,
            BTreeMap::from([("test-v1".into(), key().verifying_key())]),
            Limits::default(),
            clock.clone(),
        )
        .unwrap(),
    );
    let mut t = ticket();
    let mut small = scan();
    small.bytes = 1;
    t.blobs.insert(BLOB.replace("seq-1", "seq-2"), small);
    let id: [u8; 32] = Sha256::digest(t.canonical_json().unwrap()).into();
    let mut pending = Vec::new();
    for _ in 0..6 {
        let req = request(
            &token(&t),
            BLOB,
            "wu-1",
            &[("range", "bytes=0-0"), ("if-range", "\"old\"")],
            "127.0.0.1:1",
        );
        let router = service.clone().router();
        pending.push(tokio::spawn(
            async move { router.oneshot(req).await.unwrap() },
        ));
    }
    for _ in 0..6 {
        started_rx.next().await.unwrap();
    }
    assert_eq!(service.budgets.lock().unwrap().tickets[&id].bytes, 6);
    clock.0.store(t.expires_at, Ordering::SeqCst);
    // A different, live admission prunes the old ticket exactly as a concurrent
    // request would, while all six old requests still await properties.
    let mut live = ticket();
    live.issued_at = t.expires_at;
    live.expires_at = t.expires_at + 100;
    let live_id = Sha256::digest(live.canonical_json().unwrap()).into();
    service
        .reserve(&live, live_id, "127.0.0.2".parse().unwrap(), 0, true)
        .unwrap();
    assert!(!service.budgets.lock().unwrap().tickets.contains_key(&id));
    release_tx.send(()).unwrap();
    for request in pending {
        assert_eq!(request.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    }
    assert!(!service.budgets.lock().unwrap().tickets.contains_key(&id));
    assert!(reader.reads.lock().unwrap().is_empty());
}
