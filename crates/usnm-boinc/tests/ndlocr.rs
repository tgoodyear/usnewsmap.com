use std::path::{Path, PathBuf};
use usnm_boinc::{
    atomic,
    ndlocr::{self, Job, ResultManifest, OUTPUTS},
};
use usnm_ocr_adapter::ndlocr::{ndlocr_to_alto, Geometry, COMMIT};
use usnm_ocr_contract::boinc::{sha256, Scan};
use usnm_ocr_worker::prefetch::{sidecar_path, Sidecar};
use usnm_store::{local::LocalStore, ObjectStore};

fn job(reason: &str) -> Job {
    let mut job = Job {
        v: 1,
        id: String::new(),
        page: "sn83025517/1945-01-01/ed-1/seq-4".into(),
        batch: "dlc_ballston".into(),
        batch_version: 1,
        loc_text: reason.into(),
        route: usnm_ocr_worker::engine::candidates("jpn").remove(0),
        engine: ndlocr::engine(format!("sha256:{}", "a".repeat(64)), 600),
        scan: Scan {
            url: "https://tile.loc.gov/image/full/full/0/default.jpg".into(),
            iiif_base: "https://tile.loc.gov/image".into(),
            sha256: sha256(b"scan"),
            bytes: 4,
            md5: None,
        },
    };
    job.id = job.identity().unwrap();
    job
}
fn files(dir: &Path, job: &Job, text: &str) -> Vec<PathBuf> {
    let lines: String = text.lines().map(|line| format!("<LINE X=\"10\" Y=\"10\" WIDTH=\"900\" HEIGHT=\"100\" STRING=\"{line}\" CONF=\"0.95\"/>")).collect();
    let xml = format!("<OCRDATASET><PAGE WIDTH=\"1000\" HEIGHT=\"2000\"><TEXTBLOCK>{lines}</TEXTBLOCK></PAGE></OCRDATASET>");
    files_with_xml(dir, job, text, &xml)
}
fn files_with_xml(dir: &Path, job: &Job, text: &str, xml: &str) -> Vec<PathBuf> {
    let alto = ndlocr_to_alto(
        xml.as_bytes(),
        Geometry::default(),
        &job.scan.url,
        COMMIT,
        1,
        "2026-10-16T00:00:00+00:00",
    )
    .unwrap();
    let normalized = usnm_ocr_adapter::ndlocr::normalize(text);
    let r = ResultManifest {
        job: job.clone(),
        text_sha256: sha256(normalized.as_bytes()),
        alto_sha256: sha256(&alto),
        elapsed_ms: 500,
        completed_at: "2026-10-16T00:00:00+00:00".into(),
    };
    let files: Vec<_> = OUTPUTS.iter().map(|n| dir.join(n)).collect();
    atomic(&files[0], normalized.as_bytes()).unwrap();
    atomic(&files[1], &alto).unwrap();
    atomic(&files[2], &serde_json::to_vec(&r).unwrap()).unwrap();
    files
}
#[test]
fn route_identity_and_resources_are_japanese_specific() {
    let j = job("mixed");
    j.validate().unwrap();
    let mut changed = j.clone();
    changed.loc_text = "bulk".into();
    changed.id = changed.identity().unwrap();
    assert!(changed.validate().is_err());
    for change in ["reason", "profile", "engine", "image", "models", "scan"] {
        let mut changed = j.clone();
        match change {
            "reason" => changed.loc_text = "missing".into(),
            "profile" => changed.route.profile = "jpn".into(),
            "engine" => changed.engine.kind = usnm_ocr_contract::routing::EngineKind::Tesseract,
            "image" => changed.engine.image = "latest".into(),
            "models" => changed.engine.models = "sha256:000000000000".into(),
            _ => changed.scan.sha256 = sha256(b"other"),
        }
        assert_ne!(changed.identity().unwrap(), j.id);
        if change != "reason" && change != "scan" {
            changed.id = changed.identity().unwrap();
            assert!(changed.validate().is_err(), "{change}");
        }
    }
    let args = ndlocr::create_work_args(&j);
    for (flag, value) in [
        ("--min_quorum", "2"),
        ("--target_nresults", "2"),
        ("--rsc_memory_bound", "4294967296"),
        ("--rsc_fpops_est", "1e12"),
        ("--appname", "usnm_ndlocr"),
    ] {
        assert!(args.windows(2).any(|w| w == [flag, value]));
    }
}
#[test]
fn japanese_similarity_handles_local_differences_not_word_bags() {
    let text = vec!["日本語新聞記事を読みます。"; 80].join("\n");
    let changed = text.replacen('新', "親", 1);
    assert!(ndlocr::similarity(&text, &changed) >= ndlocr::MIN_SIMILARITY);
    assert!(ndlocr::similarity(&text, "全然異なる記事") < ndlocr::MIN_SIMILARITY);
    assert_eq!(ndlocr::similarity("日", "本"), 0.0);
    assert_eq!(ndlocr::similarity("", ""), 1.0);
}
#[test]
fn validator_checks_hashes_layout_job_and_cross_host_similarity() {
    let dir = tempfile::tempdir().unwrap();
    let j = job("mixed");
    let text = vec!["日本語新聞記事を読みます。"; 80].join("\n");
    let a = files(&dir.path().join("a"), &j, &text);
    let b = files(&dir.path().join("b"), &j, &text.replacen('新', "親", 1));
    ndlocr::compare(&a, &b).unwrap();
    let c = files(&dir.path().join("c"), &job("missing"), &text);
    assert!(ndlocr::compare(&a, &c).is_err());
    let d = files(&dir.path().join("d"), &j, "別の記事を読んだ結果");
    assert!(ndlocr::compare(&a, &d).is_err());
    atomic(&b[0], b"tampered").unwrap();
    assert!(ndlocr::load(&b).is_err());
    let mut r: ResultManifest = serde_json::from_slice(&std::fs::read(&a[2]).unwrap()).unwrap();
    r.elapsed_ms = 600001;
    atomic(&a[2], &serde_json::to_vec(&r).unwrap()).unwrap();
    assert!(ndlocr::load(&a).is_err());
}
#[test]
fn validator_preserves_lines_when_layout_and_upstream_text_orders_differ() {
    let dir = tempfile::tempdir().unwrap();
    let j = job("missing");
    let xml = r#"<OCRDATASET><PAGE WIDTH="1000" HEIGHT="2000">
<TEXTBLOCK><LINE X="10" Y="10" WIDTH="900" HEIGHT="100" ORDER="2" STRING="新聞"/>
<LINE X="10" Y="110" WIDTH="900" HEIGHT="100" ORDER="3" STRING="新聞"/></TEXTBLOCK>
<TEXTBLOCK><LINE X="10" Y="210" WIDTH="900" HEIGHT="100" ORDER="1" STRING="記事"/></TEXTBLOCK>
</PAGE></OCRDATASET>"#;
    let text = "新聞\n新聞\n記事";
    let valid = files_with_xml(&dir.path().join("valid"), &j, text, xml);
    ndlocr::load(&valid).unwrap();
    for (name, altered) in [
        ("deleted", "新聞\n記事"),
        ("duplicated", "新聞\n新聞\n新聞\n記事"),
        ("substituted", "聞新\n新聞\n記事"),
        ("repartitioned", "新\n聞新聞\n記事"),
    ] {
        let files = files_with_xml(&dir.path().join(name), &j, altered, xml);
        assert!(
            ndlocr::load(&files)
                .err()
                .unwrap()
                .to_string()
                .contains("ALTO/text disagree"),
            "{name}"
        );
    }
    let reordered = files_with_xml(&dir.path().join("reordered"), &j, "記事\n新聞\n新聞", xml);
    ndlocr::load(&reordered).unwrap();
    assert!(ndlocr::compare(&valid, &reordered).is_err());
}
#[tokio::test]
async fn canonical_collection_uses_release_overlay_and_replays_accepted_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let spool = dir.path().join("spool");
    let j = job("mixed");
    let doc = "sn83025517_1945-01-01_ed-1_seq-4";
    let unit = root.join("units").join(doc);
    atomic(
        &unit.join(ndlocr::JOB_FILE),
        &serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    atomic(&unit.join("submitted"), b"yes").unwrap();
    // The collector marks an English-heavy reading as mixed; ingest tests
    // separately cover the Japanese index's mixed-text acceptance gate.
    let text = format!(
        "{} 一",
        vec!["English advertisements and community news."; 30].join("\n")
    );
    let f = files(&spool, &j, &text);
    let mut host_result: ResultManifest =
        serde_json::from_slice(&std::fs::read(&f[2]).unwrap()).unwrap();
    host_result.completed_at = "2099-01-01T00:00:00+00:00".into();
    atomic(&f[2], &serde_json::to_vec(&host_result).unwrap()).unwrap();
    for (path, name) in f.iter().zip(OUTPUTS) {
        std::fs::rename(path, spool.join(format!("{}__file_{name}", j.workunit()))).unwrap();
    }
    let store = LocalStore::new(dir.path().join("curated"));
    ndlocr::collect(&root, &spool, doc, &store).await.unwrap();
    let parts = store.list("ocr-ja/pages").await.unwrap();
    assert_eq!(parts.len(), 1);
    let raw = store.get(&parts[0]).await.unwrap().unwrap();
    let row = usnm_overlay::read_part(raw.into()).unwrap().remove(0);
    assert_eq!(row.ocr_source, "usnm-ndlocr-lite");
    assert_eq!(row.loc_text, "mixed");
    assert_eq!(
        row.text
            .as_deref()
            .unwrap()
            .chars()
            .filter(|&c| usnm_core::ja::is_ja(c))
            .count(),
        1
    );
    let accepted_at = chrono::DateTime::parse_from_rfc3339(
        &std::fs::read_to_string(unit.join("accepted/accepted-at.txt")).unwrap(),
    )
    .unwrap();
    assert_eq!(row.ocred_at, accepted_at.timestamp_micros());
    assert!(accepted_at <= chrono::Utc::now());
    assert!(accepted_at.timestamp() < 4_070_908_800); // Never the host's 2099 date.
    let alto = store.list("ocr-ja/alto").await.unwrap();
    assert_eq!(alto.len(), 1);
    std::fs::remove_file(unit.join("published")).unwrap();
    std::fs::remove_dir_all(&spool).unwrap();
    ndlocr::collect(&root, &spool, doc, &store).await.unwrap();
    assert_eq!(store.list("ocr-ja/pages").await.unwrap().len(), 1);
}
#[tokio::test]
async fn mismatched_canonical_job_never_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let spool = dir.path().join("spool");
    let j = job("missing");
    let other = job("mixed");
    let doc = "sn83025517_1945-01-01_ed-1_seq-4";
    let unit = root.join("units").join(doc);
    atomic(
        &unit.join(ndlocr::JOB_FILE),
        &serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    atomic(&unit.join("submitted"), b"yes").unwrap();
    let f = files(&spool, &other, "日本語新聞記事を読みます。");
    for (path, name) in f.iter().zip(OUTPUTS) {
        std::fs::rename(path, spool.join(format!("{}__file_{name}", j.workunit()))).unwrap();
    }
    let store = LocalStore::new(dir.path().join("curated"));
    assert!(ndlocr::collect(&root, &spool, doc, &store).await.is_err());
    assert!(store.list("ocr-ja/pages").await.unwrap().is_empty());
}

#[tokio::test]
async fn worker_rejects_wrong_scan_before_running_engine() {
    let dir = tempfile::tempdir().unwrap();
    let j = job("missing");
    atomic(
        &dir.path().join(ndlocr::JOB_FILE),
        &serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    atomic(&dir.path().join("page"), b"tampered").unwrap();
    let error = usnm_boinc::worker::run(dir.path()).await.unwrap_err();
    assert!(error.to_string().contains("scan differs"), "{error:#}");
    assert!(!dir.path().join("result.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn submission_stages_public_inputs_and_never_blindly_retries() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let project = dir.path().join("project");
    let doc = "sn83025517_1945-01-01_ed-1_seq-4";
    let unit = root.join("units").join(doc);
    let j = job("missing");
    atomic(
        &unit.join(ndlocr::JOB_FILE),
        &serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    atomic(&unit.join("page"), b"scan").unwrap();
    atomic(
        &project.join("bin/dir_hier_path"),
        b"#!/bin/sh\nprintf 'download/fanout/%s\\n' \"$1\"\n",
    )
    .unwrap();
    atomic(
        &project.join("bin/create_work"),
        b"#!/bin/sh\nprintf '%s\\n' \"$@\" > args\nexit 1\n",
    )
    .unwrap();
    for name in ["dir_hier_path", "create_work"] {
        std::fs::set_permissions(
            project.join("bin").join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    assert!(ndlocr::submit(&root, &project, doc).await.is_err());
    assert!(unit.join("submitting").exists());
    assert!(!unit.join("submitted").exists());
    assert!(ndlocr::submit(&root, &project, doc)
        .await
        .unwrap_err()
        .to_string()
        .contains("uncertain"));
    let download = project.join("download/fanout");
    assert_eq!(
        std::fs::metadata(&download).unwrap().permissions().mode() & 0o755,
        0o755
    );
    for suffix in ["page", "ndlocr_json"] {
        let file = download.join(format!("{}_{suffix}", j.id));
        assert_eq!(
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
    let args = std::fs::read_to_string(project.join("args")).unwrap();
    assert!(args.contains("--min_quorum\n2\n"));
    // An operator has verified absence on the server before removing the marker.
    std::fs::remove_file(unit.join("submitting")).unwrap();
    atomic(&project.join("bin/create_work"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        project.join("bin/create_work"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    ndlocr::submit(&root, &project, doc).await.unwrap();
    assert!(unit.join("submitted").exists());
}

#[test]
fn worker_rejects_wrong_installed_image_before_loading_models() {
    let dir = tempfile::tempdir().unwrap();
    let j = job("missing");
    atomic(
        &dir.path().join(ndlocr::JOB_FILE),
        &serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    atomic(&dir.path().join("page"), b"scan").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_usnm-boinc"))
        .args(["work", "--dir"])
        .arg(dir.path())
        .env("USNM_NDLOCR_IMAGE", format!("sha256:{}", "b".repeat(64)))
        .env_remove("NDLOCR_ROOT")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("engine image differs"));
    assert!(!dir.path().join("result.json").exists());
}

#[tokio::test]
async fn invalid_stage_metadata_is_rejected_before_fetching_scans() {
    let dir = tempfile::tempdir().unwrap();
    for page in [
        "bulk:jpn:sn83025517_1945-01-01_ed-1_seq-4:dlc_ballston_ver01",
        "mixed:eng:sn83025517_1945-01-01_ed-1_seq-4:dlc_ballston_ver01",
    ] {
        let error = ndlocr::stage(
            dir.path(),
            &[page.parse().unwrap()],
            job("missing").engine,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("classification") || error.to_string().contains("needs jpn"),
            "{error:#}"
        );
        assert!(!dir.path().join("hold").exists());
    }
}

const FETCHED_PAGE: &str = "sn83025333_1943-01-30_ed-1_seq-8";

/// One JP2 and its sidecar as the fetcher lays them out; returns the scan's path.
fn fetched_scan(scans: &Path, bytes: &[u8]) -> PathBuf {
    let path = scans.join("keep/dlc_dupontcircle/sn83025333/1943-01-30/1/8.jp2");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let side = Sidecar {
        doc_id: FETCHED_PAGE.into(),
        batch: "dlc_dupontcircle".into(),
        batch_version: 1,
        loc_url: "https://tile.loc.gov/storage-services/service/sgp/page.jp2".into(),
        iiif_base: "https://tile.loc.gov/image-services/iiif/service:sgp:page".into(),
        resolution: "full".into(),
        fetched_at: chrono::Utc::now(),
        bytes: bytes.len() as u64,
        sha256: sha256(bytes),
        download_ms: 1,
        user_agent: "test".into(),
        format: "jp2".into(),
        md5: Some(usnm_ocr_worker::batch::md5_hex(bytes)),
    };
    std::fs::write(sidecar_path(&path), serde_json::to_vec(&side).unwrap()).unwrap();
    path
}

fn fetched_stage_page() -> usnm_boinc::surya::Page {
    format!("mixed:jpn:{FETCHED_PAGE}:dlc_dupontcircle_ver01")
        .parse()
        .unwrap()
}

#[tokio::test]
async fn stage_from_the_fetchers_tree_keeps_its_urls_and_sends_nothing_to_loc() {
    let dir = tempfile::tempdir().unwrap();
    let scans = dir.path().join("scans");
    let root = dir.path().join("stage");
    fetched_scan(&scans, b"\xff\x4f\xff\x51 a scan");
    let staged = ndlocr::stage(
        &root,
        &[fetched_stage_page()],
        job("missing").engine,
        None,
        None,
        Some(&scans),
    )
    .await
    .unwrap();
    assert_eq!(staged, 1);
    let unit = root.join("units").join(FETCHED_PAGE);
    let staged_job = ndlocr::load_job(&unit).unwrap();
    assert_eq!(
        staged_job.scan.url,
        "https://tile.loc.gov/storage-services/service/sgp/page.jp2"
    );
    assert_eq!(
        staged_job.scan.iiif_base,
        "https://tile.loc.gov/image-services/iiif/service:sgp:page"
    );
    assert_eq!(staged_job.batch, "dlc_dupontcircle");
    assert_eq!(
        std::fs::read(unit.join("page")).unwrap(),
        b"\xff\x4f\xff\x51 a scan"
    );
}

#[tokio::test]
async fn stage_from_the_fetchers_tree_refuses_a_changed_scan_and_never_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let scans = dir.path().join("scans");
    let root = dir.path().join("stage");
    let page = fetched_stage_page();
    let stage = || {
        ndlocr::stage(
            &root,
            std::slice::from_ref(&page),
            job("missing").engine,
            None,
            None,
            Some(&scans),
        )
    };
    // A missing page ends the stage: there is no LoC client to fall back to.
    let error = stage().await.unwrap_err();
    assert!(error.to_string().contains("scan not found"), "{error:#}");
    let path = fetched_scan(&scans, b"a scan");
    // The same page under another batch version is not this scan.
    let other: usnm_boinc::surya::Page = format!("mixed:jpn:{FETCHED_PAGE}:dlc_dupontcircle_ver02")
        .parse()
        .unwrap();
    let error = ndlocr::stage(
        &root,
        &[other],
        job("missing").engine,
        None,
        None,
        Some(&scans),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("differs from its sidecar"),
        "{error:#}"
    );
    std::fs::write(&path, b"another").unwrap();
    let error = stage().await.unwrap_err();
    assert!(
        error.to_string().contains("differs from its sidecar"),
        "{error:#}"
    );
    assert!(!root.join("units").join(FETCHED_PAGE).exists());
}

#[test]
fn validator_normalizes_decomposed_kana_from_alto() {
    let dir = tempfile::tempdir().unwrap();
    let j = job("mixed");
    let files = files(dir.path(), &j, "か\u{3099}くせいの新聞");
    let loaded = ndlocr::load(&files).unwrap();
    assert_eq!(std::str::from_utf8(&loaded.text).unwrap(), "がくせいの新聞");
    assert!(std::str::from_utf8(&loaded.alto)
        .unwrap()
        .contains("か\u{3099}"));
}
