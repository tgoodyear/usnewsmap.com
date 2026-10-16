use std::path::{Path, PathBuf};
use usnm_boinc::{
    atomic,
    ndlocr::{self, Job, ResultManifest, OUTPUTS},
};
use usnm_ocr_adapter::ndlocr::{ndlocr_to_alto, Geometry, COMMIT};
use usnm_ocr_contract::boinc::{sha256, Scan};
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
    let alto = ndlocr_to_alto(
        xml.as_bytes(),
        Geometry::default(),
        &job.scan.url,
        COMMIT,
        1,
        "2026-10-16T00:00:00+00:00",
    )
    .unwrap();
    let r = ResultManifest {
        job: job.clone(),
        text_sha256: sha256(text.as_bytes()),
        alto_sha256: sha256(&alto),
        elapsed_ms: 500,
        completed_at: "2026-10-16T00:00:00+00:00".into(),
    };
    let files: Vec<_> = OUTPUTS.iter().map(|n| dir.join(n)).collect();
    atomic(&files[0], text.as_bytes()).unwrap();
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
    // A long English column with too few Japanese characters stays out of the Japanese index.
    let text = format!(
        "{} 一",
        vec!["English advertisements and community news."; 30].join("\n")
    );
    let f = files(&spool, &j, &text);
    for (path, name) in f.iter().zip(OUTPUTS) {
        std::fs::rename(path, spool.join(format!("{}__file_{name}", j.workunit()))).unwrap();
    }
    let store = LocalStore::new(dir.path().join("curated"));
    ndlocr::collect(&root, &spool, doc, &store).await.unwrap();
    let parts = store.list("ocr-ja/pages").await.unwrap();
    assert_eq!(parts.len(), 1);
    let pages = usnm_ingest::ocr_ja::read_part(
        store.get(&parts[0]).await.unwrap().unwrap().into(),
        &parts[0],
    )
    .unwrap();
    assert_eq!(pages[0].ocr_source, "usnm-ndlocr-lite");
    assert_eq!(pages[0].loc_text, "mixed");
    assert!(!pages[0].indexable());
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
