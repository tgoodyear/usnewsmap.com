//! The durable observations of each read (`run_metadata_dir`).

use super::super::*;
use super::{msg, one_page, with_engine, worker, BadAltoEngine, ColdInstanceEngine, NoParts};
use async_trait::async_trait;

fn metadata_files(root: &Path) -> Vec<serde_json::Value> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            assert_eq!(path.extension().unwrap(), "json");
            let bytes = std::fs::read(&path).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let hash = crate::prefetch::sha256(value["run_id"].as_str().unwrap().as_bytes());
            assert_eq!(
                path.file_name().unwrap().to_str().unwrap(),
                format!("{hash}.json")
            );
            for excluded in [
                "Nebraska",
                "secret page text",
                "Staats-Zeitung",
                "<alto",
                "stderr",
            ] {
                assert!(!std::str::from_utf8(&bytes).unwrap().contains(excluded));
            }
            assert!(value.get("accepted").is_none());
            assert!(value.get("imported").is_none());
            assert!(value.get("indexed").is_none());
            value
        })
        .collect()
}

#[tokio::test]
async fn run_metadata_captures_concurrent_reads_replay_and_jpeg_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let (mut w, _, out) = worker(dir.path());
    let root = dir.path().join("metadata");
    assert!(w.cfg.run_metadata_dir.is_none());
    Arc::get_mut(&mut w).unwrap().cfg.run_metadata_dir = Some(root.clone());
    let (m, key) = message::decode(&msg("sn83045462/1890-05-03/ed-1/seq-7", "deu")).unwrap();
    let (fallback_m, fallback_key) =
        message::decode(&msg("sn83045462/1890-05-03/ed-1/seq-97", "deu")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let (first, replay, fallback) = tokio::join!(
        w.process(&m, &key, "first", deadline),
        w.process(&m, &key, "replay", deadline),
        w.process(&fallback_m, &fallback_key, "fallback", deadline),
    );
    let done = [first.unwrap(), replay.unwrap(), fallback.unwrap()];
    let rows = metadata_files(&root);
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row["metadata_schema"], 1);
        assert_eq!(row["status"], "ok");
        assert!(row["error_kind"].is_null());
        assert!(row["started_at"].as_str().unwrap() <= row["finished_at"].as_str().unwrap());
        assert!(row["seconds_wall"].as_f64().unwrap() >= 0.0);
        let record =
            usnm_ocr_contract::runs::from_json_line(&row["run_record"].to_string()).unwrap();
        let page = done.iter().find(|d| d.run_id == record.run_id).unwrap();
        let uploaded =
            usnm_ocr_contract::runs::read(out.get(&page.runs).await.unwrap().unwrap().into())
                .unwrap();
        let mut parquet_record = record.clone();
        // Parquet stores the existing contract's timestamps at microsecond precision.
        parquet_record.node_finished_at =
            chrono::DateTime::from_timestamp_micros(record.node_finished_at.timestamp_micros())
                .unwrap();
        assert_eq!(uploaded, vec![parquet_record]);
        assert_eq!(record.image_sha256, crate::prefetch::sha256(b"jpeg"));
        assert!(record.seconds_cpu.is_none() && record.peak_mem_mb.is_none());
        assert!(record.selected); // Route selection, not project acceptance.
        let observations = row["observations"].as_array().unwrap();
        let last = observations.last().unwrap();
        assert_eq!(last["status"], "ok");
        assert_eq!(last["info_source"], "result");
        assert_eq!(last["info"]["engine_id"], record.engine_id);
        assert_eq!(last["image_sha256"], record.image_sha256);
        assert!(last["engine_finished_at"].is_string());
        if page.jpeg_fallback {
            assert_eq!(observations.len(), 2);
            assert_eq!(observations[0]["status"], "failed");
            assert_eq!(observations[0]["error_kind"], "engine_crash");
            assert_eq!(
                observations[0]["image_sha256"],
                crate::prefetch::sha256(b"jp2 exit 136")
            );
        } else {
            assert_eq!(observations.len(), 1);
        }
    }
}

#[tokio::test]
async fn run_metadata_keeps_pre_engine_failures_and_actual_instance_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (mut w, _, _) = worker(dir.path());
    let root = dir.path().join("metadata");
    Arc::get_mut(&mut w).unwrap().cfg.run_metadata_dir = Some(root.clone());
    for (seq, language, kind) in [(7, "haw", "no_model"), (98, "deu", "fetch")] {
        let (m, key) = message::decode(&msg(
            &format!("sn83045462/1890-05-03/ed-1/seq-{seq}"),
            language,
        ))
        .unwrap();
        let result = w
            .process(&m, &key, "test", Instant::now() + Duration::from_secs(30))
            .await;
        assert!(matches!(result, Err(Failure::Transient(k, _)) if k == kind));
    }
    let failing = with_engine(&w, Arc::new(ColdInstanceEngine));
    let (m, key) = message::decode(&msg("sn83045462/1890-05-03/ed-1/seq-7", "deu")).unwrap();
    assert!(matches!(
        failing
            .process(&m, &key, "test", Instant::now() + Duration::from_secs(30))
            .await,
        Err(Failure::Transient("engine", _))
    ));
    let bad_alto = with_engine(&w, Arc::new(BadAltoEngine));
    assert!(matches!(
        bad_alto
            .process(&m, &key, "test", Instant::now() + Duration::from_secs(30))
            .await,
        Err(Failure::Transient("convert", _))
    ));
    let rows = metadata_files(&root);
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(row["status"], "failed");
        assert!(row["run_record"].is_null());
        let observations = row["observations"].as_array().unwrap();
        match row["error_kind"].as_str().unwrap() {
            "no_model" | "fetch" => assert!(observations.is_empty()),
            "engine" => {
                assert_eq!(observations[0]["status"], "timeout");
                assert_eq!(observations[0]["error_kind"], "engine_timeout");
                assert_eq!(observations[0]["info_source"], "instance");
                assert_eq!(observations[0]["info"]["cold_start"], true);
                assert_eq!(observations[0]["info"]["model_load_seconds"], 2.5);
                assert_eq!(
                    observations[0]["image_sha256"],
                    crate::prefetch::sha256(b"jpeg")
                );
            }
            "convert" => assert_eq!(observations[0]["status"], "ok"),
            other => panic!("unexpected error {other}"),
        }
    }
}

#[tokio::test]
async fn run_metadata_observes_budget_and_dropped_page_without_claiming_engine_exit() {
    #[derive(Debug)]
    struct WaitingEngine;
    #[async_trait]
    impl Engine for WaitingEngine {
        fn source(&self) -> &str {
            "usnm-tesseract"
        }
        fn can_read(&self, _: &str) -> bool {
            true
        }
        async fn ocr(&self, _: &Path, _: &str, _: &Path) -> Result<EngineOutput, EngineError> {
            std::future::pending().await
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut w, _, _) = worker(dir.path());
    let root = dir.path().join("metadata");
    Arc::get_mut(&mut w).unwrap().cfg.run_metadata_dir = Some(root.clone());
    let waiting = with_engine(&w, Arc::new(WaitingEngine));
    let (m, key) = message::decode(&msg("sn83045462/1890-05-03/ed-1/seq-7", "deu")).unwrap();
    assert!(matches!(
        waiting
            .process(
                &m,
                &key,
                "budget",
                Instant::now() + Duration::from_millis(50)
            )
            .await,
        Err(Failure::Abandoned)
    ));
    let mut page = Box::pin(waiting.process(
        &m,
        &key,
        "shutdown",
        Instant::now() + Duration::from_secs(30),
    ));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut page)
        .await
        .is_err());
    drop(page);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = std::fs::read_dir(&root).map_or(0, |files| {
                files
                    .filter_map(Result::ok)
                    .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
                    .count()
            });
            if count == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let rows = metadata_files(&root);
    assert_eq!(rows.len(), 2);
    let kinds: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r["error_kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["budget", "shutdown"].into_iter().collect());
    for row in rows {
        assert_eq!(row["status"], "abandoned");
        assert!(row["run_record"].is_null());
        let observation = &row["observations"][0];
        assert_eq!(observation["status"], "abandoned");
        assert!(observation["engine_finished_at"].is_null());
        assert!(observation["seconds_wall"].as_f64().unwrap() > 0.0);
    }
}

#[tokio::test]
async fn run_metadata_write_failure_does_not_change_success_or_original_engine_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut w, _, _) = worker(dir.path());
    let not_a_dir = dir.path().join("file");
    std::fs::write(&not_a_dir, b"not a directory").unwrap();
    Arc::get_mut(&mut w).unwrap().cfg.run_metadata_dir = Some(not_a_dir);
    let (m, key) = message::decode(&msg("sn83045462/1890-05-03/ed-1/seq-7", "deu")).unwrap();
    assert!(w
        .process(&m, &key, "test", Instant::now() + Duration::from_secs(30))
        .await
        .is_ok());
    let failing = with_engine(&w, Arc::new(ColdInstanceEngine));
    assert!(matches!(
        failing
            .process(&m, &key, "test", Instant::now() + Duration::from_secs(30))
            .await,
        Err(Failure::Transient("engine", _))
    ));
    let (_, rx) = watch::channel(false);
    assert!(w.run(rx).await.is_err()); // Misconfiguration fails before taking queue work.
}

#[tokio::test]
async fn run_metadata_queue_path_preserves_upload_failure_and_redelivery() {
    let dir = tempfile::tempdir().unwrap();
    let (mut w, queue, out) = worker(dir.path());
    let root = dir.path().join("metadata");
    {
        let w = Arc::get_mut(&mut w).unwrap();
        w.cfg.run_metadata_dir = Some(root.clone());
        w.out = Arc::new(NoParts(out.clone()));
    }
    one_page(&w, &queue, "sn83045462/1890-05-03/ed-1/seq-7", "deu").await;
    let rows = metadata_files(&root);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "failed");
    assert_eq!(rows[0]["error_kind"], "output");
    assert_eq!(rows[0]["run_record"]["status"], "ok");
    assert_eq!(rows[0]["observations"][0]["status"], "ok");
    assert_eq!(out.list("runs").await.unwrap().len(), 1);
    assert!(out.list("pages").await.unwrap().is_empty());
    let again = queue.receive(BULK, Duration::ZERO).await.unwrap().unwrap();
    assert_eq!(again.dequeue_count, 2);
}
