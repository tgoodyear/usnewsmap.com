//! The `usnm-boinc assess` command line (#456): its clap definitions and the dispatch to
//! this module's stages. `main.rs` only names the group.
use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum AssessCommands {
    /// Draw the stratified OCR sample and the larger crash-only list.
    Sample {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out_ocr: PathBuf,
        #[arg(long)]
        out_crash: PathBuf,
        #[arg(long, default_value_t = 3000)]
        ocr_pages: usize,
        #[arg(long, default_value_t = 20000)]
        crash_pages: usize,
        #[arg(long, default_value_t = 2)]
        min_per_stratum: usize,
        #[arg(long)]
        seed: String,
        /// JSON object: LCCN to the title's MARC language codes.
        #[arg(long)]
        title_languages: Option<PathBuf>,
    },
    /// Fetch sampled pages from LoC (one request at a time) into unit directories.
    Stage {
        #[arg(long)]
        sample: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long, value_enum)]
        mode: AssessMode,
        /// The worker image's full digest (sha256:...), the pinned engine build.
        #[arg(long)]
        engine_image: String,
        /// The Tesseract image the worker image was built on (its TESSERACT_IMAGE).
        #[arg(long)]
        engine_base: String,
        #[arg(long, default_value = "5.5.0")]
        engine_version: String,
        /// Page deadline; defaults to 1200 for ocr and 20 for crash.
        #[arg(long)]
        timeout_secs: Option<u32>,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
        #[arg(long, default_value_t = 100)]
        max_pages: usize,
        #[arg(long)]
        batch_cache: Option<PathBuf>,
        /// The scan archive's container URL (https://<account>.blob.core.windows.net/scans),
        /// read first with the job's identity; omit for LoC only.
        #[arg(long)]
        scan_archive: Option<String>,
    },
    /// Submit staged units with stock create_work, two results each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, value_enum)]
        mode: AssessMode,
        #[arg(long, default_value_t = 500)]
        max_units: usize,
    },
    /// script_validator's init script: check one result's files.
    Check {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
    },
    /// script_validator's compare script: do two results agree?
    Compare {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3, required = true)]
        files2: Vec<PathBuf>,
    },
    /// Write score.py/aggregate.py inputs from the canonical results.
    Collect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}
#[derive(Clone, Copy, ValueEnum)]
pub enum AssessMode {
    Ocr,
    Crash,
}
impl From<AssessMode> for usnm_ocr_contract::assess::Mode {
    fn from(m: AssessMode) -> Self {
        match m {
            AssessMode::Ocr => Self::Ocr,
            AssessMode::Crash => Self::Crash,
        }
    }
}

pub async fn run(command: AssessCommands) -> anyhow::Result<()> {
    use crate::assess as x;
    match command {
        AssessCommands::Sample {
            manifest,
            out_ocr,
            out_crash,
            ocr_pages,
            crash_pages,
            min_per_stratum,
            seed,
            title_languages,
        } => {
            let s = x::sample::run(&x::sample::Options {
                manifest,
                out_ocr,
                out_crash,
                ocr_pages,
                crash_pages,
                min_per_stratum,
                seed,
                title_languages,
            })?;
            eprintln!("{s:?}");
        }
        AssessCommands::Stage {
            sample,
            root,
            mode,
            engine_image,
            engine_base,
            engine_version,
            timeout_secs,
            gap_secs,
            max_pages,
            batch_cache,
            scan_archive,
        } => {
            let mode = mode.into();
            let s = x::stage::run(&x::stage::Options {
                sample,
                root,
                mode,
                engine_image,
                engine_base,
                engine_version,
                timeout_secs: timeout_secs.unwrap_or(match mode {
                    usnm_ocr_contract::assess::Mode::Ocr => 1200,
                    usnm_ocr_contract::assess::Mode::Crash => 20,
                }),
                gap: std::time::Duration::from_secs(gap_secs),
                max_pages,
                batch_cache,
                scan_archive,
            })
            .await?;
            eprintln!("{s:?}");
        }
        AssessCommands::Submit {
            root,
            project,
            mode,
            max_units,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                mode: mode.into(),
                max_units,
            })
            .await?;
            eprintln!("{s:?}");
        }
        AssessCommands::Check { files } => x::validate::check(&files)?,
        AssessCommands::Compare { files, files2 } => x::validate::compare(&files, &files2)?,
        AssessCommands::Collect { root, spool, out } => {
            let s = x::collect::run(&x::collect::Options { root, spool, out })?;
            eprintln!("{s:?}");
        }
    }
    Ok(())
}
