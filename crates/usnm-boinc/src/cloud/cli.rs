//! The `usnm-boinc cloud` command line. `main.rs` only names the group.
use super::pull::{self, Config};
use super::spool_out;
#[cfg(any(test, feature = "cloud-validatord"))]
use super::validatord;
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
    /// The cloud server's validation webhook for the gold apps (POST /v1/validate/<app>). Dev
    /// only; nothing in the running bridge calls it.
    #[cfg(any(test, feature = "cloud-validatord"))]
    Validatord(ValidatordArgs),
}

#[cfg(any(test, feature = "cloud-validatord"))]
#[derive(Args)]
pub struct ValidatordArgs {
    /// Where to listen. Loopback by default: the service speaks plain http, so put TLS in front.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: std::net::SocketAddr,
    /// Allow a non-loopback --listen (the bearer token would then cross that network in clear).
    #[arg(long)]
    allow_non_loopback: bool,
    /// File holding the webhook bearer token the cloud server sends (its WEBHOOK_TOKEN).
    #[arg(long)]
    secret_file: PathBuf,
    /// Verdicts by call id and scratch space.
    #[arg(long)]
    state_dir: PathBuf,
    /// The project's gold/ directory, which `gold submit` writes.
    #[arg(long)]
    gold_dir: PathBuf,
    /// The cloud server's root (https unless loopback), to fetch outputs from.
    #[arg(long)]
    base_url: String,
    /// File holding the admin bearer token for output reads, or CBOINC_TOKEN.
    #[arg(long)]
    token_file: Option<PathBuf>,
    #[arg(long, default_value_t = 1 << 20)]
    max_body_bytes: usize,
    /// Per call; the server's `<webhook_timeout_secs>` is 20 by default.
    #[arg(long, default_value_t = 15)]
    timeout_secs: u64,
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

#[cfg(any(test, feature = "cloud-validatord"))]
fn read_token(file: &Option<PathBuf>, env: &str) -> anyhow::Result<String> {
    let t = match file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading token file {}", p.display()))?,
        None => std::env::var(env).with_context(|| format!("give a token file or set {env}"))?,
    };
    let t = t.trim().to_owned();
    if t.is_empty() {
        bail!("the token is empty");
    }
    Ok(t)
}

pub async fn run(command: CloudCommands) -> anyhow::Result<()> {
    match command {
        #[cfg(any(test, feature = "cloud-validatord"))]
        CloudCommands::Validatord(a) => {
            if !a.listen.ip().is_loopback() && !a.allow_non_loopback {
                bail!("--listen is not a loopback address; add --allow-non-loopback to serve it");
            }
            let secret = read_token(&Some(a.secret_file), "")?;
            let admin = read_token(&a.token_file, "CBOINC_TOKEN")?;
            let fetch =
                pull::Config::new(&a.base_url, admin, a.state_dir.clone(), false, vec![], 1)?;
            let cfg = validatord::Config {
                secret,
                state_dir: a.state_dir,
                gold_dir: a.gold_dir,
                fetch,
                max_body: a.max_body_bytes,
                call_timeout: Duration::from_secs(a.timeout_secs),
            };
            let listener = tokio::net::TcpListener::bind(a.listen).await?;
            eprintln!("validatord listening on {}", listener.local_addr()?);
            validatord::serve(cfg, listener).await
        }
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
