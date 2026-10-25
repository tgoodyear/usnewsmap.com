//! The `usnm-boinc claim` command line (#296): its clap definitions and the dispatch to
//! this module's functions. `main.rs` only names the group.
use anyhow::Context;
use clap::Subcommand;
use std::{path::PathBuf, sync::Arc};
use usnm_state::{
    cosmos::CosmosDocs,
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
        #[arg(long = "job", required = true)]
        jobs: Vec<String>,
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
                Arc::new(CosmosDocs::new(
                    &endpoint,
                    usnm_state::DATABASE,
                    usnm_store::credential::from_env_for(usnm_store::credential::COSMOS_RESOURCE),
                )?)
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
            let held = usnm_ingest::ocr_local::boinc::Held::take(&state).await?;
            let mut report = x::Report::default();
            let pulled = async {
                let snapshot =
                    usnm_ingest::ocr_local::boinc::Snapshot::load(&state, reference.as_ref())
                        .await?;
                let ledger = usnm_ingest::ocr_local::ledger::load(curated.as_ref()).await?;
                let (_, fetched) = usnm_ingest::ocr_local::seed::newest_fetched(
                    exchange_out.as_ref(),
                    &manifest_version,
                )
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
        } => {
            let handoff = usnm_store::open(&handoff_url)?;
            let queue = usnm_store::queue::open(&queue_url)?;
            let mut lost = 0;
            for job in jobs {
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
                anyhow::ensure!(
                    record.job.id == job && record.stage == crate::Stage::Failed,
                    "job {job} has not failed; nothing to dead-letter"
                );
                let doc_id = record.job.message.key()?.doc_id();
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
