//! The native BOINC worker for `usnm_ocr` and `usnm_gold` (#1418). See docs/boinc-native-ocr.md.
//!
//! The stock client starts this with no arguments in a task's slot. It keeps the client's
//! control channel and runs the unit in a child (`unit`) that reads pages with the same code as
//! `usnm-boinc work` in the container. The child reaches Tesseract through a `tesseract` link
//! to this same file, which sets the page's limits and execs the bundled engine.
use clap::{Parser, Subcommand};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};
use usnm_boinc::native;

#[derive(Parser)]
#[command(
    name = "usnm_ocr",
    about = "Native BOINC worker for usnm_ocr and usnm_gold"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the unit in the current directory. The task's parent process starts this.
    #[command(hide = true)]
    Unit,
    /// Write the build manifest whose digest stands for this build as the worker image does.
    Manifest {
        /// The `usnm_ocr` executable.
        #[arg(long)]
        worker: PathBuf,
        /// The static Tesseract built by ocr/boinc/native/Containerfile.
        #[arg(long)]
        tesseract: PathBuf,
        /// A directory holding every model in the Tesseract lock.
        #[arg(long)]
        tessdata: PathBuf,
        /// Where to write the manifest.
        #[arg(long)]
        out: PathBuf,
    },
}

fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        None => native::run_slot(Path::new(".")),
        Some(Command::Unit) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(native::run_unit(Path::new("."))),
        Some(Command::Manifest {
            worker,
            tesseract,
            tessdata,
            out,
        }) => {
            let build = native::write_manifest(&worker, &tesseract, &tessdata, &out)?;
            println!("image {}", build.image);
            println!("base {}", build.base);
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    // Started through the `tesseract` link: this only returns if the engine can't start.
    if native::is_engine_launch() {
        eprintln!("{:#}", native::exec_engine());
        return ExitCode::FAILURE;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
