//! `run`: enqueue (if a list is given), curate everything queued, then release.
use clap::Args;
use usnm_ingest::activity::Reporter;
use usnm_ingest::release::Published;
use usnm_ingest::source;
use usnm_ingest::state::{State, Step};
use usnm_ingest::titles;
use usnm_ingest::worker;

use super::common::{deadline, IndexTarget};
use super::curate::{curate, enqueue};
use super::release::release;
use super::titles::{titles_left, titles_sync};
use crate::Cli;

/// Enqueue (if a list is given), curate everything queued, then release.
#[derive(Args)]
pub(crate) struct RunArgs {
    /// As for `enqueue`: LoC's listing unless another list is given.
    #[arg(long, default_value = source::LOC_DATASETS)]
    list: String,
    /// Only these batches from the list.
    #[arg(long, value_delimiter = ',')]
    batches: Vec<String>,
    #[arg(long)]
    full: bool,
    #[arg(long)]
    synthetic: bool,
    /// As for `release`.
    #[arg(long)]
    american_stories: bool,
    /// As for `release`.
    #[arg(long)]
    ja_latin: bool,
    /// As for `release`.
    #[arg(long)]
    ocr_local: bool,
    /// As for `release`.
    #[arg(long, env = "USNM_CONGRESS")]
    congress: bool,
    /// Include manually acquired Annals and Register after operator QA.
    #[arg(long, env = "USNM_CONGRESS_MANUAL", requires = "congress")]
    congress_manual: bool,
    /// As for `release`.
    #[arg(long)]
    partition_decade: bool,
    /// As for `release`.
    #[arg(long)]
    single_text_field: bool,
    /// As for `release`.
    #[arg(long)]
    page_language: bool,
    /// As for `release`.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..usnm_ingest::sink::MAX_SPLIT_DOCS))]
    split_docs: Option<u64>,
    /// As `curate --max-runtime-secs`, counted from the start of `run`:
    /// batches still queued then wait for the next run, and what was
    /// curated is released. Leave room for titles-sync and the release
    /// under the job's replica timeout.
    #[arg(long)]
    curate_max_runtime_secs: Option<u64>,
    /// As `titles-sync --max-runtime-secs`, counted from the start of
    /// `run`. A full release (asked for, or forced) goes ahead only once
    /// titles-sync has fetched every title LoC has, so a rebuild leaves
    /// no batch out for a missing title; otherwise it fails and the next
    /// run continues the sync.
    #[arg(long)]
    titles_max_runtime_secs: Option<u64>,
    #[command(flatten)]
    target: IndexTarget,
}

pub(crate) async fn run_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    report: &Reporter,
    args: &RunArgs,
) -> anyhow::Result<Option<Published>> {
    let RunArgs {
        list,
        batches,
        full,
        synthetic,
        american_stories,
        ja_latin,
        ocr_local,
        congress,
        congress_manual,
        partition_decade,
        single_text_field,
        page_language,
        split_docs,
        curate_max_runtime_secs,
        titles_max_runtime_secs,
        target,
    } = args;
    // Before hours of curation and titles-sync: the release would refuse.
    if *american_stories {
        let curated = usnm_store::open(&cli.stores.curated)?;
        usnm_ingest::american_stories::check_written(curated.as_ref()).await?;
    }
    let listed = enqueue(&state, list, batches, false, cli.stores.raw.as_deref()).await?;
    report.step(Step::Downloading).await;
    curate(
        &cli.stores,
        &state,
        None,
        worker::FETCH_INTERVAL_SECS,
        deadline(start, *curate_max_runtime_secs),
    )
    .await?;
    // Every curated title needs a catalog entry before release.
    report.step(Step::Titles).await;
    let lccns = listed.into_iter().flat_map(|b| b.lccns);
    let synced = titles_sync(
        &cli.stores,
        &state,
        lccns,
        false,
        titles::LOC_ITEMS,
        deadline(start, *titles_max_runtime_secs),
        report,
    )
    .await?;
    // The release decides: a delta goes ahead, a full base (asked
    // for or forced) refuses (`Release::titles_left`).
    let titles_left = titles_left(&synced);
    report.step(Step::Indexing).await;
    release(
        &cli.stores,
        &state,
        *full,
        *synthetic,
        *american_stories,
        *ja_latin,
        *ocr_local,
        *congress,
        *congress_manual,
        *partition_decade,
        *single_text_field,
        *page_language,
        *split_docs,
        titles_left,
        target,
        report,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;

    #[test]
    fn run_takes_a_titles_deadline() {
        let cli = Cli::try_parse_from([
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "run",
            "--full",
            "--american-stories",
            "--ja-latin",
            "--ocr-local",
            "--congress",
            "--partition-decade",
            "--single-text-field",
            "--page-language",
            "--split-docs",
            "120000",
            "--titles-max-runtime-secs",
            "28800",
            "--index-dir",
            "x",
        ])
        .unwrap();
        let Command::Run(RunArgs {
            titles_max_runtime_secs,
            full,
            american_stories,
            ja_latin,
            ocr_local,
            congress,
            partition_decade,
            single_text_field,
            page_language,
            split_docs,
            ..
        }) = cli.command
        else {
            panic!("not a run");
        };
        assert_eq!(
            (
                titles_max_runtime_secs,
                full,
                american_stories,
                ja_latin,
                ocr_local,
                congress,
                partition_decade,
                single_text_field,
                page_language,
                split_docs
            ),
            (
                Some(28800),
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                Some(120_000)
            )
        );
        // A split target of 0, or of a million pages or more, is refused.
        for bad in ["0", "1000000", "-1"] {
            assert!(
                Cli::try_parse_from([
                    "usnm-ingest",
                    "--curated",
                    "c",
                    "--reference",
                    "r",
                    "release",
                    "--split-docs",
                    bad,
                    "--index-dir",
                    "x",
                ])
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn manual_congress_run_requires_the_congress_index() {
        let base = [
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "run",
            "--index-dir",
            "x",
        ];
        let mut without_congress = base.to_vec();
        without_congress.push("--congress-manual");
        assert!(Cli::try_parse_from(without_congress).is_err());

        let mut enabled = base.to_vec();
        enabled.extend(["--congress", "--congress-manual"]);
        let cli = Cli::try_parse_from(enabled).unwrap();
        let Command::Run(RunArgs {
            congress,
            congress_manual,
            ..
        }) = cli.command
        else {
            panic!("not a run");
        };
        assert!(congress && congress_manual);
    }
}
