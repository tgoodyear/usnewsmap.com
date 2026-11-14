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
    /// The published outputs as a local store keeps them, and as Blob Storage lists them.
    out: Arc<dyn ObjectStore>,
    out_named: ByPrefix,
    state: State,
    parent: Record,
    page: Vec<u8>,
    image: String,
}

impl Fx {
    async fn new() -> Self {
        Self::build(false, true, None).await
    }

    /// A queue-fed parent: its journal holds the ledger row its claim carries.
    async fn queue_fed() -> Self {
        Self::build(true, true, None).await
    }

    /// `dead_lettered` is whether the ledger holds the parent's permanent failure.
    /// `outcome_at` is when it settled, by default a minute after the parent's row.
    async fn build(
        queue_fed: bool,
        dead_lettered: bool,
        outcome_at: Option<DateTime<Utc>>,
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let open = |name: &str| usnm_store::open(temp.path().join(name).to_str().unwrap()).unwrap();
        let (handoff, curated, reference, out) = (
            open("handoff"),
            open("curated"),
            open("reference"),
            open("out"),
        );
        let out_named = ByPrefix(open("out"));
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
        let mut parent = Record {
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
            origin: None,
        };
        let previous = parent.queued().unwrap();
        if queue_fed {
            parent.ledger = Some(crate::LedgerRow::from_queued(&previous));
        }
        ledger::write_queued(
            curated.as_ref(),
            std::slice::from_ref(&previous),
            previous.queued_at,
            "prior",
        )
        .await
        .unwrap();
        let at = outcome_at.unwrap_or(previous.queued_at + chrono::Duration::minutes(1));
        if dead_lettered {
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
        }
        let fx = Self {
            temp,
            handoff,
            curated,
            reference,
            out,
            out_named,
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
        self.run_with(request, true).await
    }

    async fn run_with(
        &self,
        request: &Request<'_>,
        by_name_prefix: bool,
    ) -> anyhow::Result<Outcome> {
        run(
            request,
            &Stores {
                handoff: self.handoff.as_ref(),
                curated: self.curated.as_ref(),
                reference: self.reference.as_ref(),
                out: Out {
                    store: if by_name_prefix {
                        &self.out_named
                    } else {
                        self.out.as_ref()
                    },
                    by_name_prefix,
                },
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
    let archive = fx.archive();
    fx.refused(&fx.request(Some(&archive)), "published ALTO")
        .await;
    // A local output store lists a directory, not a name prefix: the check lists the directory.
    let error = fx
        .run_with(&fx.request(Some(&archive)), false)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("published ALTO"), "{error:#}");
    fx.assert_untouched().await;
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
async fn every_import_carries_its_image_job_host_and_platform_and_a_retry_keeps_its_note() {
    use crate::{host_trust::Family, Origin};
    let fx = Fx::new().await;
    let tag = |location: &str, value: &str| {
        format!(r#"<fileIdentifier fileIdentifierLocation="{location}">{value}</fileIdentifier>"#)
    };
    // An ordinary page, with a host and its family.
    let mut page = fx.parent.clone();
    page.origin = Some(Origin {
        host: Some(77),
        platform_family: Some(Family::Arm64.as_str().to_owned()),
    });
    let published = crate::published_alto(&page, ALTO.as_bytes()).unwrap();
    for needle in [
        tag("usnm_engine_image", &page.job.engine.image),
        tag("usnm_job_id", &page.job.id),
        tag("usnm_boinc_host", "77"),
        tag("usnm_platform_family", "arm64"),
    ] {
        assert!(published.contains(&needle), "{needle} in {published}");
    }
    assert!(!published.contains("usnm_retry_of"));
    usnm_ocr_local::checks::check_alto(published.as_bytes()).unwrap();
    // The same record publishes the same bytes.
    assert_eq!(
        published,
        crate::published_alto(&page, ALTO.as_bytes()).unwrap()
    );
    // No host: still published, with the image and job id and no host or family entry.
    page.origin = None;
    let published = crate::published_alto(&page, ALTO.as_bytes()).unwrap();
    assert!(published.contains(&tag("usnm_engine_image", &page.job.engine.image)));
    assert!(published.contains(&tag("usnm_job_id", &page.job.id)));
    assert!(!published.contains("usnm_boinc_host"));
    assert!(!published.contains("usnm_platform_family"));
    usnm_ocr_local::checks::check_alto(published.as_bytes()).unwrap();
    // A retry keeps its own note beside the origin.
    let archive = fx.archive();
    let request = Request {
        target_hosts: &[1],
        ..fx.request(Some(&archive))
    };
    let Outcome::Prepared { child, .. } = fx.run(&request).await.unwrap() else {
        panic!("expected a prepared child");
    };
    let mut record: Record = serde_json::from_slice(
        &fx.handoff
            .get(&format!("dispatch/jobs/{child}/record.json"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    record.origin = Some(Origin {
        host: Some(5),
        platform_family: None,
    });
    let published = crate::published_alto(&record, ALTO.as_bytes()).unwrap();
    for needle in [
        tag("usnm_retry_of", fx.id()),
        tag("usnm_parent_image", &fx.parent.job.engine.image),
        tag("usnm_retry_reason", "slow-class"),
        tag("usnm_engine_image", &record.job.engine.image),
        tag("usnm_job_id", &record.job.id),
        tag("usnm_boinc_host", "5"),
    ] {
        assert!(published.contains(&needle), "{needle} in {published}");
    }
    usnm_ocr_local::checks::check_alto(published.as_bytes()).unwrap();
}

#[test]
fn set_origin_saves_the_host_before_the_put_and_keeps_what_it_is_not_given() {
    use crate::host_trust::Family;
    let temp = tempfile::tempdir().unwrap();
    let job = crate::bundle::work::tests::job(1);
    let mut record = Record {
        job: job.clone(),
        plan: String::new(),
        stage: crate::Stage::Prepared,
        stamp: "s".into(),
        error: None,
        terminal: None,
        retry: None,
        ledger: None,
        bundle: None,
        unbundled: None,
        held: None,
        origin: None,
    };
    std::fs::create_dir_all(crate::dir(temp.path(), &job.id).unwrap()).unwrap();
    crate::save(temp.path(), &record).unwrap();
    crate::set_origin(temp.path(), &job.id, Some(9), Some(Family::Amd64)).unwrap();
    crate::set_origin(temp.path(), &job.id, None, None).unwrap();
    crate::set_origin(temp.path(), &job.id, Some(0), None).unwrap();
    let saved = crate::load(temp.path(), &job.id).unwrap().origin.unwrap();
    assert_eq!(saved.host, Some(9));
    assert_eq!(saved.platform_family.as_deref(), Some("amd64"));
    // A published record is left alone.
    record.stage = crate::Stage::Published;
    crate::save(temp.path(), &record).unwrap();
    crate::set_origin(temp.path(), &job.id, Some(10), None).unwrap();
    assert!(crate::load(temp.path(), &job.id).unwrap().origin.is_none());
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
        "--out-url",
        "o",
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

#[tokio::test]
async fn a_dry_run_makes_the_checks_a_real_run_would() {
    // A queue-fed parent's claim must be conclusively dead-lettered.
    let fx = Fx::queue_fed().await;
    let archive = fx.archive();
    let request = Request {
        dry_run: true,
        ..fx.request(Some(&archive))
    };
    fx.refused(&request, "missing parent claim").await;
    let queued = fx.parent.queued().unwrap();
    for (ended, ok) in [
        (None, false),
        (Some("lease_lost"), false),
        (Some("dead_letter_lease_lost"), false),
        (Some("dead_lettered"), true),
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
        if ok {
            assert_eq!(fx.run(&request).await.unwrap(), Outcome::Checked);
        } else {
            fx.refused(&request, "not conclusively deleted").await;
        }
    }
    // The ledger must hold the parent's permanent failure as the page's latest outcome.
    let fx = Fx::build(false, false, None).await;
    let archive = fx.archive();
    let request = Request {
        dry_run: true,
        ..fx.request(Some(&archive))
    };
    // (The page is outstanding there, so only the hand-off is checked for what a retry writes.)
    let error = format!("{:#}", fx.run(&request).await.unwrap_err());
    assert!(error.contains("no settled outcome"), "{error}");
    assert!(fx.handoff.list("dispatch").await.unwrap().is_empty());
}

#[tokio::test]
async fn an_interrupted_retry_is_made_again_as_it_was() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let request = Request {
        target_hosts: &[2],
        ..fx.request(Some(&archive))
    };
    let Outcome::Prepared { child, .. } = fx.run(&request).await.unwrap() else {
        panic!("expected a prepared child");
    };
    // The run stopped after the reservation: no child in the hand-off, no index. The scratch
    // root went with it, so only the intent and the ledger row remain.
    for path in [
        format!("dispatch/jobs/{child}/record.json"),
        format!("dispatch/jobs/{child}/page"),
        format!("dispatch/jobs/{child}/request.json"),
        format!("dispatch/retries/{}.json", fx.id()),
    ] {
        fx.remove(&path);
    }
    // Other settings would be another child on the same reservation: refused.
    let other = Request {
        target_hosts: &[1],
        ..fx.request(Some(&archive))
    };
    let error = format!("{:#}", fx.run(&other).await.unwrap_err());
    assert!(error.contains("other settings"), "{error}");
    assert!(!fx
        .handoff
        .exists(&format!("dispatch/jobs/{child}/record.json"))
        .await
        .unwrap());
    // The same settings make the same child, with all its files.
    assert_eq!(
        fx.run(&request).await.unwrap(),
        Outcome::Prepared {
            child: child.clone(),
            exported: true
        }
    );
    for file in ["record.json", "page", "request.json"] {
        assert!(
            fx.handoff
                .exists(&format!("dispatch/jobs/{child}/{file}"))
                .await
                .unwrap(),
            "{file}"
        );
    }
    let request_json: Job = serde_json::from_slice(
        &fx.handoff
            .get(&format!("dispatch/jobs/{child}/request.json"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(request_json.id, child);
    // The index is back, so a third run finds the child.
    assert_eq!(
        fx.run(&request).await.unwrap(),
        Outcome::Prepared {
            child,
            exported: false
        }
    );
}

#[tokio::test]
async fn an_intent_that_reserved_nothing_is_replaced() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    fx.handoff
        .put(
            &format!("dispatch/retries/{}.intent.json", fx.id()),
            serde_json::to_vec(&serde_json::json!({
                "queued_at": "2026-11-13T00:00:00.000000Z", "request": "0".repeat(64)
            }))
            .unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    assert!(matches!(
        fx.run(&fx.request(Some(&archive))).await.unwrap(),
        Outcome::Prepared { .. }
    ));
}

#[tokio::test]
async fn the_live_parent_is_made_failed_for_the_controller_and_never_overwritten() {
    let record = |fx: &Fx| {
        let path = fx
            .temp
            .path()
            .join("handoff")
            .join(format!("dispatch/jobs/{}/record.json", fx.id()));
        serde_json::from_slice::<Record>(&std::fs::read(path).unwrap()).unwrap()
    };
    // An archive-only parent is written to the live hand-off, failed, with its proof.
    let fx = Fx::new().await;
    let archive = fx.archive();
    fx.run(&fx.request(Some(&archive))).await.unwrap();
    let live = record(&fx);
    assert_eq!(live.stage, Stage::Failed);
    assert_eq!(live.terminal.as_ref().unwrap().error_mask, Some(2));
    assert!(live.error.as_ref().unwrap().contains("error mask 0x2"));
    // A live parent still marked submitted is made failed, and nothing else of it changes.
    let mut fx = Fx::new().await;
    fx.parent.stage = Stage::Submitted;
    fx.put_live().await;
    fx.run(&fx.request(None)).await.unwrap();
    let live = record(&fx);
    assert_eq!(live.stage, Stage::Failed);
    assert_eq!(live.job, fx.parent.job);
    assert_eq!(live.plan, fx.parent.plan);
    // Any other live state is refused before a child is reserved.
    for stage in [Stage::Published, Stage::Reserved, Stage::Submitting] {
        let fx = Fx::new().await;
        let archive = fx.archive();
        let mut live = fx.parent.clone();
        live.stage = stage.clone();
        fx.handoff
            .put(
                &format!("dispatch/jobs/{}/record.json", fx.id()),
                serde_json::to_vec_pretty(&live).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        let error = format!(
            "{:#}",
            fx.run(&fx.request(Some(&archive))).await.unwrap_err()
        );
        assert!(error.contains("does not overwrite"), "{stage:?}: {error}");
        assert!(fx
            .handoff
            .list("dispatch/retries")
            .await
            .unwrap()
            .is_empty());
    }
    // Another terminal proof is refused too.
    let fx = Fx::new().await;
    let archive = fx.archive();
    let mut live = fx.parent.clone();
    live.terminal = Some(TerminalFailure::from_marker(Some(b"0x4\n")));
    live.error = Some("failed".into());
    fx.handoff
        .put(
            &format!("dispatch/jobs/{}/record.json", fx.id()),
            serde_json::to_vec_pretty(&live).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let error = format!(
        "{:#}",
        fx.run(&fx.request(Some(&archive))).await.unwrap_err()
    );
    assert!(error.contains("other terminal proof"), "{error}");
}

#[test]
fn the_note_goes_in_the_real_source_image_information_element() {
    let provenance = Provenance {
        retry_of: "a".repeat(64),
        parent_image: format!("sha256:{}", "b".repeat(64)),
        reason: Reason::SegfaultOnce,
        target_hosts: vec![],
    };
    let note = |location: &str, prefix: &str| {
        format!("<{prefix}fileIdentifier fileIdentifierLocation=\"{location}\">")
    };
    // A closing tag in a comment or a CDATA section is not the element's.
    let tricked = r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><!-- </sourceImageInformation> --><Description><sourceImageInformation><fileName>x</fileName></sourceImageInformation></Description></alto>"#;
    let noted = provenance.alto_note(tricked).unwrap();
    let real = noted.rfind("</sourceImageInformation>").unwrap();
    let first = noted.find(&note("usnm_retry_of", "")).unwrap();
    assert!(noted.find("<!--").unwrap() < noted.find("-->").unwrap());
    assert!(
        first > noted.find("-->").unwrap() && first < real,
        "{noted}"
    );
    assert_eq!(noted.matches("usnm_retry_reason").count(), 1);
    let cdata = tricked.replace(
        "<!-- </sourceImageInformation> -->",
        "<![CDATA[</sourceImageInformation>]]>",
    );
    let noted = provenance.alto_note(&cdata).unwrap();
    assert!(noted.find(&note("usnm_retry_of", "")).unwrap() > noted.find("]]>").unwrap());
    // A namespace prefix is kept.
    let prefixed = r#"<alto:alto xmlns:alto="http://www.loc.gov/standards/alto/ns-v3#"><alto:Description><alto:sourceImageInformation><alto:fileName>x</alto:fileName></alto:sourceImageInformation></alto:Description></alto:alto>"#;
    let noted = provenance.alto_note(prefixed).unwrap();
    assert!(
        noted.contains(&note("usnm_parent_image", "alto:")),
        "{noted}"
    );
    assert!(noted.contains("</alto:fileIdentifier></alto:sourceImageInformation>"));
    // No element, only one in a comment, or an empty one: refused.
    for alto in [
        "<alto/>",
        "<alto><!-- </sourceImageInformation> --></alto>",
        "<alto><sourceImageInformation/></alto>",
        "<alto><sourceImageInformation>",
    ] {
        assert!(provenance.alto_note(alto).is_err(), "{alto}");
    }
}

#[tokio::test]
async fn a_failure_settled_ahead_of_the_clock_is_resumed_at_the_reserved_time() {
    // The reservation moves the child past the parent's outcome; the intent holds that time, so
    // a repeat after an interruption finds the ledger row it made.
    let fx = Fx::build(false, true, Some(Utc::now() + chrono::Duration::hours(2))).await;
    let archive = fx.archive();
    let request = fx.request(Some(&archive));
    let Outcome::Prepared { child, .. } = fx.run(&request).await.unwrap() else {
        panic!("expected a prepared child");
    };
    let intent: Intent = serde_json::from_slice(
        &fx.handoff
            .get(&format!("dispatch/retries/{}.intent.json", fx.id()))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let doc = fx.parent.job.message.key().unwrap().doc_id();
    let row = ledger::load(fx.curated.as_ref()).await.unwrap().queued[&doc].clone();
    assert_eq!(
        row.queued_at
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        intent.queued_at
    );
    for file in ["record.json", "page", "request.json"] {
        fx.remove(&format!("dispatch/jobs/{child}/{file}"));
    }
    fx.remove(&format!("dispatch/retries/{}.json", fx.id()));
    assert_eq!(
        fx.run(&request).await.unwrap(),
        Outcome::Prepared {
            child,
            exported: true
        }
    );
}

#[tokio::test]
async fn an_interrupted_export_is_repaired_and_never_overwrites_an_advanced_journal() {
    let fx = Fx::new().await;
    let archive = fx.archive();
    let request = fx.request(Some(&archive));
    let Outcome::Prepared { child, .. } = fx.run(&request).await.unwrap() else {
        panic!("expected a prepared child");
    };
    let record = format!("dispatch/jobs/{child}/record.json");
    let index = format!("dispatch/retries/{}.json", fx.id());
    // The journal goes last: stopped after the page, there is none and no index.
    fx.remove(&record);
    fx.remove(&format!("dispatch/jobs/{child}/request.json"));
    fx.remove(&index);
    assert!(matches!(
        fx.run(&request).await.unwrap(),
        Outcome::Prepared { exported: true, .. }
    ));
    // A journal with a file missing (another tool's partial copy) is repaired, and the server's
    // later state of the journal stays.
    let mut advanced: Record =
        serde_json::from_slice(&fx.handoff.get(&record).await.unwrap().unwrap()).unwrap();
    advanced.stage = Stage::Submitted;
    let advanced = serde_json::to_vec_pretty(&advanced).unwrap();
    fx.handoff
        .put(&record, advanced.clone(), "application/json")
        .await
        .unwrap();
    fx.remove(&format!("dispatch/jobs/{child}/request.json"));
    fx.remove(&index);
    assert_eq!(
        fx.run(&request).await.unwrap(),
        Outcome::Prepared {
            child: child.clone(),
            exported: false
        }
    );
    assert_eq!(fx.handoff.get(&record).await.unwrap(), Some(advanced));
    assert!(fx
        .handoff
        .exists(&format!("dispatch/jobs/{child}/request.json"))
        .await
        .unwrap());
    assert!(fx.handoff.exists(&index).await.unwrap());
}

#[test]
fn only_altos_own_source_image_information_takes_the_note() {
    let provenance = Provenance {
        retry_of: "a".repeat(64),
        parent_image: format!("sha256:{}", "b".repeat(64)),
        reason: Reason::SlowClass,
        target_hosts: vec![],
    };
    // An empty ALTO element, with a foreign one of the same name in foreign XML: refused.
    let foreign = r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><sourceImageInformation/><Processing><x:sourceImageInformation xmlns:x="urn:foreign"><fileName/></x:sourceImageInformation></Processing></Description></alto>"#;
    assert!(provenance.alto_note(foreign).is_err());
    // Foreign elements of that name inside the real element's neighbours are skipped.
    let both = r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><Processing xmlns:x="urn:foreign"><x:sourceImageInformation><x:fileName/></x:sourceImageInformation></Processing><sourceImageInformation><fileName>x</fileName></sourceImageInformation></Description></alto>"#;
    let noted = provenance.alto_note(both).unwrap();
    let first = noted.find("usnm_retry_of").unwrap();
    assert!(
        first > noted.find("</x:sourceImageInformation>").unwrap(),
        "{noted}"
    );
    assert!(noted.ends_with("</fileIdentifier></sourceImageInformation></Description></alto>"));
    // Not in an ALTO namespace, or not where ALTO has it.
    for alto in [
        r#"<alto><Description><sourceImageInformation><fileName/></sourceImageInformation></Description></alto>"#,
        r#"<alto xmlns="urn:other"><Description><sourceImageInformation><fileName/></sourceImageInformation></Description></alto>"#,
        r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><sourceImageInformation><fileName/></sourceImageInformation></alto>"#,
    ] {
        assert!(provenance.alto_note(alto).is_err(), "{alto}");
    }
}
