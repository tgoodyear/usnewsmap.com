//! The `usnm-boinc ndlocr` command line: its clap definitions and the dispatch to this
//! module's stages. `main.rs` only names the group.
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum NdlocrCommands {
    /// Stage reason:jpn:doc_id:batch_verNN pages from the archive or polite IIIF fetch.
    Stage {
        #[arg(long = "page", required = true)]
        pages: Vec<crate::surya::Page>,
        #[arg(long)]
        root: PathBuf,
        /// Immutable engine BASE image digest; signed wrapper separately pins the worker.
        #[arg(long)]
        engine_image: String,
        #[arg(long, default_value_t = 600)]
        timeout_secs: u32,
        #[arg(long)]
        scan_archive: Option<String>,
        #[arg(long)]
        batch_cache: Option<PathBuf>,
        /// A copy of the scan fetcher's tree (`keep/<batch>/...` with sidecars): stage from
        /// it alone, sending nothing to LoC. A page it lacks fails the stage.
        #[arg(long, conflicts_with = "scan_archive")]
        scans: Option<PathBuf>,
    },
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        doc: String,
    },
    Check {
        #[arg(long, num_args = 3)]
        files: Vec<PathBuf>,
    },
    Compare {
        #[arg(long, num_args = 3)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3)]
        files2: Vec<PathBuf>,
    },
    Collect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        doc: String,
        #[arg(long)]
        curated_url: String,
    },
}

pub async fn run(command: NdlocrCommands) -> anyhow::Result<()> {
    use crate::ndlocr as x;
    match command {
        NdlocrCommands::Stage {
            pages,
            root,
            engine_image,
            timeout_secs,
            scan_archive,
            batch_cache,
            scans,
        } => {
            println!(
                "staged {}",
                x::stage(
                    &root,
                    &pages,
                    x::engine(engine_image, timeout_secs),
                    scan_archive.as_deref(),
                    batch_cache,
                    scans.as_deref()
                )
                .await?
            );
        }
        NdlocrCommands::Submit { root, project, doc } => x::submit(&root, &project, &doc).await?,
        NdlocrCommands::Check { files } => {
            x::load(&files)?;
        }
        NdlocrCommands::Compare { files, files2 } => x::compare(&files, &files2)?,
        NdlocrCommands::Collect {
            root,
            spool,
            doc,
            curated_url,
        } => {
            x::collect(
                &root,
                &spool,
                &doc,
                usnm_store::open(&curated_url)?.as_ref(),
            )
            .await?
        }
    }
    Ok(())
}
