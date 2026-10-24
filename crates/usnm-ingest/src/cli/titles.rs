//! `titles-sync`, `geocode`, `titles-archive` and `titles-seed`: LoC's title records and the
//! catalog built from them.
use std::collections::BTreeSet;

use anyhow::Context;
use clap::Args;
use usnm_ingest::activity::Reporter;
use usnm_ingest::release::{Published, TitlesLeft};
use usnm_ingest::source;
use usnm_ingest::state::{BatchStatus, State};
use usnm_ingest::titles;

use super::common::{deadline, read_list, Stores};
use crate::Cli;

/// Fetch LoC's records for titles with pages, then rebuild the catalog (`geocode`).
#[derive(Args)]
pub(crate) struct TitlesSyncArgs {
    /// Titles to fetch: those named in this batch list (LoC's listing
    /// names them), plus those in curated batches.
    #[arg(long, default_value = source::LOC_DATASETS)]
    list: String,
    /// Only these LCCNs instead.
    #[arg(long, value_delimiter = ',')]
    lccns: Vec<String>,
    /// Re-fetch records that are already cached.
    #[arg(long)]
    refresh: bool,
    #[arg(long, default_value = titles::LOC_ITEMS)]
    items: String,
    /// Seconds after the start to stop. Until then, when LoC rate limits
    /// the sync, it sends nothing for 65 minutes and resumes more slowly;
    /// without it, the first block stops the sync. Either way the
    /// command fails if titles are left, and the next run continues.
    #[arg(long)]
    max_runtime_secs: Option<u64>,
}

/// Fetch missing title records for `listed` titles and every curated batch's
/// titles, then rebuild the catalog from what is cached. The report says
/// whether the sync stopped early (rate limited, or at `deadline`), keeping
/// what it fetched.
pub(super) async fn titles_sync(
    cli: &Stores,
    state: &State,
    listed: impl IntoIterator<Item = String>,
    refresh: bool,
    items: &str,
    deadline: Option<tokio::time::Instant>,
    report: &Reporter,
) -> anyhow::Result<titles::SyncReport> {
    let mut lccns: BTreeSet<String> = listed.into_iter().collect();
    for (b, _) in state.batches(&[BatchStatus::Curated]).await? {
        lccns.extend(b.curated.into_iter().flat_map(|c| c.lccns));
    }
    let reference = usnm_store::open(&cli.reference)?;
    let report = titles::sync(
        reference.as_ref(),
        &lccns,
        &titles::title_records()?,
        refresh,
        items,
        &titles::Pacing::loc(deadline).reporting(report.clone()),
    )
    .await?;
    tracing::info!(?report, "title records");
    geocode(reference.as_ref()).await?;
    // Keep what the sync has outside the environment (#275). A failed copy
    // doesn't fail the sync: the next run tries again.
    if let Some(raw) = &cli.raw {
        if let Err(e) = titles_archive(reference.as_ref(), raw).await {
            tracing::warn!(error = %format!("{e:#}"), "the title records weren't copied to the raw store");
        }
    }
    Ok(report)
}

/// Copy the record cache and catalog to the raw store (`titles/{time}.json`).
async fn titles_archive(reference: &dyn usnm_store::ObjectStore, raw: &str) -> anyhow::Result<()> {
    let saved = usnm_ingest::titles_archive::save(
        reference,
        usnm_store::open(raw)?.as_ref(),
        chrono::Utc::now(),
    )
    .await?;
    tracing::info!(?saved, "title records in the raw store");
    Ok(())
}

/// What titles-sync left undone, if anything: it stopped early, or some
/// fetches failed (retried next run). Titles LoC doesn't have (404) count
/// as done: no run will find them.
pub(super) fn titles_left(report: &titles::SyncReport) -> Option<String> {
    if !report.finished() {
        return Some(unfinished(report));
    }
    (!report.failed.is_empty()).then(|| {
        format!(
            "titles-sync couldn't fetch {} of {} titles ({}); the next run retries them",
            report.failed.len(),
            report.wanted,
            report
                .failed
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Why titles-sync stopped early, for an error or a warning.
fn unfinished(report: &titles::SyncReport) -> String {
    let why = if report.throttled {
        "LoC rate limited titles-sync"
    } else {
        "titles-sync reached its deadline"
    };
    format!(
        "{why} with {} of {} titles left; the cache kept what was fetched, and the next run continues",
        report.left, report.wanted
    )
}

async fn geocode(reference: &dyn usnm_store::ObjectStore) -> anyhow::Result<()> {
    let report = titles::geocode(reference).await?;
    if !report.unresolved.is_empty() {
        tracing::warn!(unresolved = ?report.unresolved, "titles with no state were left out");
    }
    if report.gazetteer_over_loc > 0 {
        tracing::info!(
            places = report.gazetteer_over_loc,
            list = ?report.gazetteer_over_loc_places,
            "LoC points more than 100 km from the gazetteer's town of the name (not ambiguous); the gazetteer's used"
        );
    }
    if report.disagreements > 0 {
        tracing::warn!(
            places = report.disagreements,
            furthest = ?report.disagreement_examples,
            "LoC points more than 25 km from the gazetteer, kept; review them (catalog/overrides/places.json)"
        );
    }
    if report.loc_apart > 0 {
        tracing::warn!(
            places = report.loc_apart,
            furthest = ?report.loc_apart_examples,
            "places at the median of LoC points more than 25 km apart; review them (catalog/overrides/places.json)"
        );
    }
    tracing::info!(?report, "catalog");
    Ok(())
}

pub(crate) async fn titles_sync_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    report: &Reporter,
    args: &TitlesSyncArgs,
) -> anyhow::Result<Option<Published>> {
    let TitlesSyncArgs {
        list,
        lccns,
        refresh,
        items,
        max_runtime_secs,
    } = args;
    let listed = if lccns.is_empty() {
        read_list(list, cli.stores.raw.as_deref())
            .await?
            .into_iter()
            .flat_map(|b| b.lccns)
            .collect()
    } else {
        lccns.clone()
    };
    let synced = titles_sync(
        &cli.stores,
        &state,
        listed,
        *refresh,
        items,
        deadline(start, *max_runtime_secs),
        report,
    )
    .await?;
    if !synced.finished() {
        // Fail, so the job reports it and a later run continues.
        return Err(TitlesLeft(unfinished(&synced)).into());
    }
    Ok(None)
}

pub(crate) async fn geocode_command(cli: &Cli) -> anyhow::Result<Option<Published>> {
    geocode(usnm_store::open(&cli.stores.reference)?.as_ref())
        .await
        .map(|()| None)
}

pub(crate) async fn titles_archive_command(cli: &Cli) -> anyhow::Result<Option<Published>> {
    let raw = cli
        .stores
        .raw
        .as_deref()
        .context("titles-archive needs USNM_RAW_URL")?;
    titles_archive(usnm_store::open(&cli.stores.reference)?.as_ref(), raw)
        .await
        .map(|()| None)
}

pub(crate) async fn titles_seed_command(cli: &Cli) -> anyhow::Result<Option<Published>> {
    let raw = cli
        .stores
        .raw
        .as_deref()
        .context("titles-seed needs USNM_RAW_URL")?;
    let seeded = usnm_ingest::titles_archive::seed(
        usnm_store::open(raw)?.as_ref(),
        usnm_store::open(&cli.stores.reference)?.as_ref(),
    )
    .await?;
    match (&seeded.from, seeded.written.is_empty()) {
        (None, _) => {
            tracing::warn!("no title records in the raw store to start from; run titles-sync")
        }
        (Some(_), true) => tracing::info!(
            ?seeded,
            "this environment has its own title records; left as they are"
        ),
        (Some(_), false) => tracing::info!(?seeded, "title records seeded"),
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_why_titles_sync_stopped() {
        let report = titles::SyncReport {
            wanted: 3122,
            fetched: 342,
            throttled: true,
            left: 2779,
            ..Default::default()
        };
        assert!(unfinished(&report)
            .starts_with("LoC rate limited titles-sync with 2779 of 3122 titles left"));
        let late = titles::SyncReport {
            throttled: false,
            out_of_time: true,
            ..report
        };
        assert!(unfinished(&late).starts_with("titles-sync reached its deadline"));
        assert!(titles_left(&late).is_some());

        let done = titles::SyncReport {
            wanted: 3,
            fetched: 2,
            not_found: vec!["sn3".into()],
            ..Default::default()
        };
        assert_eq!(titles_left(&done), None);
        let failed = titles::SyncReport {
            failed: vec!["sn2".into()],
            ..done
        };
        assert_eq!(
            titles_left(&failed).unwrap(),
            "titles-sync couldn't fetch 1 of 3 titles (sn2); the next run retries them"
        );
    }
}
