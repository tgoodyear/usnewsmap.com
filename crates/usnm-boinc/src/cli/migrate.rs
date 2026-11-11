//! `docs-migrate` and `queue-move`: the one-off copies of pipeline documents and work-queue
//! messages from Azure to the on-prem PostgreSQL database at the cutover (#1367,
//! docs/boinc/server/local-state.md). Progress goes to stderr, and the receipt, one JSON
//! object, to stdout.
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use clap::Args;
use serde_json::{json, Value};
use usnm_state::migrate;
use usnm_store::queue_move;

#[derive(Args, Debug)]
pub struct DocsMigrateArgs {
    /// The Cosmos DB account endpoint (https://<account>.documents.azure.com/), read with the
    /// ambient Entra credential.
    #[arg(long)]
    pub from: String,
    /// The document database: a certificate-authenticated postgres:// location
    /// (scripts/onprem/queue/documents.sql applied).
    #[arg(long)]
    pub to: String,
    /// A container to take (batches, index_runs, issues, ops, titles); repeat for more.
    /// Default: all of them, refusing a source that holds any other.
    #[arg(long = "container")]
    pub containers: Vec<String>,
    /// Write nothing: compare counts and per-container digests of both sides, and exit
    /// non-zero on any difference.
    #[arg(long)]
    pub verify: bool,
    /// Documents per Cosmos page.
    #[arg(long, default_value_t = 100)]
    pub page_size: u32,
    /// Average request units per second to keep reads under (0: no pacing). The account's
    /// throughput is never changed.
    #[arg(long, default_value_t = 200.0)]
    pub max_ru_per_sec: f64,
}

#[derive(Args, Debug)]
pub struct QueueMoveArgs {
    /// The Azure queue: https://<account>.queue.core.windows.net/<queue>, with the ambient
    /// Entra credential.
    #[arg(long)]
    pub from: String,
    /// The pgmq queue: a certificate-authenticated postgres:// location with
    /// `&queue=<name>` (e.g. `queue=ocr-bulk`).
    #[arg(long)]
    pub to: String,
    /// Stop after this many messages.
    #[arg(long)]
    pub max: Option<u64>,
    /// Count both queues and hash up to 32 messages at the front of the source with a peek;
    /// receive, send and delete nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Seconds a received message stays hidden at the source while it is sent.
    #[arg(long, default_value_t = 300)]
    pub visibility_secs: u64,
    /// Messages in flight at once.
    #[arg(long, default_value_t = 4)]
    pub concurrency: usize,
    /// Append one JSON line per message (source id, dequeue count, body SHA-256, outcome)
    /// to this file.
    #[arg(long)]
    pub hash_log: Option<PathBuf>,
}

fn progress(line: &str) {
    eprintln!("{line}");
}

/// A postgres:// location without its query (certificate paths), for receipts.
fn label(location: &str) -> String {
    location.split('?').next().unwrap_or_default().to_owned()
}

fn print_receipt(receipt: &Value) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, receipt)?;
    writeln!(out)?;
    Ok(())
}

pub async fn docs_migrate(args: DocsMigrateArgs) -> anyhow::Result<()> {
    ensure!(
        args.to.starts_with("postgres://") || args.to.starts_with("postgresql://"),
        "--to is a postgres:// location"
    );
    let source = usnm_state::cosmos::CosmosDocs::new(
        &args.from,
        usnm_state::DATABASE,
        usnm_store::credential::from_env_for(usnm_store::credential::COSMOS_RESOURCE),
    )?;
    let target = usnm_state::postgres::PostgresDocs::new(&args.to)?;
    let opts = migrate::Options {
        containers: args.containers.clone(),
        page_size: args.page_size,
        max_ru_per_sec: (args.max_ru_per_sec > 0.0).then_some(args.max_ru_per_sec),
    };
    let mode = if args.verify {
        migrate::Mode::Verify
    } else {
        migrate::Mode::Copy
    };
    let receipt = migrate::run(&source, &target, mode, &opts, &progress).await?;
    let mut out = serde_json::to_value(&receipt)?;
    out["command"] = json!("docs-migrate");
    out["from"] = json!(args.from);
    out["to"] = json!(label(&args.to));
    print_receipt(&out)?;
    if !receipt.ok {
        let differ: Vec<&str> = receipt
            .containers
            .iter()
            .filter(|c| c.differences != Some(0))
            .map(|c| c.name.as_str())
            .collect();
        bail!("the document stores differ in {differ:?}");
    }
    Ok(())
}

pub async fn queue_move(args: QueueMoveArgs) -> anyhow::Result<()> {
    ensure!(
        args.visibility_secs > 0,
        "--visibility-secs must be at least 1"
    );
    ensure!(
        (1..=32).contains(&args.concurrency),
        "--concurrency must be 1 to 32"
    );
    let (account, from_queue) = queue_move::split_azure_queue(&args.from)?;
    let (location, to_queue) = queue_move::split_pgmq_queue(&args.to)?;
    let from = usnm_store::queue::AzureQueue::new(&account, usnm_store::credential::from_env())?;
    let to = usnm_store::pgmq::PgmqQueue::new(&location)?;
    let log = match &args.hash_log {
        Some(path) => Some(Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("opening {}", path.display()))?,
        )),
        None => None,
    };
    let log_error: Mutex<Option<String>> = Mutex::new(None);
    let record = |r: &queue_move::Record| {
        let Some(file) = &log else { return };
        let line = serde_json::to_string(r).expect("a record serializes");
        if let Err(e) = writeln!(file.lock().expect("hash log"), "{line}") {
            log_error
                .lock()
                .expect("log error")
                .get_or_insert_with(|| e.to_string());
        }
    };
    let started_at = chrono::Utc::now();
    let report = if args.dry_run {
        let n = args.max.map_or(32, |m| m.clamp(1, 32) as u32);
        queue_move::peek_messages(&from, &from_queue, &to, &to_queue, n, &progress, &record).await
    } else {
        let opts = queue_move::Options {
            max: args.max,
            visibility: Duration::from_secs(args.visibility_secs),
            concurrency: args.concurrency,
        };
        queue_move::move_messages(
            &from,
            &from_queue,
            &to,
            &to_queue,
            &opts,
            &progress,
            &record,
        )
        .await
    };
    let mut out = serde_json::to_value(&report)?;
    out["command"] = json!("queue-move");
    out["from"] = json!(args.from);
    out["to"] = json!(format!("{}?queue={to_queue}", label(&location)));
    out["started_at"] = json!(started_at);
    out["finished_at"] = json!(chrono::Utc::now());
    out["body_digest_kind"] = json!("sum of sha256(body) mod 2^256");
    if let Some(e) = log_error.into_inner().expect("log error") {
        out["hash_log_error"] = json!(e);
    }
    print_receipt(&out)?;
    if let Some(e) = &report.error {
        bail!("queue-move stopped: {e}");
    }
    if out.get("hash_log_error").is_some() {
        bail!("writing the hash log failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_name_the_database_without_its_certificate_paths() {
        assert_eq!(
            label("postgres://h:5432/usnm_state?user=u&sslcert=/x"),
            "postgres://h:5432/usnm_state"
        );
    }
}
