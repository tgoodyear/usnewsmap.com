//! One-off (#1938): bind legacy `dead_lettered`/`permanent` ledger outcomes (written before the
//! `message_sha256` column) to the message that was dead-lettered, so `controller-retry` accepts
//! them. Not part of any build or release.
//!
//! For each page it requires that the page's latest outcome is a legacy permanent dead letter,
//! finds the dead-letter archive of that outcome's own run (`ocr-local/deadletter/<day>/<run>.jsonl`),
//! takes the page's line, requires the archived message to equal the parent journal's message,
//! and computes `sha256(serde_json::to_vec(message))`, the digest `controller-retry` checks.
//! With `--apply` it appends one create-only outcome file through `ledger::write_outcomes`: the
//! same outcome, cause, kind and source, at the next ledger time, with `message_sha256` set. The
//! original outcome files are never touched. Writes an audit file beside the ledger.
//!
//! Usage: repair_legacy_outcomes --curated-url URL --journal-dir DIR --docs a,b,c
//!        --by WHO --why TEXT --source TEXT [--apply]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, ensure, Context};
use chrono::Utc;
use usnm_ocr_contract::message::DeadLetter;
use usnm_ocr_local::ledger::{self, Outcome, DEAD_LETTERED};

fn arg(name: &str) -> Option<String> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == name {
            return it.next();
        }
    }
    None
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let apply = std::env::args().any(|a| a == "--apply");
    let curated_url = arg("--curated-url").context("--curated-url")?;
    let journal_dir = PathBuf::from(arg("--journal-dir").context("--journal-dir")?);
    let docs: Vec<String> = arg("--docs")
        .context("--docs")?
        .split(',')
        .map(str::to_owned)
        .collect();
    let by = arg("--by").context("--by")?;
    let why = arg("--why").context("--why")?;
    let source = arg("--source").context("--source")?;

    let curated = usnm_store::open(&curated_url)?;
    let ledger = ledger::load(curated.as_ref()).await?;

    // Parent journals by page.
    let mut journals: HashMap<String, serde_json::Value> = HashMap::new();
    for entry in std::fs::read_dir(journal_dir.join("jobs"))? {
        let path = entry?.path().join("record.json");
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let page = record["job"]["message"]["page"]
            .as_str()
            .context("journal without a page")?
            .to_owned();
        journals.insert(page, record);
    }

    // Dead-letter archive files by run.
    let mut archives: HashMap<String, String> = HashMap::new();
    for path in curated.list("ocr-local/deadletter").await? {
        if let Some(stem) = path
            .rsplit('/')
            .next()
            .and_then(|n| n.strip_suffix(".jsonl"))
        {
            archives.insert(stem.to_owned(), path);
        }
    }

    let now = Utc::now();
    let run = format!("{}-repair1938", now.format("%Y%m%dT%H%M%S%6fZ"));
    let mut rows = Vec::new();
    let mut audit = Vec::new();
    for doc in &docs {
        let old = ledger
            .outcomes
            .get(doc)
            .with_context(|| format!("{doc}: no outcome"))?;
        ensure!(
            old.outcome == DEAD_LETTERED
                && old.cause.as_deref() == Some("permanent")
                && old.message_sha256.is_none(),
            "{doc}: latest outcome is not a legacy permanent dead letter"
        );
        let queued = ledger.queued.get(doc).context("no queued row")?;
        ensure!(
            old.at >= queued.queued_at,
            "{doc}: outcome predates its latest queued row"
        );
        let archive_path = archives
            .get(&old.run)
            .with_context(|| format!("{doc}: no dead-letter archive for run {}", old.run))?;
        let text = String::from_utf8(
            curated
                .get(archive_path)
                .await?
                .context("archive vanished")?,
        )?;
        let page = doc.replace('_', "/");
        let mut found = Vec::new();
        for line in text.lines() {
            let letter: DeadLetter = serde_json::from_str(line)?;
            if let Some(message) = &letter.message {
                if message.page == page {
                    found.push((letter.clone(), message.clone()));
                }
            }
        }
        ensure!(
            found.len() == 1,
            "{doc}: {} archived messages for the page in {archive_path}",
            found.len()
        );
        let (letter, message) = &found[0];
        ensure!(
            letter.cause.map(|c| c.as_str().to_owned()).as_deref() == Some("permanent"),
            "{doc}: archived cause is not permanent"
        );
        let journal = journals
            .get(&page)
            .with_context(|| format!("{doc}: no parent journal"))?;
        ensure!(
            journal["job"]["message"] == serde_json::to_value(message)?,
            "{doc}: archived message differs from the parent journal's"
        );
        let bytes = serde_json::to_vec(message)?;
        let sha = usnm_ocr_contract::boinc::sha256(&bytes);
        let at = ledger.next_event_time(doc, now)?;
        println!(
            "{doc} message {} sha256 {sha} run {} -> {run} at {at}",
            letter.message_id, old.run
        );
        audit.push(serde_json::json!({
            "doc_id": doc,
            "message_id": letter.message_id,
            "message": message,
            "message_sha256": sha,
            "archive": archive_path,
            "original_run": old.run,
            "original_source": old.source,
            "original_at": old.at,
            "parent_job": journal["job"]["id"],
        }));
        rows.push(Outcome {
            doc_id: doc.clone(),
            outcome: old.outcome.clone(),
            cause: old.cause.clone(),
            kind: old.kind.clone(),
            source: old.source.clone(),
            at,
            run: run.clone(),
            message_sha256: Some(sha),
        });
    }
    if !apply {
        println!("dry run: {} rows, nothing written", rows.len());
        return Ok(());
    }
    let audit_path = format!("ocr-local/report/{run}.json");
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "issue": "tgoodyear/usnewsmap.com#1938",
        "by": by,
        "at": now,
        "why": why,
        "source": source,
        "run": run,
        "pages": audit,
    }))?;
    if !curated
        .put_new(&audit_path, body, "application/json")
        .await?
    {
        bail!("{audit_path} already exists");
    }
    let path = ledger::write_outcomes(curated.as_ref(), &rows, now, &run)
        .await?
        .context("no rows")?;
    println!("wrote {path} and {audit_path}");
    Ok(())
}
