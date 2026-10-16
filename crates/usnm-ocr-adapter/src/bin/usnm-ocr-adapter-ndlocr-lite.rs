use std::path::PathBuf;

use anyhow::ensure;
use clap::Parser;
use usnm_ocr_adapter::{
    ndlocr::{self, Ndlocr},
    serve, Root,
};
use usnm_ocr_contract::engine::CONTRACT;

#[derive(Parser)]
#[command(about = "Pinned CPU NDLOCR-Lite adapter (usnm-ocr-engine/1)")]
struct Args {
    #[arg(long)]
    contract: u32,
    /// Accepted for the common adapter launch interface. BOINC limits CPUs with cgroups.
    #[arg(long, default_value_t = 1)]
    threads: u32,
    #[arg(long, default_value = "/opt/ndlocr-lite")]
    ndlocr_root: PathBuf,
    #[arg(long, default_value = "python3")]
    python: PathBuf,
    #[arg(long, default_value = "/out")]
    scratch: PathBuf,
    /// Prefix for /in and /out during a local conformance test.
    #[arg(long, default_value = "/")]
    root: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(args.contract == CONTRACT, "unsupported contract");
    ensure!(args.threads > 0, "zero threads");
    let hello = ndlocr::hello(&args.ndlocr_root)?;
    hello.check()?;
    let mut reader = Ndlocr {
        root: args.ndlocr_root,
        python: args.python,
        scratch: args.scratch,
    };
    serve(
        &hello,
        &mut reader,
        &Root(args.root),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )
}
