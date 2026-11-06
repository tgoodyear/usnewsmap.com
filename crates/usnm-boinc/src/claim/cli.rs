//! The `usnm-boinc claim` command line (#296): its clap definitions and the dispatch to
//! this module's functions. `main.rs` only names the group.
use anyhow::Context;
use clap::Subcommand;
use std::{path::PathBuf, sync::Arc};
use usnm_state::{
    docs::{DocStore, FileDocs},
    state::State,
};

#[derive(Subcommand)]
// Parsed once per run; boxing the arguments would only add noise.
#[allow(clippy::large_enum_variant)]
pub enum ClaimCommands {
    /// Hand-off job: take usable pages off the work queues, writing one claim file each
    /// and a summary. Messages that can't be used here go back for the nodes.
    Pull {
        /// The exchange account's queue endpoint (https://<account>.queue.core.windows.net).
        #[arg(long)]
        queue_url: String,
        /// The exchange account's ocr-out, where the nodes' fetched-scans lists are.
        #[arg(long)]
        out_url: String,
        #[arg(long)]
        curated_url: String,
        #[arg(long)]
        reference_url: String,
        /// The prefetch manifest version the cohort uses (the fetched list is per version).
        #[arg(long)]
        manifest_version: String,
        /// A queue to take from; repeat for more, in order: the first fills the claims it
        /// can, the next fills the rest. Default: ocr-bulk only.
        #[arg(long = "queue", default_value = "ocr-bulk")]
        queues: Vec<String>,
        #[arg(long, default_value_t = 10)]
        max: usize,
        /// Most messages received from each queue, usable or not (default: three times
        /// --max). Each queue has its own budget (#1057).
        #[arg(long)]
        max_receives: Option<usize>,
        /// Hours a claimed message stays hidden.
        #[arg(long, default_value_t = 96)]
        lease_hours: u64,
        /// A source language to take (`eng`); repeat for more. Pages in other languages go
        /// back for the nodes. Give the trust calibration's language, so no claimed page is
        /// held back at collect. Default: any language the worker image can read.
        #[arg(long = "language")]
        languages: Vec<String>,
        /// Where the claim files go.
        #[arg(long)]
        out: PathBuf,
        /// Where the summary goes.
        #[arg(long)]
        summary: PathBuf,
        /// Echoed in the summary, so a reader can tell this run's from an older one.
        #[arg(long)]
        nonce: String,
        #[arg(long, required_unless_present = "cosmos", conflicts_with = "cosmos")]
        state_file: Option<PathBuf>,
        #[arg(long)]
        cosmos: Option<String>,
    },
    /// Hand-off job: after `collect` published a job, delete its queue message.
    Finish {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        handoff_url: String,
        #[arg(long)]
        queue_url: String,
    },
    /// Hand-off job: a page whose BOINC workunit failed for good goes to `ocr-deadletter`
    /// and its message is deleted, so it isn't taken again (#1154).
    DeadLetter {
        #[arg(long)]
        handoff_url: String,
        #[arg(long)]
        queue_url: String,
        /// A failed job's id (its journal in the hand-off container's dispatch/ names the page
        /// and why it failed); repeat for more.
        #[arg(
            long = "job",
            required_unless_present = "supersedes_failed_job",
            conflicts_with = "supersedes_failed_job"
        )]
        jobs: Vec<String>,
        /// Operator only (#1417): a dead-lettered job whose page was claimed again with a newer
        /// message. That newer claim is dead-lettered, naming this job, and its message deleted.
        /// A dry run unless --confirm-message-sha256 is given.
        #[arg(long, requires = "curated_url")]
        supersedes_failed_job: Option<String>,
        /// The lake, for the ledger that must show the failed job dead-lettered and the newer
        /// claim outstanding.
        #[arg(long, requires = "supersedes_failed_job")]
        curated_url: Option<String>,
        /// The newer claim's message_sha256, as the dry run prints it: dead-letter it.
        #[arg(long, requires = "supersedes_failed_job")]
        confirm_message_sha256: Option<String>,
    },
    /// Operator only (#1417): a dead-lettered job whose page was claimed again with a newer
    /// message runs again with another page deadline. Records the retry, which the controller
    /// and `export --deadline-retry` need; the queue and the claim are left as they are. A dry
    /// run unless --confirm-message-sha256 is given.
    DeadlineRetry {
        #[arg(long)]
        handoff_url: String,
        /// The lake, for the ledger that must show the failed job dead-lettered and the newer
        /// claim outstanding.
        #[arg(long)]
        curated_url: String,
        /// The dead-lettered job (its journal in the hand-off container's dispatch/).
        #[arg(long)]
        supersedes_failed_job: String,
        /// The new job's page deadline (`engine.timeout_secs`): 1 to 3600, not the failed
        /// job's.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=i64::from(super::MAX_TIMEOUT_SECS)))]
        timeout_secs: u32,
        /// The newer claim's message_sha256, as the dry run prints it: record the retry.
        #[arg(long)]
        confirm_message_sha256: Option<String>,
    },
    /// Hand-off job: give claimed pages back to the queue, for the nodes.
    Release {
        #[arg(long)]
        handoff_url: String,
        #[arg(long)]
        queue_url: String,
        /// A doc id (`sn83045462_1890-05-03_ed-1_seq-7`); repeat for more.
        #[arg(long = "doc-id", required = true)]
        doc_ids: Vec<String>,
    },
}

pub async fn run(command: ClaimCommands) -> anyhow::Result<()> {
    use crate::claim as x;
    match command {
        ClaimCommands::Pull {
            queue_url,
            out_url,
            curated_url,
            reference_url,
            manifest_version,
            queues,
            max,
            max_receives,
            lease_hours,
            languages,
            out,
            summary,
            nonce,
            state_file,
            cosmos,
        } => {
            anyhow::ensure!(
                usnm_store::is_safe_segment(&manifest_version),
                "invalid --manifest-version"
            );
            anyhow::ensure!(
                languages
                    .iter()
                    .all(|l| l.len() == 3 && l.bytes().all(|b| b.is_ascii_lowercase())),
                "--language is a three-letter lower-case code"
            );
            let languages: std::collections::BTreeSet<String> = languages.into_iter().collect();
            let docs: Arc<dyn DocStore> = if let Some(endpoint) = cosmos {
                usnm_state::docs::open_endpoint(&endpoint)?
            } else {
                Arc::new(FileDocs::open(
                    state_file.context("--state-file required")?,
                )?)
            };
            let state = State::new(docs);
            let curated = usnm_store::open(&curated_url)?;
            let reference = usnm_store::open(&reference_url)?;
            let exchange_out = usnm_store::open(&out_url)?;
            let queue = usnm_store::queue::open(&queue_url)?;
            // The lock keeps the ledger as it is while messages are judged against it.
            let held = usnm_ocr_local::boinc::Held::take(&state).await?;
            let mut report = x::Report::default();
            let pulled = async {
                let snapshot =
                    usnm_ocr_local::boinc::Snapshot::load(&state, reference.as_ref()).await?;
                let ledger = usnm_ocr_local::ledger::load(curated.as_ref()).await?;
                let (_, fetched) =
                    usnm_ocr_local::seed::newest_fetched(exchange_out.as_ref(), &manifest_version)
                        .await?;
                let installed = crate::installed_models();
                x::pull(
                    &x::Sources {
                        queue: queue.as_ref(),
                        snapshot: &snapshot,
                        ledger: &ledger,
                        fetched: &fetched,
                        installed: &installed,
                        languages: (!languages.is_empty()).then_some(&languages),
                    },
                    &x::Options {
                        queues,
                        max,
                        max_receives: max_receives.unwrap_or(max * 3).max(max),
                        lease: std::time::Duration::from_secs(lease_hours * 3600),
                    },
                    &out,
                    &mut report,
                )
                .await
            }
            .await;
            let unlocked = held.release(&state).await;
            let mut claims = Vec::new();
            for claim in &report.claims {
                let m = claim.message()?;
                let key = m.key()?;
                claims.push(serde_json::json!({
                    "page": m.page,
                    "doc_id": key.doc_id(),
                    "batch": m.batch,
                    "queue": claim.queue,
                    "scan": format!("keep/{}/{}/{}/{}/{}", m.batch, key.lccn, key.date, key.edition, key.seq),
                    "message_sha256": usnm_ocr_contract::boinc::sha256(claim.text.as_bytes()),
                    "lease_until": claim.lease_until,
                }));
            }
            let body = serde_json::json!({
                "schema_version": 1,
                "nonce": nonce,
                "created_at": chrono::Utc::now().timestamp(),
                "received": report.received,
                "released": report.released,
                "stale": report.stale,
                "claims": claims,
            });
            crate::atomic(&summary, &serde_json::to_vec_pretty(&body)?)?;
            eprintln!(
                "claimed {}, sent back {}, deleted {} answered, of {} received",
                report.claims.len(),
                report.released,
                report.stale,
                report.received
            );
            for q in &report.queues {
                eprintln!(
                    "  {}: claimed {}, sent back {}, deleted {} answered, of {} received",
                    q.queue, q.claimed, q.released, q.stale, q.received
                );
            }
            // The summary says what was taken even when the run then failed.
            pulled?;
            unlocked?;
        }
        ClaimCommands::Finish {
            root,
            job,
            handoff_url,
            queue_url,
        } => {
            let record = crate::load(&root, &job)?;
            let handoff = usnm_store::open(&handoff_url)?;
            let queue = usnm_store::queue::open(&queue_url)?;
            let ended = x::finish(handoff.as_ref(), queue.as_ref(), &record).await?;
            println!("{ended:?}");
        }
        ClaimCommands::DeadLetter {
            handoff_url,
            queue_url,
            jobs,
            supersedes_failed_job,
            curated_url,
            confirm_message_sha256,
        } => {
            let handoff = usnm_store::open(&handoff_url)?;
            let queue = usnm_store::queue::open(&queue_url)?;
            if let Some(job) = supersedes_failed_job {
                if let Some(sha) = &confirm_message_sha256 {
                    anyhow::ensure!(
                        usnm_ocr_contract::boinc::is_sha256(sha),
                        "--confirm-message-sha256 is a sha256 (64 lower-case hex)"
                    );
                }
                let record = failed_record(handoff.as_ref(), &job).await?;
                let curated = usnm_store::open(&curated_url.context("--curated-url required")?)?;
                let ledger = usnm_ocr_local::ledger::load(curated.as_ref()).await?;
                let s = x::dead_letter_superseding(
                    handoff.as_ref(),
                    queue.as_ref(),
                    &record,
                    &ledger,
                    confirm_message_sha256.as_deref(),
                )
                .await?;
                println!(
                    "{} claim from {}: message {} (id {}), queued_at {}, ledger attempt {}; supersedes dead-lettered job {job}",
                    s.doc_id, s.queue, s.message_sha256, s.message_id, s.queued_at, s.attempt
                );
                match s.done {
                    None => println!(
                        "dry run: nothing changed; to dead-letter this message and delete it, give --confirm-message-sha256 {}",
                        s.message_sha256
                    ),
                    Some(x::DeadLettered::Done) => println!("{} dead_lettered", s.doc_id),
                    Some(x::DeadLettered::Already) => println!("{} already ended", s.doc_id),
                    Some(x::DeadLettered::LeaseLost) => anyhow::bail!(
                        "the dead letter went, but {}'s message had come back to its queue; run this again after the next claim pull",
                        s.doc_id
                    ),
                }
                return Ok(());
            }
            let mut lost = 0;
            for job in jobs {
                let record = failed_record(handoff.as_ref(), &job).await?;
                let doc_id = record.job.message.key()?.doc_id();
                if record.retry.is_some() || record.ledger.is_none() {
                    let done = crate::retry::dead_letter(&record, handoff.as_ref(), queue.as_ref())
                        .await?;
                    println!("{doc_id} {done:?}");
                    continue;
                }
                let error = record
                    .error
                    .unwrap_or_else(|| "BOINC workunit failed for good".into());
                let done = x::dead_letter(
                    handoff.as_ref(),
                    queue.as_ref(),
                    &doc_id,
                    &error,
                    Some(&record.job.message),
                )
                .await?;
                if done == x::DeadLettered::LeaseLost {
                    lost += 1;
                }
                println!(
                    "{doc_id} {}",
                    match done {
                        x::DeadLettered::Done => "dead_lettered",
                        x::DeadLettered::Already => "already ended",
                        x::DeadLettered::LeaseLost => "lease_lost",
                    }
                );
            }
            anyhow::ensure!(
                lost == 0,
                "{lost} dead-lettered messages had come back to their queue; delete the copies by hand"
            );
        }
        ClaimCommands::DeadlineRetry {
            handoff_url,
            curated_url,
            supersedes_failed_job: job,
            timeout_secs,
            confirm_message_sha256,
        } => {
            if let Some(sha) = &confirm_message_sha256 {
                anyhow::ensure!(
                    usnm_ocr_contract::boinc::is_sha256(sha),
                    "--confirm-message-sha256 is a sha256 (64 lower-case hex)"
                );
            }
            let handoff = usnm_store::open(&handoff_url)?;
            let record = failed_record(handoff.as_ref(), &job).await?;
            let curated = usnm_store::open(&curated_url)?;
            let ledger = usnm_ocr_local::ledger::load(curated.as_ref()).await?;
            let r = x::deadline_retry(
                handoff.as_ref(),
                &record,
                &ledger,
                timeout_secs,
                confirm_message_sha256.as_deref(),
            )
            .await?;
            println!(
                "{} claim from {}: message {} (id {}), queued_at {}, ledger attempt {}; supersedes dead-lettered job {job}, which ran with a {} s page deadline: {}",
                r.doc_id,
                r.queue,
                r.message_sha256,
                r.message_id,
                r.queued_at,
                r.attempt,
                r.failed_timeout_secs,
                r.failed_error
                    .as_deref()
                    .unwrap_or("BOINC workunit failed for good")
            );
            match r.recorded {
                None => println!(
                    "dry run: nothing changed; to retry this message with a {} s page deadline, give --confirm-message-sha256 {}",
                    r.timeout_secs, r.message_sha256
                ),
                Some(true) => println!(
                    "{} deadline retry recorded: the controller takes the claim when it is claimed again and exports it with a {} s page deadline",
                    r.doc_id, r.timeout_secs
                ),
                Some(false) => println!(
                    "{} deadline retry already recorded ({} s)",
                    r.doc_id, r.timeout_secs
                ),
            }
        }
        ClaimCommands::Release {
            handoff_url,
            queue_url,
            doc_ids,
        } => {
            let handoff = usnm_store::open(&handoff_url)?;
            let queue = usnm_store::queue::open(&queue_url)?;
            for doc_id in doc_ids {
                anyhow::ensure!(
                    usnm_core::ids::PageKey::from_doc_id(&doc_id).is_ok(),
                    "invalid doc id"
                );
                let released = x::release(handoff.as_ref(), queue.as_ref(), &doc_id).await?;
                println!(
                    "{doc_id} {}",
                    if released {
                        "released"
                    } else {
                        "already ended"
                    }
                );
            }
        }
    }
    Ok(())
}

/// A failed job's journal from the hand-off container's dispatch/.
async fn failed_record(
    handoff: &dyn usnm_store::ObjectStore,
    job: &str,
) -> anyhow::Result<crate::Record> {
    anyhow::ensure!(
        job.len() == 64 && job.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid job id"
    );
    let path = format!("dispatch/jobs/{job}/record.json");
    let bytes = handoff
        .get(&path)
        .await?
        .with_context(|| format!("no {path} in the hand-off container"))?;
    let record: crate::Record = serde_json::from_slice(&bytes)?;
    record.job.validate()?;
    crate::retry::validate(&record)?;
    anyhow::ensure!(
        record.job.id == job && record.stage == crate::Stage::Failed,
        "job {job} has not failed; nothing to dead-letter"
    );
    Ok(record)
}
