use super::*;
use crate::retry::Provenance;
use clap::Parser;
use std::sync::Arc;
use usnm_ocr_contract::boinc::Job;
use usnm_ocr_local::ledger;
use usnm_state::docs::MemoryDocs;

const NONCE: &str = "0123456789abcdef0123456789abcdef";
const ALTO: &str = r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><sourceImageInformation><fileName>page</fileName></sourceImageInformation></Description><Layout><Page WIDTH="1000" HEIGHT="1000"><PrintSpace><TextBlock><TextLine><String CONTENT="HELLO"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#;

/// A local store whose `list` matches names by string prefix, as Blob Storage's does (a local
/// store treats the prefix as a directory).
#[derive(Debug)]
struct ByPrefix(Arc<dyn ObjectStore>);

#[async_trait::async_trait]
impl ObjectStore for ByPrefix {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
        self.0.get(path).await
    }
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, usnm_store::StoreError> {
        self.0.put_new(path, body, content_type).await
    }
    async fn put(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<(), usnm_store::StoreError> {
        self.0.put(path, body, content_type).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
        let dir = prefix.rsplit_once('/').map_or("", |(dir, _)| dir);
        let mut all = self.0.list(dir).await?;
        all.retain(|name| name.starts_with(prefix));
        Ok(all)
    }
}

/// A page that failed for good under an old journal (no terminal fields), dead-lettered in the
/// native ledger, with the hand-off, lake and state a controller sees.
struct Fx {
    temp: tempfile::TempDir,
    handoff: Arc<dyn ObjectStore>,
    curated: Arc<dyn ObjectStore>,
    reference: Arc<dyn ObjectStore>,
    out: Arc<dyn ObjectStore>,
    state: State,
    parent: Record,
    page: Vec<u8>,
    image: String,
}

impl Fx {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let open = |name: &str| usnm_store::open(temp.path().join(name).to_str().unwrap()).unwrap();
        let (handoff, curated, reference, out) = (
            open("handoff"),
            open("curated"),
            open("reference"),
            Arc::new(ByPrefix(open("out"))) as Arc<dyn ObjectStore>,
        );
        let state = State::new(Arc::new(MemoryDocs::default()));
        state.set_current_version("v1").await.unwrap();
        reference.put_new("v1/batches.json", br#"[{"batch":"test_batch","curated":{"version":1,"parts":[],"counts":"","pages":1,"ok_pages":1,"lccns":["sn00000001"],"source_sha256":"","first":"","last":""}}]"#.to_vec(), "application/json").await.unwrap();
        reference
            .put_new(
                "v1/titles.json",
                br#"[{"lccn":"sn00000001","languages":["eng"]}]"#.to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        let mut job: Job = serde_json::from_str(include_str!(
            "../../../usnm-ocr-contract/src/fixtures/boinc-tesseract.json"
        ))
        .unwrap();
        let page = b"cached scan".to_vec();
        job.scan.bytes = page.len() as u64;
        job.scan.sha256 = boinc::sha256(&page);
        job.id = job.identity().unwrap();
        // Failed for good, but the journal predates terminal fields.
        let parent = Record {
            job,
            plan: "slow-class".into(),
            stage: Stage::Failed,
            stamp: "prior".into(),
            error: None,
            terminal: None,
            retry: None,
            ledger: None,
            bundle: None,
            unbundled: None,
            held: None,
        };
        let previous = parent.queued().unwrap();
        ledger::write_queued(
            curated.as_ref(),
            std::slice::from_ref(&previous),
            previous.queued_at,
            "prior",
        )
        .await
        .unwrap();
        let at = previous.queued_at + chrono::Duration::minutes(1);
        ledger::write_outcomes(
            curated.as_ref(),
            &[ledger::Outcome {
                doc_id: previous.doc_id.clone(),
                outcome: ledger::DEAD_LETTERED.into(),
                cause: Some("permanent".into()),
                kind: None,
                source: "prior-dead-letter".into(),
                at,
                run: "prior".into(),
                message_sha256: Some(boinc::sha256(
                    &serde_json::to_vec(&parent.job.message).unwrap(),
                )),
            }],
            at,
            "prior",
        )
        .await
        .unwrap();
        let fx = Self {
            temp,
            handoff,
            curated,
            reference,
            out,
            state,
            parent,
            page,
            image: format!("sha256:{}", "b".repeat(64)),
        };
        fx.put_proof(NONCE, Utc::now(), 2, 0).await;
        fx.put_trust(&[1, 2, 9]).await;
        fx
    }

    /// Removes an object from the hand-off (a local store keeps each at its path).
    fn remove(&self, path: &str) {
        std::fs::remove_file(self.temp.path().join("handoff").join(path)).unwrap();
    }

    fn id(&self) -> &str {
        &self.parent.job.id
    }

    fn workunit(&self) -> String {
        self.parent.job.workunit()
    }

    async fn put_proof(&self, nonce: &str, at: DateTime<Utc>, mask: u32, canonical: u64) {
        let doc = serde_json::json!({
            "v": 1, "nonce": nonce, "checked_at": at.to_rfc3339(),
            "workunits": [{"name": self.workunit(), "id": 20337, "error_mask": mask,
                           "canonical_resultid": canonical, "assimilate_state": 2}],
        });
        self.handoff
            .put(
                WU_PROOF_PATH,
                serde_json::to_vec(&doc).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
    }

    /// `gold/trust.json`: `hosts` have a gold report for the child's image in eng/eng.
    async fn put_trust(&self, hosts: &[u64]) {
        let list = serde_json::json!({
            "hosts": {self.image.clone(): hosts},
            "manifest_sha256": "b".repeat(64),
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "max_cer": 0.08734328574534386,
            "v": 4
        });
        self.handoff
            .put(
                "gold/trust.json",
                serde_json::to_vec(&list).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
    }

    /// The journal and page as an evidence copy keeps them: `jobs/<id>/` in a directory.
    fn archive(&self) -> PathBuf {
        let base = self.temp.path().join("evidence").join("queue-feed-old");
        let dir = base.join("jobs").join(self.id());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("record.json"),
            serde_json::to_vec_pretty(&self.parent).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("page"), &self.page).unwrap();
        base
    }

    /// The same journal and page in the live hand-off.
    async fn put_live(&self) {
        let prefix = format!("dispatch/jobs/{}", self.id());
        self.handoff
            .put(
                &format!("{prefix}/record.json"),
                serde_json::to_vec_pretty(&self.parent).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        self.handoff
            .put(&format!("{prefix}/page"), self.page.clone(), "image/jpeg")
            .await
            .unwrap();
    }

    fn request<'a>(&'a self, journal: Option<&'a Path>) -> Request<'a> {
        Request {
            parent: self.id(),
            journal_dir: journal,
            nonce: NONCE,
            image: &self.image,
            app_version_num: 106,
            timeout_secs: Some(3600),
            reason: Reason::SlowClass,
            reviewed_reason: "reviewed: WU error mask 0x2, no canonical result",
            target_hosts: &[],
            dry_run: false,
        }
    }

    async fn run(&self, request: &Request<'_>) -> anyhow::Result<Outcome> {
        run(
            request,
            &Stores {
                handoff: self.handoff.as_ref(),
                curated: self.curated.as_ref(),
                reference: self.reference.as_ref(),
                out: Some(self.out.as_ref()),
                state: &self.state,
            },
        )
        .await
    }

    /// Nothing was reserved or written for the page.
    async fn assert_untouched(&self) {
        let doc = self.parent.job.message.key().unwrap().doc_id();
        assert!(ledger::load(self.curated.as_ref())
            .await
            .unwrap()
            .outstanding(&doc)
            .is_none());
        assert!(self
            .handoff
            .list("dispatch/retries")
            .await
            .unwrap()
            .is_empty());
        let jobs = self.handoff.list("dispatch/jobs").await.unwrap();
        assert!(jobs.iter().all(|p| p.contains(self.id())), "{jobs:?}");
    }

    async fn refused(&self, request: &Request<'_>, why: &str) {
        let error = format!("{:#}", self.run(request).await.unwrap_err());
        assert!(error.contains(why), "wanted {why:?} in {error:?}");
        self.assert_untouched().await;
    }
}

#[tokio::test]
async fn an_archived_journal_without_terminal_fields_is_retried_from_the_database_proof() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let before = std::fs::read(archive.join("jobs").join(fx.id()).join("record.json")).unwrap();
    let request = Request {
        target_hosts: &[1, 9],
        ..fx.request(Some(&archive))
    };
    let Outcome::Prepared { child, exported } = fx.run(&request).await.unwrap() else {
        panic!("expected a prepared child");
    };
    assert!(exported);
    // The child is in the hand-off for the server to pull, with its page and request.
    let prefix = format!("dispatch/jobs/{child}");
    let record: Record = serde_json::from_slice(
        &fx.handoff
            .get(&format!("{prefix}/record.json"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(record.stage, Stage::Reserved);
    assert_eq!(record.job.engine.image, fx.image);
    assert_eq!(record.job.engine.timeout_secs, 3600);
    assert_eq!(record.job.retry_of.as_deref(), Some(fx.id()));
    assert_eq!(
        record.retry.as_ref().unwrap().provenance,
        Some(Provenance {
            retry_of: fx.id().into(),
            parent_image: fx.parent.job.engine.image.clone(),
            reason: Reason::SlowClass,
            target_hosts: vec![1, 9],
        })
    );
    assert_eq!(record.retry.as_ref().unwrap().app_version_num, 106);
    assert_eq!(record.retry.as_ref().unwrap().terminal.error_mask, Some(2));
    assert_eq!(
        fx.handoff.get(&format!("{prefix}/page")).await.unwrap(),
        Some(fx.page.clone())
    );
    assert!(fx
        .handoff
        .exists(&format!("{prefix}/request.json"))
        .await
        .unwrap());
    // The archive is read, never written.
    assert_eq!(
        std::fs::read(archive.join("jobs").join(fx.id()).join("record.json")).unwrap(),
        before
    );
    // A repeat finds the child instead of reserving another.
    assert_eq!(
        fx.run(&request).await.unwrap(),
        Outcome::Prepared {
            child,
            exported: false
        }
    );
}

#[tokio::test]
async fn a_journal_in_the_live_handoff_needs_no_archive() {
    let fx = Fx::new().await;
    fx.refused(&fx.request(None), "no journal").await;
    fx.put_live().await;
    let request = Request {
        reason: Reason::SegfaultOnce,
        timeout_secs: None,
        ..fx.request(None)
    };
    let Outcome::Prepared { child, .. } = fx.run(&request).await.unwrap() else {
        panic!("expected a prepared child");
    };
    let record: Record = serde_json::from_slice(
        &fx.handoff
            .get(&format!("dispatch/jobs/{child}/record.json"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    // Same settings: the parent's deadline, the current image.
    assert_eq!(
        record.job.engine.timeout_secs,
        fx.parent.job.engine.timeout_secs
    );
    assert_eq!(
        record.retry.unwrap().provenance.unwrap().reason,
        Reason::SegfaultOnce
    );
}

#[tokio::test]
async fn a_dry_run_checks_and_reserves_nothing() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let request = Request {
        dry_run: true,
        target_hosts: &[2],
        ..fx.request(Some(&archive))
    };
    assert_eq!(fx.run(&request).await.unwrap(), Outcome::Checked);
    fx.assert_untouched().await;
}

#[tokio::test]
async fn a_page_whose_claim_ended_finished_is_refused() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let queued = fx.parent.queued().unwrap();
    for (ended, ok) in [
        (Some("finished"), false),
        (Some("dead_lettered"), true),
        (None, true),
    ] {
        let claim = crate::claim::Claim {
            v: crate::claim::VERSION,
            queue: usnm_store::queue::BULK.into(),
            message_id: "m".into(),
            pop_receipt: "r".into(),
            dequeue_count: 2,
            text: fx.parent.job.message.encode(),
            claimed_at: Utc::now().to_rfc3339(),
            lease_until: Utc::now().to_rfc3339(),
            ledger: crate::LedgerRow::from_queued(&queued),
            ended: ended.map(str::to_owned),
        };
        fx.handoff
            .put(
                &crate::claim::path(&queued.doc_id),
                serde_json::to_vec(&claim).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        let request = Request {
            dry_run: true,
            ..fx.request(Some(&archive))
        };
        if ok {
            assert_eq!(fx.run(&request).await.unwrap(), Outcome::Checked);
        } else {
            fx.refused(&request, "ended finished").await;
        }
    }
}

#[tokio::test]
async fn a_page_with_a_canonical_result_anywhere_is_refused() {
    let doc = |fx: &Fx| fx.parent.job.message.key().unwrap().doc_id();
    // The BOINC database has a canonical result.
    let fx = Fx::new().await;
    fx.put_proof(NONCE, Utc::now(), 2, 31337).await;
    fx.refused(&fx.request(Some(&fx.archive())), "canonical result 31337")
        .await;
    // The server's spool holds result files.
    let fx = Fx::new().await;
    fx.handoff
        .put(
            &format!("spool/{}/{}__file_result.json", fx.id(), fx.workunit()),
            b"{}".to_vec(),
            "application/json",
        )
        .await
        .unwrap();
    fx.refused(&fx.request(Some(&fx.archive())), "spool holds output")
        .await;
    // The hand-off kept an accepted result.
    let fx = Fx::new().await;
    fx.handoff
        .put(
            &format!("dispatch/jobs/{}/accepted/result.json", fx.id()),
            b"{}".to_vec(),
            "application/json",
        )
        .await
        .unwrap();
    fx.refused(&fx.request(Some(&fx.archive())), "accepted output")
        .await;
    // The ledger says the page was imported.
    let fx = Fx::new().await;
    let at = Utc::now() + chrono::Duration::hours(1);
    ledger::write_outcomes(
        fx.curated.as_ref(),
        &[ledger::Outcome {
            doc_id: doc(&fx),
            outcome: ledger::IMPORTED.into(),
            cause: None,
            kind: None,
            source: "pages/x.parquet".into(),
            at,
            run: "other".into(),
            message_sha256: None,
        }],
        at,
        "other",
    )
    .await
    .unwrap();
    let error = format!(
        "{:#}",
        fx.run(&fx.request(Some(&fx.archive()))).await.unwrap_err()
    );
    assert!(error.contains("was imported"), "{error}");
    // A published ALTO for the page exists.
    let fx = Fx::new().await;
    fx.out
        .put(
            &format!("alto/{}.boinc-abcdef012345.stamp.xml", doc(&fx)),
            ALTO.as_bytes().to_vec(),
            "application/xml",
        )
        .await
        .unwrap();
    fx.refused(&fx.request(Some(&fx.archive())), "published ALTO")
        .await;
}

#[tokio::test]
async fn terminal_failure_must_be_proved_by_the_database() {
    fn archive_request<'a>(fx: &'a Fx, archive: &'a Path) -> Request<'a> {
        Request {
            dry_run: true,
            ..fx.request(Some(archive))
        }
    }
    // No error mask: it did not fail for good.
    let fx = Fx::new().await;
    fx.put_proof(NONCE, Utc::now(), 0, 0).await;
    let archive = fx.archive();
    fx.refused(&archive_request(&fx, &archive), "did not fail for good")
        .await;
    // Another run's proof, and a stale one.
    let fx = Fx::new().await;
    fx.put_proof(&"f".repeat(32), Utc::now(), 2, 0).await;
    let archive = fx.archive();
    fx.refused(&archive_request(&fx, &archive), "another run")
        .await;
    fx.put_proof(NONCE, Utc::now() - chrono::Duration::hours(2), 2, 0)
        .await;
    fx.refused(&archive_request(&fx, &archive), "run wu-terminal again")
        .await;
    // No proof at all.
    fx.remove(WU_PROOF_PATH);
    fx.refused(&archive_request(&fx, &archive), "wu-terminal")
        .await;
    // A workunit the proof does not list.
    let fx = Fx::new().await;
    let other = serde_json::json!({
        "v": 1, "nonce": NONCE, "checked_at": Utc::now().to_rfc3339(),
        "workunits": [{"name": format!("usnm_{}", "9".repeat(64)), "id": 1, "error_mask": 2,
                       "canonical_resultid": 0, "assimilate_state": 2}],
    });
    fx.handoff
        .put(
            WU_PROOF_PATH,
            serde_json::to_vec(&other).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let archive = fx.archive();
    fx.refused(&archive_request(&fx, &archive), "has no workunit")
        .await;
    // The assimilator's own marker agrees with the database, or the retry is refused.
    let fx = Fx::new().await;
    let archive = fx.archive();
    let marker = |body: &[u8]| {
        let fx = &fx;
        let body = body.to_vec();
        async move {
            fx.handoff
                .put(
                    &format!("spool/{}/{}_error", fx.id(), fx.workunit()),
                    body,
                    "text/plain",
                )
                .await
                .unwrap();
        }
    };
    marker(b"0x4\n").await;
    fx.refused(&archive_request(&fx, &archive), "disagree")
        .await;
    marker(b"0x2\n").await;
    assert_eq!(
        fx.run(&archive_request(&fx, &archive)).await.unwrap(),
        Outcome::Checked
    );
    // A no-output-files marker is another disposition.
    fx.handoff
        .put(
            &format!("spool/{}/{}_no_output_files", fx.id(), fx.workunit()),
            b"".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    fx.refused(&archive_request(&fx, &archive), "no-output-files")
        .await;
}

#[tokio::test]
async fn only_a_failed_single_page_journal_is_retried() {
    for stage in [
        Stage::Prepared,
        Stage::Reserved,
        Stage::Submitting,
        Stage::Published,
    ] {
        let mut fx = Fx::new().await;
        fx.parent.stage = stage;
        let archive = fx.archive();
        fx.refused(&fx.request(Some(&archive)), "only a failed job")
            .await;
    }
    let mut fx = Fx::new().await;
    fx.parent.bundle = Some("b".repeat(64));
    let archive = fx.archive();
    fx.refused(&fx.request(Some(&archive)), "single-page").await;
    // The page must be the request's scan.
    let mut fx = Fx::new().await;
    fx.page = b"another scan".to_vec();
    let archive = fx.archive();
    fx.refused(&fx.request(Some(&archive)), "differs from its request")
        .await;
    // A journal for another job is not found.
    let fx = Fx::new().await;
    let archive = fx.archive();
    let other = "7".repeat(64);
    let error = parent_files(
        &Request {
            parent: &other,
            ..fx.request(Some(&archive))
        },
        fx.handoff.as_ref(),
    )
    .await
    .err()
    .unwrap();
    assert!(format!("{error:#}").contains("no journal"));
}

#[tokio::test]
async fn target_hosts_must_be_golded_for_the_childs_image() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let with = |hosts: &'static [u64]| Request {
        target_hosts: hosts,
        ..fx.request(Some(&archive))
    };
    // 3 has no gold report; one bad host refuses the whole list.
    fx.refused(&with(&[1, 3]), "host 3 is not golded").await;
    fx.refused(&with(&[3]), "host 3 is not golded").await;
    // A host golded for the parent's image only is not golded for the child's.
    let old = fx.parent.job.engine.image.clone();
    let list = serde_json::json!({
        "hosts": {old: [1, 2], fx.image.clone(): [2]},
        "manifest_sha256": "b".repeat(64),
        "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
        "max_cer": 0.08734328574534386, "v": 4
    });
    fx.handoff
        .put(
            "gold/trust.json",
            serde_json::to_vec(&list).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    fx.refused(&with(&[1, 2]), "host 1 is not golded").await;
    // A held host is refused.
    let list = serde_json::json!({
        "hosts": {fx.image.clone(): [1, 2]},
        "manifest_sha256": "b".repeat(64),
        "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
        "max_cer": 0.08734328574534386, "v": 4,
        "holds": {fx.image.clone(): {"2": {"reason": "spot check", "job": "a".repeat(64),
                                           "peer": 1, "at": "2026-11-01T00:00:00Z"}}}
    });
    fx.handoff
        .put(
            "gold/trust.json",
            serde_json::to_vec(&list).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    fx.refused(&with(&[2]), "held").await;
    // No trust list, no targeting.
    fx.remove("gold/trust.json");
    fx.refused(&with(&[1]), "gold/trust.json").await;
    // With no hosts the pool is allowed and trust is not consulted.
    let request = Request {
        dry_run: true,
        ..fx.request(Some(&archive))
    };
    assert_eq!(fx.run(&request).await.unwrap(), Outcome::Checked);
}

#[tokio::test]
async fn the_deadline_follows_the_reason() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let with = |reason, timeout_secs| Request {
        reason,
        timeout_secs,
        dry_run: true,
        ..fx.request(Some(&archive))
    };
    // Slow-class: a deadline, longer than the parent's 1200 s, up to 3600 s.
    fx.refused(&with(Reason::SlowClass, None), "needs --timeout-secs")
        .await;
    fx.refused(
        &with(Reason::SlowClass, Some(1200)),
        "longer than the parent's",
    )
    .await;
    fx.refused(
        &with(Reason::SlowClass, Some(600)),
        "longer than the parent's",
    )
    .await;
    fx.refused(&with(Reason::SlowClass, Some(3601)), "1 to 3600")
        .await;
    assert_eq!(
        fx.run(&with(Reason::SlowClass, Some(3600))).await.unwrap(),
        Outcome::Checked
    );
    // Segfault-once: the same settings.
    fx.refused(
        &with(Reason::SegfaultOnce, Some(3600)),
        "keeps the parent's settings",
    )
    .await;
    assert_eq!(
        fx.run(&with(Reason::SegfaultOnce, None)).await.unwrap(),
        Outcome::Checked
    );
    let mut second = fx.parent.job.clone();
    second.attempt = 1;
    assert!(child_timeout(Reason::SegfaultOnce, &second, None).is_err());
}

#[tokio::test]
async fn requests_are_checked_before_anything_is_read() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let base = fx.request(Some(&archive));
    let bad = [
        Request {
            parent: "xyz",
            ..fx.request(None)
        },
        Request {
            image: "sha256:short",
            ..fx.request(None)
        },
        Request {
            image: "registry/worker:latest",
            ..fx.request(None)
        },
        Request {
            app_version_num: 0,
            ..fx.request(None)
        },
        Request {
            nonce: "nonce",
            ..fx.request(None)
        },
        Request {
            target_hosts: &[2, 1],
            ..fx.request(None)
        },
        Request {
            target_hosts: &[1, 1],
            ..fx.request(None)
        },
        Request {
            target_hosts: &[0],
            ..fx.request(None)
        },
    ];
    for request in bad {
        assert!(check_request(&request).is_err());
    }
    assert!(check_request(&base).is_ok());
}

#[tokio::test]
async fn the_provenance_is_published_with_the_page_and_never_dropped() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let request = Request {
        target_hosts: &[1],
        ..fx.request(Some(&archive))
    };
    let Outcome::Prepared { child, .. } = fx.run(&request).await.unwrap() else {
        panic!("expected a prepared child");
    };
    let record: Record = serde_json::from_slice(
        &fx.handoff
            .get(&format!("dispatch/jobs/{child}/record.json"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    // What `import` publishes for the child carries the note after the scan's own identifiers.
    let published = crate::published_alto(&record, ALTO.as_bytes()).unwrap();
    for needle in [
        format!(
            r#"<fileIdentifier fileIdentifierLocation="usnm_retry_of">{}</fileIdentifier>"#,
            fx.id()
        ),
        format!(
            r#"<fileIdentifier fileIdentifierLocation="usnm_parent_image">{}</fileIdentifier>"#,
            fx.parent.job.engine.image
        ),
        r#"<fileIdentifier fileIdentifierLocation="usnm_retry_reason">slow-class</fileIdentifier>"#
            .to_string(),
    ] {
        assert!(published.contains(&needle), "{needle} in {published}");
    }
    usnm_ocr_local::checks::check_alto(published.as_bytes()).unwrap();
    // An ordinary page's ALTO has no note, and an ALTO that cannot carry one is refused.
    let published = crate::published_alto(&fx.parent, ALTO.as_bytes()).unwrap();
    assert!(!published.contains("usnm_retry_of"));
    assert!(crate::published_alto(&record, b"<alto xmlns=\"http://www.loc.gov/standards/alto/ns-v3#\"><Layout><Page WIDTH=\"1\" HEIGHT=\"1\"><PrintSpace/></Page></Layout></alto>").is_err());
    // The journal's note is checked wherever it is read.
    let mut changed = record.clone();
    changed
        .retry
        .as_mut()
        .unwrap()
        .provenance
        .as_mut()
        .unwrap()
        .retry_of = "e".repeat(64);
    assert!(crate::retry::validate(&changed).is_err());
    let mut changed = record.clone();
    changed
        .retry
        .as_mut()
        .unwrap()
        .provenance
        .as_mut()
        .unwrap()
        .parent_image = "image".into();
    assert!(crate::retry::validate(&changed).is_err());
    let mut changed = record.clone();
    changed
        .retry
        .as_mut()
        .unwrap()
        .provenance
        .as_mut()
        .unwrap()
        .target_hosts = vec![2, 1];
    assert!(crate::retry::validate(&changed).is_err());
    assert!(crate::retry::validate(&record).is_ok());
}

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: ControllerRetryArgs,
}

#[test]
fn the_command_takes_named_flags_and_bounds_its_numbers() {
    let base = [
        "x",
        "--job",
        "j",
        "--nonce",
        "n",
        "--image",
        "sha256:i",
        "--app-version-num",
        "106",
        "--reason",
        "slow-class",
        "--reviewed-reason",
        "r",
        "--handoff-url",
        "h",
        "--curated-url",
        "c",
        "--reference-url",
        "f",
        "--cosmos",
        "https://c/",
    ];
    let parse = |extra: &[&str]| Cli::try_parse_from([&base[..], extra].concat());
    let cli = parse(&[
        "--timeout-secs",
        "3600",
        "--target-host",
        "1",
        "--target-host",
        "9",
    ])
    .unwrap();
    assert_eq!(cli.args.target_hosts, [1, 9]);
    assert_eq!(cli.args.timeout_secs, Some(3600));
    assert_eq!(cli.args.reason, Reason::SlowClass);
    for bad in [
        &["--timeout-secs", "3601"][..],
        &["--timeout-secs", "0"],
        &["--target-host", "0"],
        &["--target-host", "x"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
    let mut other = base.to_vec();
    other[10] = "whenever";
    assert!(Cli::try_parse_from(other).is_err());
    // No positional arguments.
    assert!(parse(&["positional"]).is_err());
}
