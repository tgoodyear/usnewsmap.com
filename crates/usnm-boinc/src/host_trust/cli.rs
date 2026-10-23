//! The `usnm-boinc host-trust` command line: its clap definitions and the dispatch to this
//! module's functions. `main.rs` only names the group.
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum HostTrustCommands {
    /// Read existing quarantine reasons without connecting to BOINC or modifying results.
    Quarantine {
        #[arg(long)]
        root: PathBuf,
    },
    /// Discover BOINC hosts and issue one gold unit per untrusted host/campaign.
    Onboard {
        #[arg(long)]
        config: PathBuf,
        /// Dispatch work; without this switch only read-only discovery/planning occurs.
        #[arg(long)]
        execute: bool,
    },
    /// Collect untrusted hosts' onboarding gold units and admit the hosts whose complete native
    /// evidence passes the scope's existing calibration (#901). Adds hosts only; never sets a
    /// threshold or revokes.
    Promote {
        #[arg(long)]
        config: PathBuf,
        /// The gold units' assimilator results directory (batch 0).
        #[arg(long)]
        spool: PathBuf,
        /// Query BOINC, collect and write trust; without this switch only already collected
        /// evidence is judged and nothing is written.
        #[arg(long)]
        execute: bool,
    },
    /// Deterministically sample OCR jobs and rerun on a second same-image trusted host.
    SpotSubmit {
        #[arg(long)]
        config: PathBuf,
        /// JSON array of original job digests.
        #[arg(long)]
        jobs: PathBuf,
        #[arg(long)]
        execute: bool,
    },
    /// Validate both canonical results and record agreement; never publishes or grants trust.
    SpotCompare {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        original_spool: PathBuf,
        #[arg(long)]
        spot_spool: PathBuf,
    },
}

pub async fn run(command: HostTrustCommands) -> anyhow::Result<()> {
    use crate::host_trust as x;
    match command {
        HostTrustCommands::Quarantine { root } => {
            println!("{}", serde_json::to_string_pretty(&x::quarantine(&root)?)?);
        }
        HostTrustCommands::Onboard { config, execute } => {
            let hosts = x::onboard(&x::Config::load(&config)?, execute).await?;
            println!("{}", serde_json::to_string(&hosts)?);
        }
        HostTrustCommands::Promote {
            config,
            spool,
            execute,
        } => {
            let p = x::promote(&x::Config::load(&config)?, &spool, execute).await?;
            println!("{}", serde_json::to_string(&p)?);
            // Hosts that could be judged were; the others are named in the summary.
            anyhow::ensure!(
                p.failed.is_empty(),
                "gold evidence of {} host(s) could not be collected or judged; see the summary",
                p.failed.len()
            );
        }
        HostTrustCommands::SpotSubmit {
            config,
            jobs,
            execute,
        } => {
            let units = x::spot_submit(&x::Config::load(&config)?, &jobs, execute).await?;
            println!("{}", serde_json::to_string(&units)?);
        }
        HostTrustCommands::SpotCompare {
            config,
            job,
            original_spool,
            spot_spool,
        } => {
            anyhow::ensure!(
                x::spot_compare(&x::Config::parse(&config)?, &job, &original_spool, &spot_spool).await?,
                "independent OCR spot check disagrees; inspect comparison.json; no trust or publication changed"
            );
        }
    }
    Ok(())
}
