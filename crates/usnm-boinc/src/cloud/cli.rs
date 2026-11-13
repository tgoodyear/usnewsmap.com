//! The `usnm-boinc cloud` command line. `main.rs` only names the group.
use super::pull::{self, Config};
use anyhow::{bail, Context};
use clap::{Args, Subcommand};
use std::{path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum CloudCommands {
    /// Pull finished workunits from the cloud server into a stock-layout spool, then
    /// acknowledge them (unless --no-ack). Dev cloud server only.
    Pull(PullArgs),
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
    /// Application to pull (the API requires one); repeat for several.
    #[arg(long = "app", required = true)]
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
