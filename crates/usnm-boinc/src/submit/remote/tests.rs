use super::*;
use crate::{
    atomic, load, save,
    submit::{reconcile, submit, submit_with_download},
    Record, Stage,
};
use axum::{
    body::Body,
    http::{Response, StatusCode},
    routing::get,
    Router,
};
use std::os::unix::fs::{symlink, PermissionsExt};
use usnm_ocr_contract::boinc::{self, Engine, Job, Scan};

const PAGE: &[u8] = b"immutable newspaper page";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    mirror: PathBuf,
    id: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let root = path.join("dispatch");
        let project = path.join("project");
        let mirror = path.join("mirror");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        std::fs::create_dir(&mirror).unwrap();
        std::fs::set_permissions(&mirror, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut job = Job {
            v: boinc::VERSION,
            id: String::new(),
            message: serde_json::from_value(serde_json::json!({
                "v": 1, "page": "sn00000001/1900-01-01/ed-1/seq-1",
                "batch": "test_pilot", "language": "eng", "reason": "bulk",
                "queued_at": "2026-10-17T00:00:00Z"
            }))
            .unwrap(),
            batch_version: 1,
            scan: Scan {
                url: "https://tile.loc.gov/test".into(),
                iiif_base: "https://tile.loc.gov/test".into(),
                sha256: boinc::sha256(PAGE),
                bytes: PAGE.len() as u64,
                md5: None,
            },
            engine: Engine {
                kind: Default::default(),
                image: format!("sha256:{}", "a".repeat(64)),
                version: "5.5.0".into(),
                profile: "eng".into(),
                models: "sha256:aaaaaaaaaaaa".into(),
                psm: 3,
                dpi: 300,
                timeout_secs: 60,
            },
        };
        job.id = job.identity().unwrap();
        job.validate().unwrap();
        atomic(
            &root.join("jobs").join(&job.id).join("request.json"),
            &serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        atomic(&root.join("jobs").join(&job.id).join("page"), PAGE).unwrap();
        let id = job.id.clone();
        save(
            &root,
            &Record {
                job,
                plan: "test".into(),
                stage: Stage::Reserved,
                stamp: "test".into(),
                error: None,
            },
        )
        .unwrap();
        executable(
            &project.join("bin/dir_hier_path"),
            "#!/bin/sh\nprintf 'download/%s\\n' \"$1\"\n",
        );
        Self {
            _dir: dir,
            root,
            project,
            mirror,
            id,
        }
    }

    fn tool(&self, remote: bool, fail: bool) {
        executable(
            &self.project.join("bin/create_work"),
            &format!(
                r#"#!/usr/bin/env python3
import hashlib, json, pathlib, sys
root = pathlib.Path({root})
project = pathlib.Path({project})
mirror = pathlib.Path({mirror})
record = json.loads((root / "jobs" / "{id}" / "record.json").read_text())
assert record["stage"] == "submitting"
args = sys.argv[1:]
if {remote}:
    assert args[-5] == "--remote_file"
    assert args[-4].endswith("/" + record["job"]["scan"]["sha256"])
    assert args[-3] == str({size})
    assert args[-2] == hashlib.md5({page}).hexdigest()
    assert not (project / "download" / "{id}_page").exists()
    assert (mirror / record["job"]["scan"]["sha256"]).read_bytes() == {page}
else:
    assert "--remote_file" not in args
    assert args[-2] == "{id}_page"
    assert (project / "download" / "{id}_page").read_bytes() == {page}
assert args[-1] == "{id}_request_json"
assert (project / "download" / args[-1]).read_bytes() == (root / "jobs" / "{id}" / "request.json").read_bytes()
(project / "called").write_text("called")
sys.exit({fail})
"#,
                root = serde_json::to_string(&self.root).unwrap(),
                project = serde_json::to_string(&self.project).unwrap(),
                mirror = serde_json::to_string(&self.mirror).unwrap(),
                id = self.id,
                remote = if remote { "True" } else { "False" },
                size = PAGE.len(),
                page = format!("b{:?}", std::str::from_utf8(PAGE).unwrap()),
                fail = i32::from(fail),
            ),
        );
    }
}

fn executable(path: &Path, bytes: &str) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

async fn server(status: StatusCode, bytes: Vec<u8>) -> (Url, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/{*path}",
        get(move || {
            let bytes = bytes.clone();
            async move {
                Response::builder()
                    .status(status)
                    .body(Body::from(bytes))
                    .unwrap()
            }
        }),
    );
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/pages/", socket.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    (url, task)
}

fn test_download(fx: &Fixture, base: Url) -> Download {
    // Private unit-test construction only: the public API always enforces HTTPS,
    // public pinned DNS and normal certificate validation.
    Download {
        base,
        root: Some(fx.mirror.clone()),
        test_client: Some(
            Client::builder()
                .no_proxy()
                .pool_max_idle_per_host(0)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
        ),
    }
}

#[tokio::test]
async fn default_and_remote_order_readiness_and_replay() {
    for remote in [false, true] {
        let fx = Fixture::new();
        fx.tool(remote, false);
        let (url, task) = server(StatusCode::OK, PAGE.to_vec()).await;
        let download = test_download(&fx, url);
        submit_with_download(&fx.root, &fx.project, &fx.id, remote.then_some(&download))
            .await
            .unwrap();
        assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Submitted);
        assert!(
            submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download))
                .await
                .is_err()
        );
        assert!(fx.project.join("called").exists());
        task.abort();
    }
}

#[tokio::test]
async fn separately_published_origin_needs_no_local_mount() {
    let fx = Fixture::new();
    fx.tool(true, false);
    // Producer publishes independently; the Azure submitter has no origin path.
    let receipt =
        prepare_download(&fx.root.join("jobs").join(&fx.id).join("page"), &fx.mirror).unwrap();
    assert_eq!(receipt.sha256, boinc::sha256(PAGE));
    assert_eq!(receipt.bytes, PAGE.len() as u64);
    assert_eq!(receipt.md5, hex(&Md5::digest(PAGE)));
    let (url, task) = server(StatusCode::OK, PAGE.to_vec()).await;
    let mut download = test_download(&fx, url);
    download.root = None;
    submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download))
        .await
        .unwrap();
    assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Submitted);
    task.abort();
}

#[tokio::test]
async fn remote_failures_leave_reserved_and_never_dispatch() {
    for (status, bytes) in [
        (StatusCode::NOT_FOUND, vec![]),
        (StatusCode::FOUND, PAGE.to_vec()),
        (StatusCode::OK, vec![b'x'; PAGE.len()]),
        (StatusCode::OK, PAGE[..PAGE.len() - 1].to_vec()),
        (StatusCode::OK, vec![b'x'; PAGE.len() + 1]),
    ] {
        let fx = Fixture::new();
        fx.tool(true, false);
        let (url, task) = server(status, bytes).await;
        let download = test_download(&fx, url);
        assert!(
            submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download))
                .await
                .is_err()
        );
        assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Reserved);
        assert!(!fx.project.join("called").exists());
        assert!(!fx.project.join("download").exists());
        task.abort();
    }
    let fx = Fixture::new();
    let (url, task) = server(StatusCode::OK, PAGE.to_vec()).await;
    task.abort();
    let _ = task.await;
    let download = test_download(&fx, url);
    assert!(
        submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download))
            .await
            .is_err()
    );
    assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Reserved);
}

#[tokio::test]
async fn mirror_uncertainty_requires_reconcile_then_fresh_probe() {
    let fx = Fixture::new();
    fx.tool(true, true);
    let (url, task) = server(StatusCode::OK, PAGE.to_vec()).await;
    let download = test_download(&fx, url);
    assert!(
        submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download))
            .await
            .is_err()
    );
    assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Submitting);
    assert!(submit(&fx.root, &fx.project, &fx.id).await.is_err());
    assert!(
        submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download))
            .await
            .is_err()
    );
    reconcile(&fx.root, &fx.id, false).unwrap();
    fx.tool(true, false);
    // Identical immutable input is reused, but HTTP is still required.
    task.abort();
    let _ = task.await;
    assert!(
        submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download))
            .await
            .is_err()
    );
    assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Reserved);
    let (url, task) = server(StatusCode::OK, PAGE.to_vec()).await;
    let download = test_download(&fx, url);
    // Parallel tests spawn processes on Linux. An unrelated pre-exec child can
    // briefly inherit a flock even after its parent's descriptor was closed.
    // Retry only failure to acquire the lock, never an attempted submission.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match submit_with_download(&fx.root, &fx.project, &fx.id, Some(&download)).await {
            Ok(()) => break,
            Err(error)
                if error.to_string().contains("another BOINC bridge process")
                    && tokio::time::Instant::now() < deadline =>
            {
                assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Reserved);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!("{error:#}"),
        }
    }
    task.abort();
}

#[tokio::test]
async fn changed_staged_scan_prevents_publication() {
    let fx = Fixture::new();
    atomic(&fx.root.join("jobs").join(&fx.id).join("page"), b"changed").unwrap();
    let (url, task) = server(StatusCode::OK, PAGE.to_vec()).await;
    assert!(submit_with_download(
        &fx.root,
        &fx.project,
        &fx.id,
        Some(&test_download(&fx, url))
    )
    .await
    .is_err());
    assert_eq!(std::fs::read_dir(&fx.mirror).unwrap().count(), 0);
    assert_eq!(load(&fx.root, &fx.id).unwrap().stage, Stage::Reserved);
    task.abort();
}

#[test]
fn publication_is_create_only_readonly_and_rejects_links_conflicts() {
    let fx = Fixture::new();
    let sha = boinc::sha256(PAGE);
    publish(&fx.mirror, &sha, PAGE).unwrap();
    publish(&fx.mirror, &sha, PAGE).unwrap();
    assert_eq!(
        std::fs::metadata(fx.mirror.join(&sha))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
    assert!(publish(&fx.mirror, &sha, b"conflict").is_err());
    assert_eq!(std::fs::read(fx.mirror.join(&sha)).unwrap(), PAGE);
    std::fs::remove_file(fx.mirror.join(&sha)).unwrap();
    let victim = fx.mirror.join("victim");
    std::fs::write(&victim, PAGE).unwrap();
    symlink(&victim, fx.mirror.join(&sha)).unwrap();
    assert!(publish(&fx.mirror, &sha, PAGE).is_err());
    let link = fx.mirror.with_file_name("linked-root");
    symlink(&fx.mirror, &link).unwrap();
    assert!(Download::new("https://downloads.example.org/pages", &link).is_err());
    std::fs::create_dir(fx.mirror.join("nested")).unwrap();
    assert!(Download::new("https://downloads.example.org/pages", &link.join("nested")).is_err());
    assert!(Download::new("https://downloads.example.org/pages", Path::new("relative")).is_err());
    assert!(Download::new(
        "https://downloads.example.org/pages",
        &fx.mirror.join("../mirror")
    )
    .is_err());
    std::fs::set_permissions(&fx.mirror, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(Download::new("https://downloads.example.org/pages", &fx.mirror).is_err());
}

#[tokio::test]
async fn urls_and_nonpublic_addresses_are_rejected() {
    let fx = Fixture::new();
    for url in [
        "http://downloads.example.org/pages",
        "https://user@downloads.example.org/",
        "https://downloads.example.org/pages?x=1",
        "https://downloads.example.org/#x",
        "https://downloads.example.org/a/../b",
        "https://downloads.example.org/a/./b",
        "https://downloads.example.org/%2e%2e/",
        "https://downloads.example.org/a//b",
        "https://downloads.example.org/a\\b",
        "https://downloads.example.org/a&b",
    ] {
        assert!(Download::new(url, &fx.mirror).is_err(), "{url}");
    }

    for base in [
        "https://downloads.example.org/pages",
        "https://downloads.example.org/pages/",
    ] {
        let download = Download::new(base, &fx.mirror).unwrap();
        assert_eq!(
            download.base.join(&boinc::sha256(PAGE)).unwrap().path(),
            format!("/pages/{}", boinc::sha256(PAGE))
        );
    }
    for ip in [
        "127.0.0.1",
        "169.254.169.254",
        "100.64.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "224.0.0.1",
        "::1",
        "::ffff:8.8.8.8",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "2002:0808:0808::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["8.8.8.8", "2606:4700:4700::1111"] {
        assert!(public_ip(ip.parse().unwrap()), "{ip}");
    }
    let private_ip = IpAddr::from([10, 0, 0, 1]);
    assert!(!public_ip(private_ip));
    let private_url = format!("https://{private_ip}/page");
    for url in ["https://127.0.0.1/page", "https://[::1]/page", &private_url] {
        assert!(public_client(&Url::parse(url).unwrap()).await.is_err());
    }
}

#[tokio::test]
async fn chunked_downloads_are_bounded_and_stalls_time_out() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (body, stall, success) in [
        (PAGE.to_vec(), false, true),
        (vec![b'x'; PAGE.len() + 1], false, false),
        (PAGE.to_vec(), true, false),
    ] {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/page", socket.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = socket.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            if stall {
                std::future::pending::<()>().await;
            }
            stream
                .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
            stream.write_all(b"\r\n0\r\n\r\n").await.unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(300))
            .build()
            .unwrap();
        let result = probe(&client, url, PAGE.len(), &boinc::sha256(PAGE)).await;
        assert_eq!(result.is_ok(), success, "{result:?}");
        task.abort();
    }
}

#[test]
fn stalled_dns_process_child() {
    if std::env::var_os("USNM_TEST_STALLED_DNS_CHILD").is_none() {
        return;
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_started = started.clone();
    let result = runtime.block_on(resolve(
        move || {
            worker_started.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(Duration::from_secs(60));
            Ok(vec![])
        },
        Duration::from_secs(1),
    ));
    assert!(result.is_err());
    assert!(started.load(std::sync::atomic::Ordering::SeqCst));
    // This drop would wait sixty seconds with tokio::net::lookup_host's
    // blocking-pool resolution. The isolated worker must not delay process exit.
    drop(runtime);
}

#[tokio::test]
async fn stalled_dns_deadline_bounds_process_exit() {
    let child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "submit::remote::tests::stalled_dns_process_child",
            "--nocapture",
        ])
        .env("USNM_TEST_STALLED_DNS_CHILD", "1")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("stalled DNS prevented process exit")
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn interrupted_publication_scratch_names_are_skipped_without_removal() {
    let fx = Fixture::new();
    let stale = format!(".publish-{}-0", std::process::id());
    std::fs::write(fx.mirror.join(&stale), b"interrupted publisher").unwrap();
    let next = std::sync::atomic::AtomicU64::new(0);
    let directory = open_root(&fx.mirror).unwrap();
    let (name, mut file) = scratch_file(&directory, &next).unwrap();
    assert_eq!(name, format!(".publish-{}-1", std::process::id()));
    file.write_all(PAGE).unwrap();
    assert_eq!(
        std::fs::read(fx.mirror.join(&stale)).unwrap(),
        b"interrupted publisher"
    );
    drop(file);
    std::fs::remove_file(fx.mirror.join(name)).unwrap();
    publish(&fx.mirror, &boinc::sha256(PAGE), PAGE).unwrap();
    publish(&fx.mirror, &boinc::sha256(PAGE), PAGE).unwrap();
    assert_eq!(
        std::fs::read(fx.mirror.join(&stale)).unwrap(),
        b"interrupted publisher"
    );
}
