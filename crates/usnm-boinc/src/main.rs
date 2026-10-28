//! The `usnm-boinc` command line, top level only. Each app and each group of commands keeps its
//! clap definitions and its dispatch in a `cli` module next to its code (`surya::cli`,
//! `quality::cli`, `qwen::cli`, `assess::cli`, `gold::cli`, `ndlocr::cli`, `claim::cli`,
//! `handoff::cli`, `host_trust::cli`), and the commands that carry one job from export to
//! publication are in `cli::job`. This file names them, in the order `--help` lists them, and
//! routes each to its module.
use clap::{Parser, Subcommand};
use usnm_boinc::{
    assess, claim, cli::job, gold, handoff, host_trust, ndlocr, quality, qwen, surya,
};

#[derive(Parser)]
#[command(about = "Opt-in stock BOINC newspaper OCR batch pilot")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// OFF-by-default onboarding and independent-host OCR spot checks; never modifies trust.
    #[command(subcommand)]
    HostTrust(host_trust::cli::HostTrustCommands),
    /// Export selected cached pages and reserve their native requested-work ledger entries.
    Export(job::ExportArgs),
    /// BOINC as a consumer of the OCR work queues (#296).
    #[command(subcommand)]
    Claim(claim::cli::ClaimCommands),
    /// Run one page inside the pinned BOINC worker container.
    Work(job::WorkArgs),
    /// Publish a page on the local producer; this is NOT public readiness proof.
    PrepareDownload(job::PrepareDownloadArgs),
    /// Stage and submit one reserved job with stock create_work.
    Submit(job::SubmitArgs),
    /// Submit several reserved pages as one OCR bundle workunit (#1059).
    SubmitBundle(job::SubmitBundleArgs),
    /// Reconcile an uncertain submission after checking the server for its workunit name.
    Reconcile(job::ReconcileArgs),
    /// Read stock batch_collect_assimilator's spool and publish native ALTO/Parquet.
    Collect(job::CollectArgs),
    /// Quarantine explicitly pinned contract-invalid canonical outputs; never requeue.
    RejectOutput(job::RejectOutputArgs),
    /// Verify the gold bundle and download the original files it refers to (network; outside the
    /// job container).
    GoldStage(gold::cli::GoldStageArgs),
    /// Score an engine on the gold bundle, offline; writes counts only, no text.
    GoldRun(gold::cli::GoldRunArgs),
    /// The JP2 against IIIF JPEG assessment (#456), app `usnm_assess`.
    #[command(subcommand)]
    Assess(assess::cli::AssessCommands),
    /// The gold check as BOINC work (#462), app `usnm_gold`.
    #[command(subcommand)]
    Gold(gold::cli::GoldCommands),
    /// The Surya 2 test on volunteer CPUs and GPUs (#516), app `usnm_surya`.
    #[command(subcommand)]
    Surya(surya::cli::SuryaCommands),
    /// The OCR quality estimate on volunteer CPUs (#522), app `usnm_quality`.
    #[command(subcommand)]
    Quality(quality::cli::QualityCommands),
    /// The vision-language readings on volunteer GPUs and CPUs (#522), app `usnm_qwen`.
    #[command(subcommand)]
    Qwen(qwen::cli::QwenCommands),
    /// Japanese NDLOCR-Lite staging, redundant validation and ocr-ja publication.
    #[command(subcommand)]
    Ndlocr(ndlocr::cli::NdlocrCommands),
    /// The remote hand-off between the OCR node and the BOINC server (#469, docs/boinc/scan-cache-handoff.md).
    #[command(subcommand)]
    Handoff(handoff::cli::HandoffCommands),
    /// Inspect the local submission/publication journal; emits no page text.
    Status(job::StatusArgs),
}

/// Export and claim log to stderr (stdout carries their output): warnings,
/// and the `ocr-sync` lock waits from `usnm_ocr_local` at info (#1133).
/// `RUST_LOG` overrides it. The other commands keep their own output.
fn log_to_stderr() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,usnm_ocr_local=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let command = Cli::parse().command;
    if matches!(command, Commands::Export(_) | Commands::Claim(_)) {
        log_to_stderr();
    }
    let result = match command {
        Commands::HostTrust(command) => host_trust::cli::run(command).await,
        Commands::Export(args) => job::export(args).await,
        Commands::Claim(command) => claim::cli::run(command).await,
        Commands::Work(args) => job::work(args).await,
        Commands::PrepareDownload(args) => job::prepare_download(args),
        Commands::Submit(args) => job::submit(args).await,
        Commands::SubmitBundle(args) => job::submit_bundle(args).await,
        Commands::Reconcile(args) => job::reconcile(args),
        Commands::Collect(args) => job::collect(args).await,
        Commands::RejectOutput(args) => job::reject_output(args),
        Commands::GoldStage(args) => gold::cli::gold_stage(args).await,
        Commands::GoldRun(args) => gold::cli::gold_run(args).await,
        Commands::Assess(command) => assess::cli::run(command).await,
        Commands::Gold(command) => gold::cli::run(command).await,
        Commands::Surya(command) => surya::cli::run(command).await,
        Commands::Quality(command) => quality::cli::run(command).await,
        Commands::Qwen(command) => qwen::cli::run(command).await,
        Commands::Ndlocr(command) => ndlocr::cli::run(command).await,
        Commands::Handoff(command) => handoff::cli::run(command).await,
        Commands::Status(args) => job::status(args),
    };
    if let Err(error) = &result {
        if error.downcast_ref::<qwen::retain::CaptureError>().is_some() {
            eprintln!("{error:#}");
            std::process::exit(3);
        }
    }
    result
}

#[cfg(test)]
mod qwen_runtime_tests {
    use super::*;
    use usnm_boinc::qwen::cli::QwenCommands;

    #[test]
    fn staged_gpu_jobs_allow_four_hours_unless_explicitly_bounded() {
        let args = [
            "usnm-boinc",
            "qwen",
            "stage",
            "--quality-root",
            "quality",
            "--root",
            "qwen",
            "--engine-image",
            "sha256:image",
            "--engine-llama",
            "sha256:llama",
            "--engine-weights",
            "sha256:weights",
        ];
        for override_secs in [None, Some("1800")] {
            let mut command = args.to_vec();
            if let Some(value) = override_secs {
                command.extend(["--gpu-timeout-secs", value]);
            }
            let cli = Cli::try_parse_from(command).unwrap();
            let Commands::Qwen(QwenCommands::Stage {
                gpu_timeout_secs,
                cpu_timeout_secs,
                ..
            }) = cli.command
            else {
                panic!("expected Qwen staging command");
            };
            assert_eq!(
                gpu_timeout_secs,
                if override_secs.is_some() {
                    1800
                } else {
                    14_400
                }
            );
            assert_eq!(cpu_timeout_secs, 7200);
        }
    }
}
