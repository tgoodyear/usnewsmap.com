//! The `usnm-boinc handoff` command line (#469): its clap definitions and the dispatch to
//! this module's functions. `main.rs` only names the group.
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum HandoffCommands {
    /// Copy exactly one object, without downloading adjacent jobs or scans.
    GetFile {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Copy every object under a prefix of the hand-off container into a directory.
    Get {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        prefix: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Write every file under a directory to a prefix of the hand-off container.
    Put {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to_url: String,
        #[arg(long)]
        prefix: String,
    },
    /// Server side: copy one exported job into the hand-off root. Prints `reserved` (submit
    /// it) or the stage the server already had.
    Pull {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        job: String,
        /// The scans the controller pushed to this host (`keep/<batch>/...`): the job's page
        /// comes from there when it matches, and from the container otherwise (#1116).
        #[arg(long)]
        scans: Option<PathBuf>,
    },
    /// Server side: write a job's spool files, its host id and trust.json to the container.
    /// With --root (the server's hand-off root), a page submitted in an OCR bundle gets its
    /// share of the bundle's result, or is requeued alone if it failed there (prints
    /// `requeued`).
    PutSpool {
        #[arg(long)]
        to_url: String,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        host_id: Option<u64>,
        #[arg(long)]
        trust: Option<PathBuf>,
    },
    /// Server side: print the BOINC workunit a job runs in (its own, or its OCR bundle's).
    Workunit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        job: String,
    },
    /// Hand-off job side: copy a job's spool and trust.json (when there) down; prints the
    /// host id, `unknown`, or `requeued` (the page went back to BOINC alone).
    GetSpool {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        job: String,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        trust_out: PathBuf,
    },
}

pub async fn run(command: HandoffCommands) -> anyhow::Result<()> {
    use crate::handoff as x;
    match command {
        HandoffCommands::GetFile { from_url, path, to } => {
            x::get_file(usnm_store::open(&from_url)?.as_ref(), &path, &to).await?;
        }
        HandoffCommands::Get {
            from_url,
            prefix,
            to,
        } => {
            let n = x::get(usnm_store::open(&from_url)?.as_ref(), &prefix, &to).await?;
            eprintln!("{n} objects copied from {prefix}/ to {}", to.display());
        }
        HandoffCommands::Put {
            from,
            to_url,
            prefix,
        } => {
            let n = x::put(usnm_store::open(&to_url)?.as_ref(), &from, &prefix).await?;
            eprintln!("{n} files written from {} to {prefix}/", from.display());
        }
        HandoffCommands::Pull {
            from_url,
            root,
            job,
            scans,
        } => match x::pull_with(
            usnm_store::open(&from_url)?.as_ref(),
            &root,
            &job,
            scans.as_deref(),
        )
        .await?
        {
            x::Pulled::Reserved => println!("reserved"),
            x::Pulled::Already(stage) => {
                println!("{}", serde_json::to_value(stage)?.as_str().unwrap_or("?"))
            }
        },
        HandoffCommands::PutSpool {
            to_url,
            root,
            spool,
            job,
            host_id,
            trust,
        } => {
            let store = usnm_store::open(&to_url)?;
            match x::put_spool(
                store.as_ref(),
                root.as_deref(),
                &spool,
                &job,
                host_id,
                trust.as_deref(),
            )
            .await?
            {
                x::Spooled::Files(n) => eprintln!("{n} spool files written for {job}"),
                x::Spooled::Requeued(why) => {
                    eprintln!("{job} failed in its bundle ({why}); requeued to go alone");
                    println!("requeued");
                }
            }
        }
        HandoffCommands::Workunit { root, job } => {
            println!("{}", crate::bundle::submit::workunit(&root, &job)?);
        }
        HandoffCommands::GetSpool {
            from_url,
            job,
            spool,
            trust_out,
        } => {
            let store = usnm_store::open(&from_url)?;
            let host = match x::get_spool(store.as_ref(), &job, &spool).await? {
                x::Spool::Requeued => {
                    println!("requeued");
                    return Ok(());
                }
                x::Spool::Files(host) => host,
            };
            if !x::get_trust(store.as_ref(), &trust_out).await? {
                eprintln!("no gold/trust.json in the hand-off container");
            }
            println!("{}", host.map_or("unknown".into(), |h| h.to_string()));
        }
    }
    Ok(())
}
