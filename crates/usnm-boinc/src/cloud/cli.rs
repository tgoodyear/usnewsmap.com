//! The `usnm-boinc cloud` command line. `main.rs` only names the group.
use super::pull::{self, Config};
use super::spool_out;
use anyhow::{bail, Context};
use clap::{Args, Subcommand};
use std::{path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum CloudCommands {
    /// Pull finished workunits from the cloud server into a stock-layout spool, then
    /// acknowledge them (unless --no-ack). Dev cloud server only.
    Pull(PullArgs),
    /// Hand the spool `cloud pull` wrote to the hand-off store with `handoff::put_spool`, one
    /// finished single-page job at a time. Dev cloud server only.
    SpoolOut(SpoolOutArgs),
}

#[derive(Args)]
pub struct SpoolOutArgs {
    /// The spool root `cloud pull` writes (`results/<batch>/` under it).
    #[arg(long)]
    spool_dir: PathBuf,
    /// The hand-off store, as `handoff put-spool --to-url`. Not needed with --dry-run.
    #[arg(long)]
    to_url: Option<String>,
    /// A gold/trust.json to put with each job, as the bridge does when the share has one.
    #[arg(long)]
    trust: Option<PathBuf>,
    /// List what would be handed off; touch nothing.
    #[arg(long)]
    dry_run: bool,
    /// Hand off again even where the ledger says it was done.
    #[arg(long)]
    redo: bool,
    /// Make one pass and exit. Without it, pass again every --interval-secs.
    #[arg(long)]
    once: bool,
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,
}

#[derive(Args)]
pub struct PullArgs {
    /// Server root, e.g. https://boinc-dev.example.org (the /admin/v1 path is added).
    #[arg(long)]
    base_url: String,
    /// File holding the admin bearer token. Without it the CBOINC_TOKEN environment variable
    /// is used. The token is never taken from the command line and never logged.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Spool root: files go to `results/<batch>/`, staging to `.incoming/`.
    #[arg(long)]
    spool_dir: PathBuf,
    /// Shadow mode: write the spool but never acknowledge.
    #[arg(long)]
    no_ack: bool,
    /// Stop after this many workunits in total (per pass).
    #[arg(long)]
    limit: Option<usize>,
    /// Application to pull; repeat for several. Without it, every app is pulled.
    #[arg(long = "app")]
    apps: Vec<String>,
    /// Workunits per request (1 to 500).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    page_size: u32,
    /// Make one pass and exit. Without it, pass again every --interval-secs.
    #[arg(long)]
    once: bool,
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,
}

pub async fn run(command: CloudCommands) -> anyhow::Result<()> {
    match command {
        CloudCommands::SpoolOut(a) => {
            let cfg = spool_out::Config {
                spool: a.spool_dir,
                to_url: a.to_url,
                trust: a.trust,
                dry_run: a.dry_run,
                redo: a.redo,
            };
            loop {
                let report = spool_out::spool_out_once(&cfg).await?;
                println!("{report}");
                if !report.failed.is_empty() {
                    bail!("{} job(s) not handed off", report.failed.len());
                }
                if a.once || a.dry_run {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(a.interval_secs)).await;
            }
        }
        CloudCommands::Pull(a) => {
            let token = match &a.token_file {
                Some(p) => std::fs::read_to_string(p)
                    .with_context(|| format!("reading token file {}", p.display()))?,
                None => std::env::var("CBOINC_TOKEN")
                    .context("give --token-file or set CBOINC_TOKEN")?,
            };
            let token = token.trim().to_owned();
            if token.is_empty() {
                bail!("the token is empty");
            }
            let cfg = Config::new(
                &a.base_url,
                token,
                a.spool_dir,
                !a.no_ack,
                a.apps,
                a.page_size,
            )?;
            loop {
                let stats = pull::pull_once(&cfg, a.limit).await?;
                println!("{stats}");
                if !stats.failed.is_empty() {
                    bail!("{} workunit(s) not pulled", stats.failed.len());
                }
                if a.once {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(a.interval_secs)).await;
            }
        }
    }
}
