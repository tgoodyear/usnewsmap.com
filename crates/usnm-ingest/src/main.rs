//! The `usnm-ingest` command line, top level only. Each command group keeps its clap arguments
//! and the functions that run them in a module of `cli` (`cli::curate`, `cli::release`,
//! `cli::titles`, `cli::ocr`, ...), and a command's help is the doc comment on its arguments
//! struct there. This file names the commands in the order `--help` lists them (those that take
//! no arguments are documented here), starts the process and routes each command to its handler.
mod cli;

use clap::{Parser, Subcommand};
use cli::common::{state, Stores};
use cli::{batches, congress, curate, ocr, onprem, pictures, prune, release, run, titles};
use usnm_ingest::activity::{self, Reporter};
use usnm_ingest::owner_id;
use usnm_ingest::release::{Published, PublishedUnrecorded, TitlesLeft};
use usnm_ingest::state::{Outcome, State, Step};
use usnm_ingest::telemetry;

/// US News Map ingest pipeline (docs/design/04 §4.4).
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(flatten)]
    stores: Stores,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Enqueue(curate::EnqueueArgs),
    Curate(curate::CurateArgs),
    Release(release::ReleaseArgs),
    TitlesSync(titles::TitlesSyncArgs),
    /// Rebuild `catalog/titles.json` and `places.json` from the cached records
    /// and `overrides/places.json` (no network).
    Geocode,
    /// Save titles-sync's record cache and the catalog to the raw store
    /// (`USNM_RAW_URL`) as `titles/{time}.json`, unless the newest copy
    /// there is the same (docs/operations/environments-and-deploys.md, "Dev environment lifecycle").
    /// titles-sync does this itself after each run; this is for doing it by
    /// hand, e.g. before a teardown.
    TitlesArchive,
    /// Start a new environment's record cache and catalog from the newest
    /// copy in the raw store (`USNM_RAW_URL`), so it needs no fresh
    /// titles-sync. Writes nothing if the environment has a record cache
    /// already; never replaces a file.
    TitlesSeed,
    ArchiveCopy(batches::ArchiveCopyArgs),
    /// Report pages that ship in more than one curated batch (04 §4.7):
    /// JSON on stdout. Reads every batch's counts and the parts of the
    /// batches that share title-days; changes nothing.
    Duplicates,
    Prune(prune::PruneArgs),
    SeedState(onprem::SeedStateArgs),
    ImportStagedRun(onprem::ImportStagedRunArgs),
    MirrorStore(onprem::MirrorStoreArgs),
    PublishStaged(onprem::PublishStagedArgs),
    OcrDamage(ocr::OcrDamageArgs),
    OcrAgreement(ocr::OcrAgreementArgs),
    OcrPrefetchManifest(ocr::OcrPrefetchManifestArgs),
    OcrSync(ocr::OcrSyncArgs),
    OcrSeed(ocr::OcrSeedArgs),
    OcrEnqueue(ocr::OcrEnqueueArgs),
    OcrRequeue(ocr::OcrRequeueArgs),
    CongressGlobe(congress::CongressGlobeArgs),
    CongressSerialset(congress::CongressSerialsetArgs),
    CongressDamage(congress::CongressDamageArgs),
    Pictures(pictures::PicturesArgs),
    ReleaseFinish(release::ReleaseFinishArgs),
    Run(run::RunArgs),
}

/// The exit code of a run stopped by SIGTERM (128 + 15), which the on-prem
/// unit counts as a clean stop (`SuccessExitStatus=143`), as it does a
/// process the signal kills.
const EXIT_SIGTERM: u8 = 143;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let telemetry = telemetry::init();
    let result = run(&cli).await;
    // Every failure ends with one JSON line (the whole error chain), which
    // the job-failure alert and the `errors-by-batch` query look for. Its
    // `outcome` is `titles_left` for the expected stop of a full run that
    // titles-sync didn't finish (the alert leaves those out; starting the
    // job again continues), else `failed`. The exit code stays 1 either
    // way, so the execution shows as failed and is started again.
    // Returning an exit code rather than the error keeps Rust from printing
    // it again as plain text after that line.
    let code = match &result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) if e.is::<usnm_ingest::congress::fetch::Stopped>() => {
            tracing::info!(command = cli.command.name(), "{e}");
            std::process::ExitCode::from(EXIT_SIGTERM)
        }
        Err(e) => {
            tracing::error!(
                command = cli.command.name(),
                outcome = failure_outcome(e),
                error = %format!("{e:#}"),
                "command failed"
            );
            // Exported by the shutdown below, for the on-prem runs' alerts.
            telemetry::metrics().command_failures.add(
                1,
                &[
                    opentelemetry::KeyValue::new("command", cli.command.name()),
                    opentelemetry::KeyValue::new("outcome", failure_outcome(e)),
                ],
            );
            std::process::ExitCode::FAILURE
        }
    };
    // Before exit on every path: whatever telemetry is still buffered is lost.
    telemetry.shutdown().await;
    code
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Enqueue { .. } => "enqueue",
            Command::Curate { .. } => "curate",
            Command::Release { .. } => "release",
            Command::ReleaseFinish { .. } => "release-finish",
            Command::TitlesSync { .. } => "titles-sync",
            Command::Geocode => "geocode",
            Command::TitlesArchive => "titles-archive",
            Command::TitlesSeed => "titles-seed",
            Command::Duplicates => "duplicates",
            Command::ArchiveCopy { .. } => "archive-copy",
            Command::Prune { .. } => "prune",
            Command::SeedState { .. } => "seed-state",
            Command::PublishStaged { .. } => "publish-staged",
            Command::MirrorStore { .. } => "mirror-store",
            Command::ImportStagedRun { .. } => "import-staged-run",
            Command::OcrDamage { .. } => "ocr-damage",
            Command::OcrAgreement { .. } => "ocr-agreement",
            Command::OcrPrefetchManifest { .. } => "ocr-prefetch-manifest",
            Command::OcrSync { .. } => "ocr-sync",
            Command::OcrSeed { .. } => "ocr-seed",
            Command::OcrEnqueue { .. } => "ocr-enqueue",
            Command::OcrRequeue { .. } => "ocr-requeue",
            Command::CongressGlobe { .. } => "congress-globe",
            Command::CongressSerialset { .. } => "congress-serialset",
            Command::CongressDamage { .. } => "congress-damage",
            Command::Pictures { .. } => "pictures",
            Command::Run { .. } => "run",
        }
    }
}

/// The step a command starts at, for those that report what they do on
/// the status page (`ops/activity`): the ingest job's `run`, and
/// `titles-sync` and `release` run by hand. Curate workers don't.
fn first_step(command: &Command) -> Option<Step> {
    match command {
        Command::Run { .. } => Some(Step::Listing),
        Command::TitlesSync { .. } => Some(Step::Titles),
        Command::Release { .. } | Command::ReleaseFinish { .. } => Some(Step::Indexing),
        Command::Enqueue { .. }
        | Command::Curate { .. }
        | Command::Geocode
        | Command::TitlesArchive
        | Command::TitlesSeed
        | Command::Duplicates
        | Command::ArchiveCopy { .. }
        | Command::Prune { .. }
        | Command::SeedState { .. }
        | Command::PublishStaged { .. }
        | Command::MirrorStore { .. }
        | Command::ImportStagedRun { .. }
        | Command::OcrDamage { .. }
        | Command::OcrAgreement { .. }
        | Command::OcrPrefetchManifest { .. }
        | Command::OcrSync { .. }
        | Command::OcrSeed { .. }
        | Command::OcrEnqueue { .. }
        | Command::OcrRequeue { .. }
        | Command::CongressGlobe { .. }
        | Command::CongressSerialset { .. }
        | Command::CongressDamage { .. }
        | Command::Pictures { .. } => None,
    }
}

/// The `outcome` of the "command failed" line: `titles_left` for a run that
/// stopped because titles-sync left titles (expected; start it again),
/// `failed` for anything else.
fn failure_outcome(e: &anyhow::Error) -> &'static str {
    if e.is::<TitlesLeft>() {
        "titles_left"
    } else {
        "failed"
    }
}

/// How a reporting command ended, for `ops/activity`.
fn outcome(result: &anyhow::Result<Option<Published>>) -> (Outcome, Option<String>) {
    match result {
        Ok(Some(_)) => (Outcome::Published, None),
        Ok(None) => (Outcome::NothingNew, None),
        Err(e) => {
            // The site changed even though the command failed afterwards.
            let kind = if e.is::<PublishedUnrecorded>() {
                Outcome::Published
            } else if e.is::<TitlesLeft>() {
                Outcome::TitlesLeft
            } else {
                Outcome::Failed
            };
            (kind, Some(format!("{e:#}")))
        }
    }
}

async fn run(cli: &Cli) -> anyhow::Result<()> {
    let start = tokio::time::Instant::now();
    let state = state(&cli.stores)?;
    let report = match first_step(&cli.command) {
        Some(step) => Reporter::start(state.clone(), cli.command.name(), &owner_id(), step).await,
        None => Reporter::off(),
    };
    let beat = report.every(activity::HEARTBEAT);
    let result = command(cli, &state, start, &report).await;
    drop(beat);
    let (kind, error) = outcome(&result);
    report.end(kind, error).await;
    result.map(drop)
}

/// Run the command; `Some` when it published a version.
async fn command(
    cli: &Cli,
    state: &State,
    start: tokio::time::Instant,
    report: &Reporter,
) -> anyhow::Result<Option<Published>> {
    let state = state.clone();
    match &cli.command {
        Command::Enqueue(args) => curate::enqueue_command(cli, state, args).await,
        Command::TitlesSync(args) => {
            titles::titles_sync_command(cli, state, start, report, args).await
        }
        Command::Geocode => titles::geocode_command(cli).await,
        Command::TitlesArchive => titles::titles_archive_command(cli).await,
        Command::TitlesSeed => titles::titles_seed_command(cli).await,
        Command::SeedState(args) => onprem::seed_state_command(cli, state, args).await,
        Command::PublishStaged(args) => onprem::publish_staged_command(cli, state, args).await,
        Command::ImportStagedRun(args) => onprem::import_staged_run_command(cli, state, args).await,
        Command::MirrorStore(args) => onprem::mirror_store_command(args).await,
        Command::ArchiveCopy(args) => batches::archive_copy_command(args).await,
        Command::OcrDamage(args) => ocr::ocr_damage_command(cli, start, args).await,
        Command::OcrAgreement(args) => ocr::ocr_agreement_command(cli, start, args).await,
        Command::OcrPrefetchManifest(args) => ocr::ocr_prefetch_manifest_command(cli, args).await,
        Command::OcrSync(args) => ocr::ocr_sync_command(cli, state, start, args).await,
        Command::OcrSeed(args) => ocr::ocr_seed_command(cli, state, start, args).await,
        Command::OcrEnqueue(args) => ocr::ocr_enqueue_command(cli, state, args).await,
        Command::OcrRequeue(args) => ocr::ocr_requeue_command(cli, state, args).await,
        Command::CongressGlobe(args) => {
            congress::congress_globe_command(cli, state, start, args).await
        }
        Command::CongressSerialset(args) => {
            congress::congress_serialset_command(cli, state, start, args).await
        }
        Command::CongressDamage(args) => congress::congress_damage_command(cli, start, args).await,
        Command::Pictures(args) => pictures::pictures_command(cli, args).await,
        Command::Duplicates => batches::duplicates_command(cli, state).await,
        Command::Prune(args) => prune::prune_command(cli, state, args).await,
        Command::Curate(args) => curate::curate_command(cli, state, start, args).await,
        Command::Release(args) => release::release_command(cli, state, report, args).await,
        Command::ReleaseFinish(args) => {
            release::release_finish_command(cli, state, report, args).await
        }
        Command::Run(args) => run::run_command(cli, state, start, report, args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_how_a_command_ended() {
        let published = Published {
            index_version: "v1".into(),
            indexes: vec![],
            full: true,
            docs: 1,
            pages: 1,
        };
        assert_eq!(outcome(&Ok(Some(published))).0, Outcome::Published);
        assert_eq!(outcome(&Ok(None)), (Outcome::NothingNew, None));
        let left: anyhow::Error = TitlesLeft("titles-sync reached its deadline".into()).into();
        let (kind, error) = outcome(&Err(left.context("run")));
        assert_eq!(kind, Outcome::TitlesLeft);
        assert!(error.unwrap().contains("deadline"));
        let live: anyhow::Error = PublishedUnrecorded("v2 is live, but…".into()).into();
        let (kind, error) = outcome(&Err(live));
        assert_eq!(kind, Outcome::Published);
        assert!(error.is_some());
        let (kind, _) = outcome(&Err(anyhow::anyhow!("connection refused")));
        assert_eq!(kind, Outcome::Failed);
        // The failure line's outcome, which the job-failed alert reads.
        let left: anyhow::Error = TitlesLeft("titles-sync reached its deadline".into()).into();
        assert_eq!(failure_outcome(&left.context("run")), "titles_left");
        assert_eq!(
            failure_outcome(&anyhow::anyhow!("connection refused")),
            "failed"
        );
        assert_eq!(
            first_step(
                &Cli::try_parse_from([
                    "usnm-ingest",
                    "--curated",
                    "c",
                    "--reference",
                    "r",
                    "curate"
                ])
                .unwrap()
                .command
            ),
            None
        );
    }
}
