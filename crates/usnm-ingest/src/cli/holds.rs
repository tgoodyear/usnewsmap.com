//! `holds`: writing `quality.boinc_page_holds` (#1956). Holds are written only through
//! `holds add`, which checks the row against the stored pages with the test the release applies
//! and refuses one that matches none.
use anyhow::{bail, Context};
use clap::{Args, Subcommand};
use usnm_ocr_local::quality_db::{HoldsWriter, NewHold};
use usnm_ocr_local::replace::{vet_new_hold, Listing};

use super::common::Stores;
use crate::Cli;

#[derive(Args)]
pub(crate) struct HoldsArgs {
    #[command(subcommand)]
    command: HoldsCommand,
}

#[derive(Subcommand)]
enum HoldsCommand {
    /// Hold one reading of one page out of the index. The row is checked against
    /// `ocr-local/pages` first, as the release will read it, and refused if it matches no
    /// stored row (an engine tag that differs from the stored one by a field, a page with no
    /// reading); `--allow-zero-match` is for a reading that isn't stored yet. Writes with the
    /// DSN file in `USNM_QUALITY_HOLDS_WRITER_DSN_FILE`.
    Add(AddArgs),
}

#[derive(Args)]
struct AddArgs {
    /// The page: `<lccn>_<date>_ed-<n>_seq-<n>`.
    #[arg(long)]
    doc_id: String,
    /// The reading's engine: the stored `ocr_engine` tag in full, or its key
    /// (`<family>/<version> <profile>`). Copy it from the stored row, not from a job.
    #[arg(long)]
    engine: Option<String>,
    /// The reading's `node` (`boinc-<12 hex of the job id>`), instead of or with `--engine`.
    #[arg(long)]
    node: Option<String>,
    #[arg(long)]
    reason: String,
    #[arg(long)]
    created_by: String,
    /// Write the hold although it matches no stored row (a reading not stored yet). The
    /// release then does not stop on it.
    #[arg(long)]
    allow_zero_match: bool,
    /// Check only; write nothing.
    #[arg(long)]
    dry_run: bool,
}

pub(crate) async fn holds_command(cli: &Cli, args: &HoldsArgs) -> anyhow::Result<()> {
    let HoldsCommand::Add(a) = &args.command;
    add(&cli.stores, a).await
}

async fn add(stores: &Stores, a: &AddArgs) -> anyhow::Result<()> {
    if a.engine.is_none() && a.node.is_none() {
        bail!(
            "give --engine or --node: a hold naming neither would take every reading of the page"
        );
    }
    let hold = NewHold {
        doc_id: a.doc_id.clone(),
        node: a.node.clone(),
        engine: a.engine.clone(),
        reason: a.reason.clone(),
        created_by: a.created_by.clone(),
        allow_zero_match: a.allow_zero_match,
    };
    for (name, v) in [
        ("--doc-id", Some(&hold.doc_id)),
        ("--engine", hold.engine.as_ref()),
        ("--node", hold.node.as_ref()),
        ("--reason", Some(&hold.reason)),
        ("--created-by", Some(&hold.created_by)),
    ] {
        if v.is_some_and(|v| v.trim().is_empty() || (name != "--reason" && v.trim() != v)) {
            bail!("{name} is empty or has spaces around it");
        }
    }
    let curated = usnm_store::open(&stores.curated).context("opening the curated lake")?;
    let listing = Listing::list(curated.as_ref()).await?;
    let matched = vet_new_hold(curated.as_ref(), &listing, &hold).await?;
    if a.dry_run {
        println!(
            "dry run: {} matches {matched} stored row(s); nothing written",
            hold.doc_id
        );
        return Ok(());
    }
    let added = HoldsWriter::from_env()?.add(&hold).await?;
    println!(
        "{}: {} (matches {matched} stored row(s))",
        hold.doc_id,
        if added {
            "hold added"
        } else {
            "already held; nothing changed"
        }
    );
    Ok(())
}
