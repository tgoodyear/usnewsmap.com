//! The native BOINC worker for `usnm_ocr` and `usnm_gold` (#1418). See docs/boinc/apps/native-ocr.md.
//!
//! The stock client starts this with no arguments in a task's slot. It keeps the client's
//! control channel and runs the unit in a child (`unit`) that reads pages with the same code as
//! `usnm-boinc work` in the container. The child reaches Tesseract through a `tesseract` link
//! to this same file, which sets the page's limits and execs the bundled engine.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
))]
use clap::{Parser, Subcommand};
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
))]
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
))]
use usnm_boinc::native;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
))]
#[derive(Parser)]
#[command(
    name = "usnm_ocr",
    about = "Native BOINC worker for usnm_ocr and usnm_gold"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
))]
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
        /// The static Tesseract: boinc/apps/native/Containerfile on Linux, build-tesseract-macos.sh on macOS, build-tesseract-windows.ps1 on Windows.
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

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
))]
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

/// Linux, macOS and x86-64 Windows only (docs/boinc/apps/native-ocr.md).
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
)))]
fn main() -> ExitCode {
    eprintln!("usnm_ocr runs on Linux, macOS and x86-64 Windows only");
    ExitCode::FAILURE
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
))]
fn main() -> ExitCode {
    // Started through the `tesseract` link: this only returns if the engine can't start.
    // Windows has no link: the unit runs the engine itself.
    #[cfg(unix)]
    if native::is_engine_launch() {
        eprintln!("{:#}", native::exec_engine());
        return ExitCode::FAILURE;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{:?}` names every cause, and shows the backtrace when RUST_BACKTRACE asks for it.
            eprintln!("{error:?}");
            // A page too slow for its CPU limit exits with its own status, for BOINC's
            // `result.exit_status` (#1919).
            ExitCode::from(native::exit_code(&error))
        }
    }
}
