//! A page through the worker: its output, retries and dead letters, loc.gov
//! holds, the JPEG fallback (#454) and the output names.

use super::super::*;
use super::{msg, worker, worker_with_scans};

#[test]
fn fallback_fetch_dimensions_keep_source_and_fallback_separate() {
    let cached = Scan {
        url: "https://example.org/page.jpg".into(),
        iiif_base: "https://example.org".into(),
        format: ScanFormat::Jpeg,
        bytes: bytes::Bytes::new(),
        md5: None,
        download: Duration::ZERO,
        from_cache: true,
        evict: None,
    };
    assert_eq!(fetch_dimensions(&Ok(cached), true), ("ok", "cache", true));
    assert_eq!(
        fetch_dimensions(&Err(FetchError::Transient("offline".into())), true),
        ("error", "loc", true)
    );
    assert_eq!(
        fetch_dimensions(&Err(FetchError::Transient("offline".into())), false),
        ("error", "loc", false)
    );
}

#[tokio::test]
async fn writes_alto_and_a_part_the_release_reads() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = worker(dir.path());
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-7", "deu"))
        .await
        .unwrap();
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-99", "deu"))
        .await
        .unwrap();
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-8", "haw"))
        .await
        .unwrap();
    queue.send(BULK, "not a message").await.unwrap();
    // Refused for its extra field; the search text mustn't be kept.
    queue
        .send(
            BULK,
            r#"{"v":1,"page":"sn83045462/1890-05-03/ed-1/seq-9","batch":"nbu_gage","language":"deu","reason":"user","queued_at":"2026-10-10T12:00:00Z","q":"secret search"}"#,
        )
        .await
        .unwrap();
    let (_tx, rx) = watch::channel(false);
    w.clone().run(rx).await.unwrap();
    // The tracker saw the page through, with its engine tag and scan
    // source, and holds nothing in flight.
    let st = w.tracker.status();
    assert_eq!(st.since_start.done, 1);
    assert_eq!(st.since_start.from_loc, 1);
    assert!(st.in_flight.is_empty());
    assert_eq!(st.recent[0].doc_id, "sn83045462_1890-05-03_ed-1_seq-7");
    assert!(st.recent[0].uploaded);

    let altos = out.list("alto").await.unwrap();
    assert_eq!(altos.len(), 1, "{altos:?}");
    let prefix = "alto/sn83045462_1890-05-03_ed-1_seq-7.node1.";
    let stamp = altos[0]
        .strip_prefix(prefix)
        .unwrap()
        .strip_suffix(".xml")
        .unwrap();
    let alto = out.get(&altos[0]).await.unwrap().unwrap();
    // The stored ALTO 3.1, written from the engine's: the scan's URL,
    // and the sha256 of the bytes read (ADR-0015 decision 9).
    let alto = String::from_utf8(alto).unwrap();
    assert!(alto.contains(&format!(
        "<fileName>https://tile.loc.gov/image-services/iiif/service:ndnp:x:7/full/full/0/default.jpg</fileName>\n      <fileIdentifier fileIdentifierLocation=\"sha256\">{}</fileIdentifier>",
        crate::prefetch::sha256(b"jpeg")
    )), "{alto}");
    assert!(
        alto.contains("alto-3-1.xsd") && alto.contains("<MeasurementUnit>pixel</MeasurementUnit>")
    );
    assert!(alto.contains("CONTENT=\"Staats-Zeitung\"") && !alto.contains("/tmp/page.jpg"));
    let parts = out.list("pages").await.unwrap();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(
        parts[0],
        format!("pages/sn83045462_1890-05-03_ed-1.node1.{stamp}.parquet")
    );
    // The provenance: the run_id in the stored ALTO, and the runs
    // record under the same name (13 §13.8).
    assert!(
        alto.contains("<processingStepSettings>")
            && alto.contains(&format!("run_id=node1.{stamp}"))
            && alto.contains("damage_before=0.62"),
        "{alto}"
    );
    let runs = out.list("runs").await.unwrap();
    assert_eq!(
        runs,
        vec![format!(
            "runs/sn83045462_1890-05-03_ed-1.node1.{stamp}.parquet"
        )]
    );
    let record = out.get(&runs[0]).await.unwrap().unwrap();
    let record = usnm_ocr_contract::runs::read(bytes::Bytes::from(record)).unwrap();
    assert_eq!(record.len(), 1);
    let r = &record[0];
    assert_eq!(r.run_id, format!("node1.{stamp}"));
    assert_eq!(r.doc_id, "sn83045462_1890-05-03_ed-1_seq-7");
    assert_eq!(r.node, "node1");
    assert_eq!(r.ocr_engine, "tesseract-5.5.1/deu_latf+deu");
    assert_eq!(
        (
            r.family.as_str(),
            r.engine_version.as_str(),
            r.profile.as_str()
        ),
        ("tesseract", "5.5.1", "deu_latf+deu")
    );
    assert!(r.selected && r.status == "ok");
    assert_eq!(r.image_sha256, crate::prefetch::sha256(b"jpeg"));
    assert_eq!(r.damage_before, Some(0.62));
    assert!(r.damage_after.is_some_and(|d| (0.0..=1.0).contains(&d)));
    assert_eq!(r.reason, "bulk");
    assert_eq!(r.language, "deu");
    assert_eq!(r.queued_at, Some("2026-10-10T12:00:00Z".parse().unwrap()));
    assert_eq!(r.words, Some(7));
    assert_eq!(
        r.text_chars,
        Some(
            "Die Nebraska Staats-Zeitung\nerscheint jeden Sonnabend."
                .chars()
                .count() as u32
        )
    );
    let bytes = out.get(&parts[0]).await.unwrap().unwrap();
    let pages = usnm_overlay::read_part(bytes::Bytes::from(bytes)).unwrap();
    assert_eq!(pages.len(), 1);
    let p = &pages[0];
    assert_eq!(p.key.to_string(), "sn83045462/1890-05-03/ed-1/seq-7");
    assert_eq!(p.batch, "nbu_gage");
    assert_eq!(p.loc_text, "bulk");
    assert_eq!(p.ocr_source, "usnm-tesseract");
    assert_eq!(p.ocr_engine, "tesseract-5.5.1/deu_latf+deu");
    assert_eq!(p.text_status, "ok");
    // Normalized as LoC's text is (04 §4.5): hyphenation rejoined, spaces collapsed.
    assert_eq!(
        p.text.as_deref(),
        Some("Die Nebraska Staats-Zeitung\nerscheint jeden Sonnabend.")
    );

    // Nothing visible: the bad ones are dead-lettered, and the page in
    // a language this node has no model for waits for another node.
    assert_eq!(
        queue.receive(BULK, Duration::from_secs(1)).await.unwrap(),
        None
    );
    let mut dead = Vec::new();
    let mut kept = Vec::new();
    while let Some(d) = queue
        .receive(DEADLETTER, Duration::from_secs(600))
        .await
        .unwrap()
    {
        use base64::Engine as _;
        let body: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(&d.text)
                .unwrap(),
        )
        .unwrap();
        assert!(!body.to_string().contains("secret"), "{body}");
        assert!(body["message_id"].is_string());
        // Each is a valid v2 dead letter, with its cause's fields.
        let parsed = message::decode_dead_letter(&d.text).unwrap();
        assert_eq!(parsed.v, 2);
        match parsed.cause.unwrap() {
            Cause::Invalid => assert_eq!(parsed.kind.as_deref(), Some("bad_json")),
            Cause::Permanent => assert!(parsed.kind.is_none() && parsed.message.is_some()),
            c => panic!("unexpected cause {c:?}"),
        }
        kept.push(body["message"]["page"].as_str().map(str::to_owned));
        dead.push(body["error"].as_str().unwrap().to_owned());
    }
    dead.sort();
    assert_eq!(dead.len(), 3, "{dead:?}");
    // Only the valid message is kept, re-encoded from its checked fields.
    kept.sort();
    assert_eq!(
        kept,
        [
            None,
            None,
            Some("sn83045462/1890-05-03/ed-1/seq-99".to_owned())
        ]
    );
    assert!(dead.iter().any(|e| e.contains("no image")));
    assert!(dead.iter().any(|e| e == "invalid message: bad_json"));
}

#[tokio::test]
async fn transient_failures_retry_then_dead_letter() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = worker(dir.path());
    queue
        .send(PRIORITY, &msg("sn83045462/1890-05-03/ed-1/seq-98", "deu"))
        .await
        .unwrap();
    for n in 1..=5 {
        // Visible again at once, as if its time had run out.
        let m = queue
            .receive(PRIORITY, Duration::ZERO)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m.dequeue_count, n);
        w.handle(m, Instant::now()).await;
    }
    // The sixth time it goes to the dead-letter queue.
    let m = queue
        .receive(PRIORITY, Duration::from_secs(600))
        .await
        .unwrap()
        .unwrap();
    w.handle(m, Instant::now()).await;
    assert_eq!(
        queue
            .receive(PRIORITY, Duration::from_secs(1))
            .await
            .unwrap(),
        None
    );
    assert!(queue
        .receive(DEADLETTER, Duration::from_secs(1))
        .await
        .unwrap()
        .is_some());
    let st = w.tracker.status();
    assert!(st.since_start.failed > 0);
    assert_eq!(st.since_start.dead_lettered, 1);
    assert_eq!(st.recent[0].error_kind.as_deref(), Some("dequeue_count"));
    assert!(out.list("pages").await.unwrap().is_empty());
}

#[tokio::test]
async fn a_page_with_no_model_fails_without_an_image_request() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out, scans) = worker_with_scans(dir.path());
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-8", "haw"))
        .await
        .unwrap();
    let m = queue.receive(BULK, Duration::ZERO).await.unwrap().unwrap();
    w.handle(m, Instant::now()).await;
    assert_eq!(scans.requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(out.list("pages").await.unwrap().is_empty());
    // Left for another node, not dead-lettered.
    assert!(queue
        .receive(DEADLETTER, Duration::from_secs(1))
        .await
        .unwrap()
        .is_none());
    assert_eq!(queue.approximate_count(BULK).await.unwrap(), 1);
}

#[tokio::test]
async fn a_long_loc_hold_takes_no_messages() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, _out, scans) = worker_with_scans(dir.path());
    // A 1-hour Retry-After, past the page budget (1440 s).
    *scans.held_until.lock().unwrap() =
        Some(tokio::time::Instant::now() + Duration::from_secs(3600));
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-7", "deu"))
        .await
        .unwrap();
    let (tx, rx) = watch::channel(false);
    let run = tokio::spawn(w.clone().run(rx));
    tokio::time::sleep(Duration::from_millis(300)).await;
    tx.send_replace(true);
    run.await.unwrap().unwrap();
    assert_eq!(scans.requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    // Never received: its first receive is still to come.
    let m = queue
        .receive(BULK, Duration::from_secs(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(m.dequeue_count, 1);
}

#[tokio::test]
async fn a_short_loc_hold_still_takes_messages() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out, scans) = worker_with_scans(dir.path());
    // Shorter than a page's budget: the page waits it out in the pacer.
    *scans.held_until.lock().unwrap() = Some(tokio::time::Instant::now() + Duration::from_secs(60));
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-7", "deu"))
        .await
        .unwrap();
    let (_tx, rx) = watch::channel(false);
    w.clone().run(rx).await.unwrap();
    assert_eq!(out.list("pages").await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_jp2_that_crashes_the_engine_is_read_from_its_jpeg() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out, scans) = worker_with_scans(dir.path());
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-97", "deu"))
        .await
        .unwrap();
    let m = queue.receive(BULK, Duration::ZERO).await.unwrap().unwrap();
    w.handle(m, Instant::now()).await;
    assert_eq!(scans.jpeg_requests.load(SeqCst), 1);
    // Done on its first receive: deleted, not retried.
    assert_eq!(queue.approximate_count(BULK).await.unwrap(), 0);
    let st = w.tracker.status();
    assert_eq!(st.since_start.done, 1);
    assert!(st.recent[0].jpeg_fallback);
    // The ALTO names the JPEG read, with no bag md5.
    let altos = out.list("alto").await.unwrap();
    let alto = String::from_utf8(out.get(&altos[0]).await.unwrap().unwrap()).unwrap();
    assert!(
        alto.contains("service:ndnp:x:97/full/full/0/default.jpg</fileName>"),
        "{alto}"
    );
    assert!(alto.contains(&crate::prefetch::sha256(b"jpeg")), "{alto}");
    assert!(!alto.contains("md5"), "{alto}");
    assert_eq!(st.since_start.from_loc, 1);
    assert_eq!(out.list("pages").await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_failed_jpeg_fallback_or_another_exit_takes_the_retry_path() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out, scans) = worker_with_scans(dir.path());
    // 96: the JPEG crashes too, and isn't retried a second way. 95: an
    // exit other than 136 gets no fallback.
    for (page, jpegs) in [("seq-96", 1), ("seq-95", 1)] {
        queue
            .send(
                BULK,
                &msg(&format!("sn83045462/1890-05-03/ed-1/{page}"), "deu"),
            )
            .await
            .unwrap();
        let m = queue.receive(BULK, Duration::ZERO).await.unwrap().unwrap();
        w.handle(m, Instant::now()).await;
        assert_eq!(scans.jpeg_requests.load(SeqCst), jpegs, "{page}");
        let st = w.tracker.status();
        assert_eq!(st.recent[0].outcome, "failed");
        assert_eq!(st.recent[0].error_kind.as_deref(), Some("engine"));
        assert!(!st.recent[0].jpeg_fallback);
        // Left on the queue, to reappear when its visibility runs out.
        assert_eq!(queue.approximate_count(BULK).await.unwrap(), 1);
        let again = queue
            .receive(BULK, Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap();
        queue.delete(&again).await.unwrap();
    }
    assert!(out.list("alto").await.unwrap().is_empty());
}

#[test]
fn status_136_is_told_apart() {
    use std::os::unix::process::ExitStatusExt;
    let e = |status| {
        anyhow::Error::new(crate::engine::Exited {
            status,
            stderr: String::new(),
        })
    };
    // From a container: 128 + SIGFPE.
    assert!(exited_136(&e(std::process::ExitStatus::from_raw(136 << 8))));
    // From a host process: the signal itself.
    assert!(exited_136(&e(std::process::ExitStatus::from_raw(8))));
    assert!(!exited_136(&e(std::process::ExitStatus::from_raw(1 << 8))));
    assert!(!exited_136(&anyhow::anyhow!(
        "tesseract exited with exit status: 136"
    )));
}

#[test]
fn output_names() {
    let key: PageKey = "sn83045462/1890-05-03/ed-2/seq-7".parse().unwrap();
    let run_id = crate::provenance::mint_run_id("nuc1");
    let stamp = run_id.strip_prefix("nuc1.").unwrap();
    let p = output_paths(&key, &run_id);
    assert_eq!(
        p.alto,
        format!("alto/sn83045462_1890-05-03_ed-2_seq-7.nuc1.{stamp}.xml")
    );
    assert_eq!(
        p.runs,
        format!("runs/sn83045462_1890-05-03_ed-2.nuc1.{stamp}.parquet")
    );
    assert_eq!(
        p.part,
        format!("pages/sn83045462_1890-05-03_ed-2.nuc1.{stamp}.parquet")
    );
    for path in [&p.alto, &p.runs, &p.part] {
        assert!(usnm_store::validate_path(path).is_ok());
    }
    assert!(check_node_id("nuc-1").is_ok());
    assert!(check_node_id("a.b").is_err());
    assert!(check_node_id("").is_err());
    let cfg = Config::default();
    assert_eq!(cfg.budget(), Duration::from_secs(1440));
}
