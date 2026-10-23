//! `release` and `release-finish`: building the indexes and the reference snapshot from the
//! curated batches and publishing them (`usnm_ingest::release`, `usnm_ingest::finish`).
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Args;
use usnm_ingest::activity::Reporter;
use usnm_ingest::merges::{self, MergeWait};
use usnm_ingest::owner_id;
use usnm_ingest::release::{self, Published, Release};
use usnm_ingest::sink::{IndexSink, IngestApi, JsonlSink, QuickwitNode, QuickwitSink};
use usnm_ingest::state::State;

use super::common::{root, IndexTarget, Stores};
use crate::Cli;

/// Build a new index and reference snapshot from curated batches, then publish.
#[derive(Args)]
pub(crate) struct ReleaseArgs {
    /// Rebuild everything into a new base index (compaction).
    #[arg(long)]
    full: bool,
    /// Label the version as synthetic demo data (the fixture batches).
    #[arg(long)]
    synthetic: bool,
    /// Index American Stories' text with LoC's (#218, 04 §4.9): what
    /// `jaocr.py american-stories-write` wrote for the finished years.
    /// The first release with it builds a full base. Keep it on once a
    /// version has the text: a release without it publishes a version
    /// whose searches leave the text out.
    #[arg(long)]
    american_stories: bool,
    /// Give the pages LoC ships without text a main-index document with
    /// the Latin-script text our Japanese OCR read on them (#203, 04
    /// §4.8), so English searches reach their English ads and sections.
    /// The release's new main index takes the pages no index of the
    /// version holds yet; without it, a full release leaves them out.
    #[arg(long)]
    ja_latin: bool,
    /// Weigh our own OCR from `ocr-local` against LoC's text (12 §12.5
    /// Step 9): on the pages of the batches this release indexes, ours
    /// replaces LoC's where rule v1 prefers it. Published batches take
    /// it at the next full release.
    #[arg(long)]
    ocr_local: bool,
    /// Search the Congress pages curated under `congress/` (#229, 04
    /// §4.10): the version gets a Congress index set, rebuilt when the
    /// curated units changed and kept otherwise. Off: no Congress set.
    #[arg(long, env = "USNM_CONGRESS")]
    congress: bool,
    /// Lay a full base out by decade (#123, 05 §5.5.5), so date-limited
    /// searches skip the other decades' splits. Only a full base takes
    /// it; deltas follow the published version.
    #[arg(long)]
    partition_decade: bool,
    /// Search a full base's texts, LoC's and American Stories', in one
    /// field (#283, 05 §5.5.7), so a search looks each word up once.
    /// Only a full base takes it; deltas follow the published version.
    #[arg(long)]
    single_text_field: bool,
    /// Give a full base's pages their own languages, found from their
    /// words (#289, 05 §5.5.8), and the version page-language baselines,
    /// so `lang=` filters by them. Only a full base takes it; deltas
    /// follow the published version.
    #[arg(long)]
    page_language: bool,
    /// Pages per split of a full base, in place of the index template's
    /// 30,000 (#283): fewer, larger splits for a cold search to open.
    /// Bounded by the writer's memory and scratch share (08 §8.4). Only
    /// a full base takes it; deltas keep the template's.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..usnm_ingest::sink::MAX_SPLIT_DOCS))]
    split_docs: Option<u64>,
    #[command(flatten)]
    target: IndexTarget,
}

/// Finish a release whose main index passed its exact count but that
/// failed later, e.g. when its merge wait ran out (#283): take up its
/// run and its index on the writer's data, replay the build without
/// sending anything, check the replay against the run's state, index and
/// log, then merge, seal and publish as the release would have
/// (docs/operations.md, "Finishing a release"). Give the release's own
/// settings: the run's build record must come out the same. How it
/// publishes isn't recorded, so it is said again: `--stage-index-storage`
/// as the release had it, or `--publish-serving`.
#[derive(Args)]
#[command(group(
    clap::ArgGroup::new("publication")
        .required(true)
        .args(["stage_index_storage", "publish_serving"])
))]
pub(crate) struct ReleaseFinishArgs {
    /// The run's version (`pages-v20261022-1`).
    #[arg(long)]
    run: String,
    /// The run's JSON log (on-prem, the run directory's `release.log`).
    #[arg(long)]
    release_log: PathBuf,
    /// Read the curated store's listings as of this time (RFC 3339)
    /// instead of the run's start.
    #[arg(long)]
    listed_before: Option<chrono::DateTime<chrono::Utc>>,
    /// The run was a release that writes the serving pointer
    /// (`current.json`), not a staged one (`--stage-index-storage`).
    #[arg(long)]
    publish_serving: bool,
    /// As for `release`.
    #[arg(long)]
    full: bool,
    /// As for `release`.
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
    #[command(flatten)]
    target: IndexTarget,
}

// The command's options, passed through.
#[allow(clippy::too_many_arguments)]
pub(super) async fn release(
    cli: &Stores,
    state: &State,
    full: bool,
    synthetic: bool,
    american_stories: bool,
    ja_latin: bool,
    ocr_local: bool,
    congress: bool,
    partition_decade: bool,
    single_text_field: bool,
    page_language: bool,
    split_docs: Option<u64>,
    titles_left: Option<String>,
    t: &IndexTarget,
    report: &Reporter,
) -> anyhow::Result<Option<Published>> {
    let r = Release {
        state: state.clone(),
        curated: usnm_store::open(&cli.curated)?,
        reference: usnm_store::open(&cli.reference)?,
        owner: owner_id(),
        full,
        stage_index_storage: t.stage_index_storage,
        synthetic,
        now: chrono::Utc::now(),
        titles_left,
        american_stories,
        ja_latin,
        ocr_local,
        congress,
        partition_decade,
        single_text_field,
        page_language,
        split_docs,
    };
    // Held from before the writer node starts until after it stops, and
    // released on every path.
    let lease = r.lock().await?;
    let result = release_locked(cli, &r, &lease, t, report).await;
    let unlocked = r.unlock(lease).await;
    let result = result.and_then(|p| unlocked.map(|()| p));
    let published = result?;
    match &published {
        Some(p) => {
            tracing::info!(version = %p.index_version, docs = p.docs, pages = p.pages, full = p.full, "released")
        }
        None => tracing::info!("nothing to release"),
    }
    Ok(published)
}

/// Start the index target (and writer node), release, stop the node.
async fn release_locked(
    cli: &Stores,
    r: &Release,
    lease: &usnm_ingest::release::WriterLease,
    t: &IndexTarget,
    report: &Reporter,
) -> anyhow::Result<Option<Published>> {
    let mut node = None;
    let wait = MergeWait {
        timeout: std::time::Duration::from_secs(
            t.merge_timeout_secs.unwrap_or(merges::DEFAULT_TIMEOUT_SECS),
        ),
        report: report.clone(),
        ..MergeWait::default()
    };
    let ingest = IngestApi::from_env()?;
    let mut sink: Box<dyn IndexSink> = match (&t.index_dir, &t.quickwit_url, &t.quickwit_bin) {
        (Some(dir), None, None) => Box::new(JsonlSink::new(dir)),
        (None, Some(_), None) if ingest == IngestApi::V1 => bail!(
            "{}=v1 restarts the writer for its final merges: use --quickwit-bin",
            IngestApi::VAR
        ),
        (None, Some(url), None) => Box::new(QuickwitSink::new(url, root(t)?)?.merges(wait)),
        (None, None, Some(bin)) => {
            let metastore = t
                .quickwit_metastore
                .as_deref()
                .context("--quickwit-bin needs --quickwit-metastore")?;
            let dir = cli.work_dir.join("quickwit");
            std::fs::create_dir_all(&dir)?;
            remove_stale_spill(&dir)?;
            check_free_disk(&dir, t.min_free_gib)?;
            // Before the writer starts: Kafka needs the v1 ingest API.
            let kafka = usnm_ingest::kafka::Kafka::from_env()?;
            ingest.check_kafka(kafka.is_some())?;
            let n = QuickwitNode::start(bin, &dir, t.quickwit_port, metastore, root(t)?).await?;
            tracing::info!(api = ingest.name(), "writer ingest api");
            let sink = QuickwitSink::new(&n.url, root(t)?)?
                .watching(&n)
                .merges(wait)
                .ingest(ingest)
                .kafka(kafka);
            node = Some(n);
            Box::new(sink)
        }
        _ => bail!("choose one of --index-dir, --quickwit-url or --quickwit-bin"),
    };
    let result = r.run_reporting(sink.as_mut(), lease, report).await;
    if let Some(n) = node {
        n.stop().await?;
    }
    result
}

/// The settings `release` and `release-finish` share.
struct ReleaseFlags {
    full: bool,
    synthetic: bool,
    american_stories: bool,
    ja_latin: bool,
    ocr_local: bool,
    congress: bool,
    partition_decade: bool,
    single_text_field: bool,
    page_language: bool,
    split_docs: Option<u64>,
}

/// `release-finish`: the run's evidence checked under the writer lock, then
/// the writer started on the run's data and the release finished
/// (`usnm_ingest::finish`), the node stopped, the lock released.
// The command's options, passed through.
#[allow(clippy::too_many_arguments)]
async fn release_finish(
    cli: &Stores,
    state: &State,
    flags: &ReleaseFlags,
    version: &str,
    log: &std::path::Path,
    listed_before: Option<chrono::DateTime<chrono::Utc>>,
    t: &IndexTarget,
    report: &Reporter,
) -> anyhow::Result<Option<Published>> {
    use usnm_ingest::finish::{AsOf, Resume};
    if t.quickwit_url.is_some() {
        bail!("release-finish runs its own writer on the run's data: use --quickwit-bin");
    }
    let reference = usnm_store::open(&cli.reference)?;
    let owner = owner_id();
    let lease = release::lock_writer(state, &owner).await?;
    let result = async {
        // Under the lock: no release is running, so a run still `building`
        // has stopped.
        let resume = Resume::load(state, reference.as_ref(), version, log).await?;
        let before = listed_before.unwrap_or(resume.run.started_at);
        tracing::info!(listed_before = %before, "reading the curated store's listings as of then");
        let r = Release {
            state: state.clone(),
            curated: Arc::new(AsOf::new(usnm_store::open(&cli.curated)?, before.into())),
            reference: reference.clone(),
            owner: owner.clone(),
            full: flags.full,
            stage_index_storage: t.stage_index_storage,
            synthetic: flags.synthetic,
            now: resume.run.started_at,
            titles_left: None,
            american_stories: flags.american_stories,
            ja_latin: flags.ja_latin,
            ocr_local: flags.ocr_local,
            congress: flags.congress,
            partition_decade: flags.partition_decade,
            single_text_field: flags.single_text_field,
            page_language: flags.page_language,
            split_docs: flags.split_docs,
        };
        let wait = MergeWait {
            timeout: std::time::Duration::from_secs(
                t.merge_timeout_secs
                    .unwrap_or(usnm_ingest::finish::DEFAULT_TIMEOUT_SECS),
            ),
            report: report.clone(),
            ..MergeWait::default()
        };
        match (&t.index_dir, &t.quickwit_bin) {
            (Some(dir), None) => {
                let mut sink = JsonlSink::new(dir);
                r.finish(&mut sink, &lease, report, &resume).await
            }
            (None, Some(bin)) => {
                let metastore = t
                    .quickwit_metastore
                    .as_deref()
                    .context("--quickwit-bin needs --quickwit-metastore")?;
                let ingest = IngestApi::from_env()?;
                let dir = cli.work_dir.join("quickwit");
                std::fs::create_dir_all(&dir)?;
                remove_stale_spill(&dir)?;
                check_free_disk(&dir, t.min_free_gib)?;
                let kafka = usnm_ingest::kafka::Kafka::from_env()?;
                ingest.check_kafka(kafka.is_some())?;
                let n =
                    QuickwitNode::start(bin, &dir, t.quickwit_port, metastore, root(t)?).await?;
                tracing::info!(api = ingest.name(), "writer ingest api");
                let mut sink = QuickwitSink::new(&n.url, root(t)?)?
                    .watching(&n)
                    .merges(wait)
                    .ingest(ingest)
                    .kafka(kafka);
                let result = r.finish(&mut sink, &lease, report, &resume).await;
                n.stop().await?;
                result
            }
            _ => bail!("choose one of --index-dir or --quickwit-bin"),
        }
    }
    .await;
    let unlocked = release::unlock_writer(state, &owner, lease).await;
    let published = result?;
    unlocked?;
    if let Some(p) = &published {
        tracing::info!(version = %p.index_version, docs = p.docs, pages = p.pages, full = p.full, "released");
    }
    Ok(published)
}

/// Remove the decade files (`decade_order::SPILL_DIR`) a release that
/// stopped left in `dir`, whatever this release builds: only a partitioned
/// base would remove them otherwise, and the free-disk check must see the
/// space they take.
fn remove_stale_spill(dir: &std::path::Path) -> anyhow::Result<()> {
    let spill = dir.join(usnm_ingest::decade_order::SPILL_DIR);
    if spill.exists() {
        tracing::info!(dir = %spill.display(), "removing a previous release's decade files");
        std::fs::remove_dir_all(&spill).with_context(|| format!("removing {}", spill.display()))?;
    }
    Ok(())
}

/// Fail before indexing anything if `dir` has less than `min_gib` free. A
/// previous writer's data in `dir/qwdata` is about to be removed, so it
/// counts as free.
fn check_free_disk(dir: &std::path::Path, min_gib: u64) -> anyhow::Result<()> {
    if min_gib == 0 {
        return Ok(());
    }
    let free = usnm_ingest::progress::disk_free(dir)
        .with_context(|| format!("reading the free space of {}", dir.display()))?;
    let stale = dir_size(&dir.join("qwdata"));
    let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    if free.saturating_add(stale) < min_gib.saturating_mul(1 << 30) {
        bail!(
            "{} has {:.1} GiB free, under the {min_gib} GiB a release needs for indexing and \
             merges (08 §8.4); is the scratch volume mounted?",
            dir.display(),
            gib(free + stale)
        );
    }
    tracing::info!(dir = %dir.display(), free_gib = gib(free + stale).round(), "work disk");
    Ok(())
}

/// Bytes in the files under `path` (0 if it doesn't exist).
fn dir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

pub(crate) async fn release_command(
    cli: &Cli,
    state: State,
    report: &Reporter,
    args: &ReleaseArgs,
) -> anyhow::Result<Option<Published>> {
    let ReleaseArgs {
        full,
        synthetic,
        american_stories,
        ja_latin,
        ocr_local,
        congress,
        partition_decade,
        single_text_field,
        page_language,
        split_docs,
        target,
    } = args;
    release(
        &cli.stores,
        &state,
        *full,
        *synthetic,
        *american_stories,
        *ja_latin,
        *ocr_local,
        *congress,
        *partition_decade,
        *single_text_field,
        *page_language,
        *split_docs,
        None,
        target,
        report,
    )
    .await
}

pub(crate) async fn release_finish_command(
    cli: &Cli,
    state: State,
    report: &Reporter,
    args: &ReleaseFinishArgs,
) -> anyhow::Result<Option<Published>> {
    let ReleaseFinishArgs {
        run,
        release_log,
        listed_before,
        publish_serving: _,
        full,
        synthetic,
        american_stories,
        ja_latin,
        ocr_local,
        congress,
        partition_decade,
        single_text_field,
        page_language,
        split_docs,
        target,
    } = args;
    let flags = ReleaseFlags {
        full: *full,
        synthetic: *synthetic,
        american_stories: *american_stories,
        ja_latin: *ja_latin,
        ocr_local: *ocr_local,
        congress: *congress,
        partition_decade: *partition_decade,
        single_text_field: *single_text_field,
        page_language: *page_language,
        split_docs: *split_docs,
    };
    release_finish(
        &cli.stores,
        &state,
        &flags,
        run,
        release_log,
        *listed_before,
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
    fn a_writer_needs_its_scratch_disk() {
        let dir = tempfile::tempdir().unwrap();
        check_free_disk(dir.path(), 0).unwrap();
        check_free_disk(dir.path(), 1).unwrap();
        // More than any test machine has: the volume isn't mounted.
        let err = check_free_disk(dir.path(), 1 << 30)
            .unwrap_err()
            .to_string();
        assert!(err.contains("scratch volume"), "{err}");
    }

    #[test]
    fn a_previous_releases_decade_files_are_removed_first() {
        let dir = tempfile::tempdir().unwrap();
        let spill = dir.path().join(usnm_ingest::decade_order::SPILL_DIR);
        std::fs::create_dir_all(&spill).unwrap();
        std::fs::write(spill.join("1890.jsonl.zst"), vec![0u8; 3000]).unwrap();
        remove_stale_spill(dir.path()).unwrap();
        assert!(!spill.exists());
        // Nothing to remove is fine too.
        remove_stale_spill(dir.path()).unwrap();
    }

    #[test]
    fn a_previous_writers_data_counts_as_free() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("qwdata/wal");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("a"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.path().join("qwdata/b"), vec![0u8; 500]).unwrap();
        assert_eq!(dir_size(&dir.path().join("qwdata")), 3500);
        assert_eq!(dir_size(&dir.path().join("missing")), 0);
    }

    #[test]
    fn merge_and_disk_settings_default() {
        let cli = Cli::try_parse_from([
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "release",
            "--index-dir",
            "x",
        ])
        .unwrap();
        let Command::Release(ReleaseArgs {
            target,
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
            panic!("not a release");
        };
        assert_eq!(split_docs, None, "the template's unless asked for");
        assert!(!american_stories, "off unless asked for");
        assert!(!ja_latin, "off unless asked for");
        assert!(!ocr_local, "off unless asked for");
        assert!(!congress, "off unless asked for");
        assert!(!partition_decade, "off unless asked for");
        assert!(!single_text_field, "off unless asked for");
        assert!(!page_language, "off unless asked for");
        // Each command's own default: 4 hours for a release.
        assert_eq!(target.merge_timeout_secs, None);
        assert_eq!(target.min_free_gib, 0);
    }

    #[test]
    fn release_finish_arguments() {
        let cli = Cli::try_parse_from([
            "usnm-ingest",
            "--state-file",
            "s",
            "--curated",
            "c",
            "--reference",
            "r",
            "release-finish",
            "--run",
            "pages-v20261022-1",
            "--release-log",
            "/srv/run/release.log",
            "--full",
            "--partition-decade",
            "--split-docs",
            "60000",
            "--stage-index-storage",
            "--quickwit-bin",
            "qw",
        ])
        .unwrap();
        let Command::ReleaseFinish(ReleaseFinishArgs {
            run,
            release_log,
            listed_before,
            full,
            partition_decade,
            split_docs,
            target,
            ..
        }) = cli.command
        else {
            panic!("not a finish");
        };
        assert_eq!(run, "pages-v20261022-1");
        assert_eq!(release_log, PathBuf::from("/srv/run/release.log"));
        assert_eq!(listed_before, None);
        assert!(full && partition_decade);
        assert_eq!(split_docs, Some(60000));
        assert_eq!(target.merge_timeout_secs, None);
        assert!(target.stage_index_storage);
        // How the run publishes is said, once: the run doesn't record it, and
        // a staged run finished as a serving one would write current.json.
        let finish = |publication: &[&'static str]| {
            let mut args = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "release-finish",
                "--run",
                "pages-v20261022-1",
                "--release-log",
                "l",
            ];
            args.extend_from_slice(publication);
            Cli::try_parse_from(args)
        };
        assert!(finish(&[]).is_err());
        assert!(finish(&["--stage-index-storage", "--publish-serving"]).is_err());
        assert!(finish(&["--publish-serving"]).is_ok());
        // Named flags only.
        assert!(Cli::try_parse_from([
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "release-finish",
            "pages-v20261022-1",
        ])
        .is_err());
    }
}
