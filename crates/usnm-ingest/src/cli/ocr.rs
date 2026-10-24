//! The `ocr-*` commands that exchange work with the OCR nodes (12 §12.5): `ocr-damage`,
//! `ocr-prefetch-manifest`, `ocr-sync`, `ocr-seed`, `ocr-enqueue` and `ocr-requeue`. The OCR
//! pipeline itself is `usnm_ingest::ocr_local` (the `usnm-ocr-local` crate).
use anyhow::Context;
use clap::Args;
use usnm_ingest::owner_id;
use usnm_ingest::release::Published;
use usnm_ingest::state::State;

use super::common::deadline;
use crate::Cli;

/// Why a page is queued for OCR.
#[derive(Clone, Copy, clap::ValueEnum)]
enum OcrReason {
    /// A page someone was shown (`ocr-priority`).
    User,
    /// The damage-ranked backlog (`ocr-bulk`).
    Bulk,
}

impl From<OcrReason> for usnm_ocr_contract::message::Reason {
    fn from(r: OcrReason) -> Self {
        match r {
            OcrReason::User => Self::User,
            OcrReason::Bulk => Self::Bulk,
        }
    }
}

/// The seeder's settings (12 §12.5 Step 10), for `ocr-sync`'s seeding
/// step and `ocr-seed`. Seeding is off while the plan is empty.
#[derive(Args, Clone, Debug)]
struct SeedArgs {
    /// The seeding plan, as the ledger records it (`pilot-deu`). Empty: off.
    #[arg(long, env = "USNM_OCR_SEED_PLAN")]
    seed_plan: Option<String>,
    /// Only these batches (base names or `name_verNN`), comma separated.
    #[arg(long, env = "USNM_OCR_SEED_BATCHES", value_delimiter = ',')]
    seed_batches: Vec<String>,
    /// ISO 639-3 languages to seed (the page's detected language, or its
    /// title's first for `und`), comma separated. Default: the Tesseract
    /// pilot's, ces, deu, eng, pol, rus and srp.
    #[arg(long, env = "USNM_OCR_SEED_LANGUAGES", value_delimiter = ',')]
    seed_languages: Vec<String>,
    /// Only issues in these years: `1890` or `1850-1919`.
    #[arg(long, env = "USNM_OCR_SEED_YEARS")]
    seed_years: Option<String>,
    /// The least damage a page needs to be queued.
    #[arg(long, env = "USNM_OCR_SEED_MIN_DAMAGE", default_value_t = usnm_ingest::ocr_local::seed::MIN_DAMAGE)]
    seed_min_damage: f64,
    /// The plan's budget: the most distinct pages it ever queues.
    #[arg(long, env = "USNM_OCR_SEED_MAX_PAGES")]
    seed_max_pages: Option<u64>,
    /// A pilot: one group (one language, or `--seed-batches`) and a budget
    /// of at most 10,000 pages.
    #[arg(long, env = "USNM_OCR_SEED_PILOT")]
    seed_pilot: bool,
    /// Also skip pages whose American Stories text scores under the
    /// minimum damage (12 §12.6 question 8; off by default).
    #[arg(long, env = "USNM_OCR_SEED_SKIP_AMERICAN_STORIES")]
    seed_skip_american_stories: bool,
    /// The most bulk pages outstanding at once. 0: about two days of pages,
    /// from the import records, at least 500.
    #[arg(long, env = "USNM_OCR_BULK_WINDOW", default_value_t = 0)]
    bulk_window: u64,
    /// Top `ocr-bulk` up only when it holds fewer messages than this.
    /// 0: half the window.
    #[arg(long, env = "USNM_OCR_BULK_REFILL_BELOW", default_value_t = 0)]
    bulk_refill_below: u64,
    /// Where new pages come from: `damage` (units ranked from the damage
    /// tables) or `fetched` (the prefetch manifest's keep set in the
    /// fetcher's order, only pages whose scans are on pve-big, from the
    /// newest list a node published to `ocr-out/fetched/`; 12 Step 11).
    #[arg(long, env = "USNM_OCR_SEED_SOURCE", default_value = "damage")]
    seed_source: usnm_ingest::ocr_local::seed::Source,
    /// With `--seed-source fetched`: the prefetch manifest's index version.
    /// Default: the published version.
    #[arg(long, env = "USNM_OCR_SEED_MANIFEST")]
    seed_manifest: Option<String>,
}

impl SeedArgs {
    fn options(&self, dry_run: bool) -> anyhow::Result<usnm_ingest::ocr_local::seed::Options> {
        let opts = usnm_ingest::ocr_local::seed::Options {
            plan: self.seed_plan.clone().unwrap_or_default().trim().to_owned(),
            batches: self
                .seed_batches
                .iter()
                .map(|b| b.trim().to_owned())
                .filter(|b| !b.is_empty())
                .collect(),
            languages: self
                .seed_languages
                .iter()
                .map(|l| l.trim().to_owned())
                .filter(|l| !l.is_empty())
                .collect(),
            years: usnm_ingest::ocr_local::seed::parse_years(
                self.seed_years.as_deref().unwrap_or_default(),
            )?,
            min_damage: self.seed_min_damage,
            max_pages: self.seed_max_pages,
            window: self.bulk_window,
            refill_below: self.bulk_refill_below,
            pilot: self.seed_pilot,
            skip_american_stories: self.seed_skip_american_stories,
            dry_run,
            deadline: None,
            source: self.seed_source,
            manifest: self
                .seed_manifest
                .clone()
                .unwrap_or_default()
                .trim()
                .to_owned(),
        };
        if opts.on() {
            opts.check()?;
        }
        Ok(opts)
    }
}

/// Score every curated page of an index version with the OCR damage
/// metric (12 §12.5 Step 6, #296): tables under
/// `audit/damage/v<VERSION>/` in the curated store, one per batch
/// version, skipping those already scored. With `--sample-pct` and
/// `--compare-csv`, the parity gate against the Python audit instead
/// (writes no tables); with `--parity`, a parity file from
/// `jaocr.py damage-parity`. Logs counts and doc_ids, never text.
#[derive(Args)]
pub(crate) struct OcrDamageArgs {
    /// The index version whose batches to score (`{version}/batches.json`).
    #[arg(
        long = "version",
        value_name = "INDEX_VERSION",
        required_unless_present = "parity"
    )]
    index_version: Option<String>,
    /// Only these batches (`name` or `name_verNN`). The version's list
    /// of tables is written only by a run over every batch.
    #[arg(long, value_delimiter = ',')]
    batches: Vec<String>,
    /// The gate: score only the pages the audit samples at this
    /// percentage, and compare them with `--compare-csv`.
    #[arg(long, requires_all = ["index_version", "compare_csv"])]
    sample_pct: Option<f64>,
    /// The audit's per-page CSV (`.csv.gz`, a curated path) to compare the sample with.
    #[arg(long, requires = "sample_pct")]
    compare_csv: Option<String>,
    /// A parity file (`.jsonl.zst`, a curated path) to score again.
    #[arg(long)]
    parity: Option<String>,
    /// Seconds after the start to stop starting batches; the next run
    /// continues. Keep it under the job's replica timeout.
    #[arg(long)]
    max_runtime_secs: Option<u64>,
    /// Batches scored at once (default: the CPUs).
    #[arg(long)]
    parallel: Option<usize>,
}

/// The scan prefetch manifest (#368): the pages of an index version
/// above `--min-damage` in the damage tables, as Parquet in issue
/// order, and a summary by language, decade and batch, under
/// `audit/damage/v2/prefetch/<version>/` in the curated store. Logs
/// counts, never text.
#[derive(Args)]
pub(crate) struct OcrPrefetchManifestArgs {
    /// The index version whose damage tables to read.
    #[arg(long = "version", value_name = "INDEX_VERSION")]
    index_version: String,
    /// Before the damage pass has listed every table: read the batches
    /// that have theirs, into `partial-<stamp>/`.
    #[arg(long)]
    partial: bool,
    /// Pages with damage above this go in.
    #[arg(long, default_value_t = usnm_core::damage::BADLY_DAMAGED)]
    min_damage: f64,
    /// The exchange account's `ocr-out` container (a Blob container URL
    /// or a directory): also copy the manifest and summary to
    /// `manifests/<version>/` there, for the nodes.
    #[arg(long)]
    exchange_url: Option<String>,
}

/// Pick up the OCR nodes' output (12 §12.5 Step 8, #296): drain the
/// dead letters, check each part in the exchange account's `ocr-out`
/// and its ALTO, import what passes into `ocr-local/` in the curated
/// store, move the rest to `ocr-quarantine`, delete the sources and
/// write `status/ocr-local.json`. Holds the `ocr-sync` lock. Logs page
/// ids and reason codes, never text.
#[derive(Args)]
pub(crate) struct OcrSyncArgs {
    /// The exchange account's queue endpoint (`OCR_QUEUE_ENDPOINT`).
    #[arg(long, env = "USNM_OCR_QUEUE_URL")]
    queue_url: Option<String>,
    /// The exchange account's `ocr-out` container: a Blob container URL or a directory.
    #[arg(long, env = "USNM_OCR_OUT_URL")]
    out_url: Option<String>,
    /// The exchange account's `ocr-quarantine` container: a Blob container URL or a directory.
    #[arg(long, env = "USNM_OCR_QUARANTINE_URL")]
    quarantine_url: Option<String>,
    /// The API's daily page-id-only request container.
    #[arg(long, env = "USNM_OCR_REQUESTS_URL")]
    requests_url: Option<String>,
    /// The most parts checked in one run; the rest wait for the next.
    #[arg(long, default_value_t = 2000)]
    max_parts: usize,
    /// Parts per import (each import writes and deletes on its own).
    #[arg(long, default_value_t = 200)]
    chunk: usize,
    /// The most dead letters drained in one run.
    #[arg(long, default_value_t = 5000)]
    max_dead_letters: usize,
    /// Maximum distinct hit-request pages queued per UTC request day.
    #[arg(long, env = "USNM_OCR_USER_DAILY_MAX", default_value_t = 1000)]
    user_daily_max: usize,
    /// Only apply rule v1 to the stored scores of all of `ocr-local`
    /// and write `ocr-local/report/<run>.json`; picks nothing up.
    #[arg(long, conflicts_with = "from_quarantine")]
    report: bool,
    /// First put the rejects with `--reason` back where they came
    /// from, so this run checks them again.
    #[arg(long, requires = "reason")]
    from_quarantine: bool,
    /// The reject reason code to replay (as in the quarantine records).
    #[arg(long, requires = "from_quarantine")]
    reason: Option<String>,
    /// Seconds after the start to stop starting imports. Keep it under
    /// the job's replica timeout.
    #[arg(long)]
    max_runtime_secs: Option<u64>,
    /// With a seed plan, top `ocr-bulk` up after the imports (Step 10).
    #[command(flatten)]
    seed: SeedArgs,
}

/// Fill `ocr-bulk` with the pages LoC read worst, ranked by Step 6's
/// damage tables (12 §12.5 Step 10), as `ocr-sync`'s seeding step does,
/// once. `--dry-run` prints what it would queue as JSON and writes
/// nothing; otherwise it holds the `ocr-sync` lock. Run as an execution
/// of the pickup job: only `id-usnm-ingest` can add messages.
#[derive(Args)]
pub(crate) struct OcrSeedArgs {
    #[arg(long, env = "USNM_OCR_QUEUE_URL")]
    queue_url: String,
    /// The exchange account's `ocr-out`, for `--seed-source fetched`.
    #[arg(long, env = "USNM_OCR_OUT_URL")]
    out_url: Option<String>,
    #[command(flatten)]
    seed: SeedArgs,
    /// Print what would be queued; write and send nothing.
    #[arg(long)]
    dry_run: bool,
    /// Seconds after the start to stop working out ranks; the next run
    /// goes on from there.
    #[arg(long)]
    max_runtime_secs: Option<u64>,
}

/// Queue chosen pages for OCR (12 §12.5 Step 8): finds each page in the
/// published version, writes a ledger row for it and puts its message
/// on `ocr-priority` (`--reason user`) or `ocr-bulk` (`bulk`). Run as
/// an ingest job execution: only `id-usnm-ingest` can add messages.
#[derive(Args)]
pub(crate) struct OcrEnqueueArgs {
    #[arg(long, env = "USNM_OCR_QUEUE_URL")]
    queue_url: String,
    /// Page doc_ids, comma separated (`sn83045462_1890-05-03_ed-1_seq-7`).
    #[arg(long, value_delimiter = ',', required = true)]
    pages: Vec<String>,
    #[arg(long, value_enum)]
    reason: OcrReason,
    /// The plan name recorded in the ledger.
    #[arg(long, default_value = "manual")]
    plan: String,
    /// Look for the pages only in this base batch.
    #[arg(long)]
    batch: Option<String>,
    /// Queue pages the ledger already has outstanding: after a run that
    /// wrote its ledger rows but failed to put some messages.
    #[arg(long)]
    include_outstanding: bool,
    /// Count what would be queued; write and send nothing.
    #[arg(long)]
    dry_run: bool,
}

/// Queue dead-lettered pages again by their cause and kind (12 §12.5
/// Step 8), from the dead letters `ocr-sync` drained into `ocr-local`.
#[derive(Args)]
pub(crate) struct OcrRequeueArgs {
    #[arg(long, env = "USNM_OCR_QUEUE_URL")]
    queue_url: String,
    /// `dequeue_count`, `invalid`, `permanent` or `unroutable`.
    #[arg(long)]
    cause: String,
    /// For `invalid`: the check that failed.
    #[arg(long)]
    kind: Option<String>,
    /// Only dead letters drained on or after this UTC day (YYYY-MM-DD).
    #[arg(long)]
    since: Option<chrono::NaiveDate>,
    /// The plan name recorded in the ledger.
    #[arg(long, default_value = "requeue")]
    plan: String,
    #[arg(long, default_value_t = 1000)]
    max_pages: usize,
    #[arg(long)]
    dry_run: bool,
}

pub(crate) async fn ocr_damage_command(
    cli: &Cli,
    start: tokio::time::Instant,
    args: &OcrDamageArgs,
) -> anyhow::Result<Option<Published>> {
    let OcrDamageArgs {
        index_version,
        batches,
        sample_pct,
        compare_csv,
        parity,
        max_runtime_secs,
        parallel,
    } = args;
    let opts = usnm_ingest::ocr_local::ocr_damage::Options {
        index_version: index_version.clone().unwrap_or_default(),
        batches: batches.clone(),
        sample_pct: *sample_pct,
        compare_csv: compare_csv.clone(),
        parity: parity.clone(),
        deadline: deadline(start, *max_runtime_secs),
        parallel: parallel.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
        }),
    };
    let curated = usnm_store::open(&cli.stores.curated)?;
    let reference = usnm_store::open(&cli.stores.reference)?;
    usnm_ingest::ocr_local::ocr_damage::run(curated, reference.as_ref(), &opts)
        .await
        .map(|()| None)
}

pub(crate) async fn ocr_prefetch_manifest_command(
    cli: &Cli,
    args: &OcrPrefetchManifestArgs,
) -> anyhow::Result<Option<Published>> {
    let OcrPrefetchManifestArgs {
        index_version,
        partial,
        min_damage,
        exchange_url,
    } = args;
    let curated = usnm_store::open(&cli.stores.curated)?;
    let reference = usnm_store::open(&cli.stores.reference)?;
    let exchange = exchange_url.as_deref().map(usnm_store::open).transpose()?;
    let opts = usnm_ingest::ocr_local::ocr_prefetch::Options {
        index_version: index_version.clone(),
        partial: *partial,
        min_damage: *min_damage,
    };
    usnm_ingest::ocr_local::ocr_prefetch::run(
        curated.as_ref(),
        reference.as_ref(),
        exchange.as_deref(),
        &opts,
    )
    .await
    .map(|_| None)
}

pub(crate) async fn ocr_sync_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    args: &OcrSyncArgs,
) -> anyhow::Result<Option<Published>> {
    let OcrSyncArgs {
        queue_url,
        out_url,
        quarantine_url,
        requests_url,
        max_parts,
        chunk,
        max_dead_letters,
        user_daily_max,
        report,
        from_quarantine,
        reason,
        max_runtime_secs,
        seed,
    } = args;
    let seed = seed.options(false)?;
    let curated = usnm_store::open(&cli.stores.curated)?;
    if *report {
        usnm_ingest::ocr_local::report::run(curated.as_ref()).await?;
        return Ok(None);
    }
    let need = |v: &Option<String>, flag: &str| {
        v.clone().with_context(|| format!("ocr-sync needs {flag}"))
    };
    let stores = usnm_ingest::ocr_local::sync::Stores {
        curated,
        reference: usnm_store::open(&cli.stores.reference)?,
        out: usnm_store::open(&need(out_url, "--out-url")?)?,
        quarantine: usnm_store::open(&need(quarantine_url, "--quarantine-url")?)?,
        requests: requests_url.as_deref().map(usnm_store::open).transpose()?,
        queue: usnm_store::queue::open(&need(queue_url, "--queue-url")?)?,
    };
    let opts = usnm_ingest::ocr_local::sync::Options {
        max_parts: *max_parts,
        chunk: *chunk,
        max_dead_letters: *max_dead_letters,
        user_daily_max: *user_daily_max,
        from_quarantine: from_quarantine.then(|| reason.clone()).flatten(),
        deadline: deadline(start, *max_runtime_secs),
        seed,
        // Read only with `--seed-skip-american-stories`, on first use.
        american_stories: Some(std::sync::Arc::new(
            usnm_ingest::american_stories::SeedTexts::default(),
        )),
    };
    usnm_ingest::ocr_local::sync::run(&state, &owner_id(), &stores, &opts)
        .await
        .map(|_| None)
}

pub(crate) async fn ocr_seed_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    args: &OcrSeedArgs,
) -> anyhow::Result<Option<Published>> {
    let OcrSeedArgs {
        queue_url,
        out_url,
        seed,
        dry_run,
        max_runtime_secs,
    } = args;
    let opts = usnm_ingest::ocr_local::seed::Options {
        deadline: deadline(start, *max_runtime_secs),
        ..seed.options(*dry_run)?
    };
    let curated = usnm_store::open(&cli.stores.curated)?;
    let reference = usnm_store::open(&cli.stores.reference)?;
    let queue = usnm_store::queue::open(queue_url)?;
    let out = out_url.as_deref().map(usnm_store::open).transpose()?;
    let american_stories = usnm_ingest::american_stories::SeedTexts::default();
    let stores = usnm_ingest::ocr_local::seed::Stores {
        curated: curated.as_ref(),
        reference: reference.as_ref(),
        queue: &queue,
        out: out.as_deref(),
        american_stories: Some(&american_stories),
    };
    let report = usnm_ingest::ocr_local::seed::run(&state, &owner_id(), &stores, &opts).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(None)
}

pub(crate) async fn ocr_enqueue_command(
    cli: &Cli,
    state: State,
    args: &OcrEnqueueArgs,
) -> anyhow::Result<Option<Published>> {
    let OcrEnqueueArgs {
        queue_url,
        pages,
        reason,
        plan,
        batch,
        include_outstanding,
        dry_run,
    } = args;
    let opts = usnm_ingest::ocr_local::enqueue::EnqueueOptions {
        pages: pages.clone(),
        reason: (*reason).into(),
        plan: plan.clone(),
        batch: batch.clone(),
        include_outstanding: *include_outstanding,
        dry_run: *dry_run,
    };
    usnm_ingest::ocr_local::enqueue::enqueue(
        &state,
        usnm_store::open(&cli.stores.curated)?.as_ref(),
        usnm_store::open(&cli.stores.reference)?.as_ref(),
        &usnm_store::queue::open(queue_url)?,
        &opts,
    )
    .await
    .map(|_| None)
}

pub(crate) async fn ocr_requeue_command(
    cli: &Cli,
    state: State,
    args: &OcrRequeueArgs,
) -> anyhow::Result<Option<Published>> {
    let OcrRequeueArgs {
        queue_url,
        cause,
        kind,
        since,
        plan,
        max_pages,
        dry_run,
    } = args;
    let opts = usnm_ingest::ocr_local::enqueue::RequeueOptions {
        cause: cause.clone(),
        kind: kind.clone(),
        since: *since,
        plan: plan.clone(),
        max_pages: *max_pages,
        dry_run: *dry_run,
    };
    usnm_ingest::ocr_local::enqueue::requeue(
        &state,
        usnm_store::open(&cli.stores.curated)?.as_ref(),
        usnm_store::open(&cli.stores.reference)?.as_ref(),
        &usnm_store::queue::open(queue_url)?,
        &opts,
    )
    .await
    .map(|_| None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;

    #[test]
    fn ocr_damage_arguments() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        let parse = |args: &[&str]| {
            let mut all = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "ocr-damage",
            ];
            all.extend_from_slice(args);
            Cli::try_parse_from(all)
        };
        assert!(parse(&[]).is_err(), "needs --version or --parity");
        assert!(parse(&["--parity", "p.jsonl.zst"]).is_ok());
        assert!(
            parse(&["--version", "v1", "--compare-csv", "x.csv.gz"]).is_err(),
            "--compare-csv needs --sample-pct"
        );
        assert!(
            parse(&["--version", "v1", "--sample-pct", "10"]).is_err(),
            "the gate needs --compare-csv"
        );
        assert!(parse(&[
            "--version",
            "v1",
            "--sample-pct",
            "10",
            "--compare-csv",
            "x.csv.gz"
        ])
        .is_ok());
        let cli = parse(&[
            "--version",
            "v1",
            "--batches",
            "a,b_ver02",
            "--max-runtime-secs",
            "60",
        ])
        .unwrap();
        let Command::OcrDamage(OcrDamageArgs {
            index_version,
            batches,
            max_runtime_secs,
            ..
        }) = cli.command
        else {
            panic!("not ocr-damage");
        };
        assert_eq!(index_version.as_deref(), Some("v1"));
        assert_eq!(batches, ["a", "b_ver02"]);
        assert_eq!(max_runtime_secs, Some(60));
    }

    #[test]
    fn ocr_seed_arguments() {
        let parse = |args: &[&str]| {
            let mut all = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "ocr-seed",
                "--queue-url",
                "q",
            ];
            all.extend_from_slice(args);
            Cli::try_parse_from(all)
        };
        let cli = parse(&[
            "--seed-plan",
            "pilot-deu",
            "--seed-languages",
            "deu",
            "--seed-years",
            "1850-1919",
            "--seed-max-pages",
            "3000",
            "--seed-pilot",
            "--bulk-window",
            "1000",
            "--dry-run",
        ])
        .unwrap();
        let Command::OcrSeed(OcrSeedArgs { seed, dry_run, .. }) = cli.command else {
            panic!("not ocr-seed");
        };
        assert!(dry_run);
        let opts = seed.options(true).unwrap();
        assert_eq!(opts.plan, "pilot-deu");
        assert_eq!(opts.languages, ["deu"]);
        assert_eq!(opts.years, Some((1850, 1919)));
        assert_eq!(opts.max_pages, Some(3000));
        assert_eq!(opts.window, 1000);
        assert_eq!(opts.refill_below, 0);
        assert!(opts.pilot && !opts.skip_american_stories);
        assert!((opts.min_damage - 0.25).abs() < 1e-12);
        // A pilot needs a budget.
        let cli = parse(&[
            "--seed-plan",
            "p",
            "--seed-pilot",
            "--seed-languages",
            "deu",
        ])
        .unwrap();
        let Command::OcrSeed(OcrSeedArgs { seed, .. }) = cli.command else {
            panic!("not ocr-seed");
        };
        assert!(seed.options(false).is_err());
        // ocr-sync takes the same settings; no plan leaves seeding off.
        let cli = Cli::try_parse_from([
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "ocr-sync",
        ])
        .unwrap();
        let Command::OcrSync(OcrSyncArgs { seed, .. }) = cli.command else {
            panic!("not ocr-sync");
        };
        assert!(!seed.options(false).unwrap().on());
    }
}
