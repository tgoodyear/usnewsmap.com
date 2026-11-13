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
    /// Native OCR spot checks (#1863): replay sampled native pages on a second host trusted for
    /// the same build (or one listed as identical) and compare the results byte for byte. A
    /// mismatch holds both hosts' trust for their builds. Writes the report to --out.
    NativeSpot {
        /// This step's journal root.
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        /// The bridge's job journals: each original's request and scan.
        #[arg(long)]
        jobs_root: PathBuf,
        #[arg(long)]
        trust: PathBuf,
        /// The replays' assimilator results directory (batch 0).
        #[arg(long)]
        spool: PathBuf,
        /// The live usnm_ocr_native version numbers, comma-separated.
        #[arg(long, default_value = "")]
        native_versions: String,
        /// Replays to create: <job>=<source host>:<target host>:<target image hex>, comma-separated.
        #[arg(long, default_value = "")]
        submit: String,
        /// Replays to compare: <job>=<source>:<target>:<target image hex>:<original spool>.
        #[arg(long, default_value = "")]
        check: String,
        /// The report, as JSON with `nonce`.
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        nonce: String,
        #[arg(long)]
        execute: bool,
    },
    /// Release a host's trust hold for one build (#1863) after an operator has looked. The hold
    /// and the note go to holds-released.jsonl beside the trust list.
    ReleaseHold {
        #[arg(long)]
        trust: PathBuf,
        #[arg(long)]
        image: String,
        #[arg(long)]
        host: u64,
        /// What was looked at, on one line.
        #[arg(long)]
        note: String,
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
        HostTrustCommands::NativeSpot {
            root,
            project,
            jobs_root,
            trust,
            spool,
            native_versions,
            submit,
            check,
            out,
            nonce,
            execute,
        } => {
            anyhow::ensure!(
                nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit()),
                "--nonce is 32 hex"
            );
            let versions = native_versions
                .split(',')
                .filter(|v| !v.is_empty())
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()?;
            let paths = x::Paths {
                root,
                project,
                jobs_root,
                trust,
                spool,
            };
            let report = x::native_spot(
                &paths,
                crate::trust::BuildTrust::embedded()?,
                &versions,
                &x::Replay::parse_list(&submit, false)?,
                &x::Replay::parse_list(&check, true)?,
                execute,
            )
            .await?;
            let mut value = serde_json::to_value(&report)?;
            value["nonce"] = nonce.into();
            value["schema_version"] = 1.into();
            crate::atomic(&out, &serde_json::to_vec_pretty(&value)?)?;
            println!("{}", serde_json::to_string(&report)?);
        }
        HostTrustCommands::ReleaseHold {
            trust,
            image,
            host,
            note,
        } => {
            let release = crate::trust::Trust::release_hold(&trust, &image, host, &note)?;
            println!("{}", serde_json::to_string(&release)?);
        }
    }
    Ok(())
}
