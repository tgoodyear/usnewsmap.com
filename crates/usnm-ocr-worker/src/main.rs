//! `usnm-ocr-worker`: see the crate's README.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand, ValueEnum};
use tokio::sync::watch;
use usnm_ocr_worker::cache::CachedScans;
use usnm_ocr_worker::contract::{ContractEngine, ContractOptions, Launch};
use usnm_ocr_worker::engine::{Engine, Limits, Remote, Runner, Tesseract, TesseractOptions};
use usnm_ocr_worker::loc::{ImageSize, LocClient, LocConfig, STORAGE_SERVICE_ROOT};
use usnm_ocr_worker::message::{OcrMessage, Reason};
use usnm_ocr_worker::prefetch;
use usnm_ocr_worker::publish;
use usnm_ocr_worker::queue::{self, BULK, PRIORITY};
use usnm_ocr_worker::status::{self, About, Tracker};
use usnm_ocr_worker::telemetry::{self, Metrics};
use usnm_ocr_worker::worker::{check_node_id, Config, ScanSource, Worker};

/// `--version`: the crate version and the commit CI built it from
/// (`USNM_GIT_SHA` at build time), or `unknown commit` for a local build.
static VERSION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{} ({})",
        env!("CARGO_PKG_VERSION"),
        option_env!("USNM_GIT_SHA")
            .filter(|s| !s.is_empty())
            .map_or_else(|| "unknown commit".to_owned(), |s| format!("commit {s}"))
    )
});

#[derive(Parser)]
#[command(
    version = VERSION.as_str(),
    about = "Re-OCR damaged newspaper pages from a work queue (#296)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum EngineName {
    /// Tesseract, one process or container per page.
    Tesseract,
    /// An adapter behind the engine contract (ADR-0016), in long-lived
    /// instances: --engine-image through --podman-url, or --engine-command
    /// on this host for development.
    Contract,
}

#[derive(clap::Args)]
struct EngineArgs {
    /// Engine catalog; defaults to the catalog embedded at build time.
    #[arg(long, env = "USNM_OCR_ENGINES")]
    engines: Option<PathBuf>,
    /// OCR engine.
    #[arg(long, value_enum, default_value = "tesseract")]
    engine: EngineName,
    /// The Tesseract binary, when Tesseract is installed on the host.
    #[arg(long, default_value = "tesseract", conflicts_with = "tesseract_image")]
    tesseract_bin: PathBuf,
    /// Run Tesseract from this container image with podman, one container
    /// per page: `sha256:<image id>` or `<name>@sha256:<digest>`, never a
    /// tag.
    #[arg(long, env = "USNM_OCR_TESSERACT_IMAGE")]
    tesseract_image: Option<String>,
    /// The podman binary, for --tesseract-image.
    #[arg(long, default_value = "podman")]
    podman_bin: PathBuf,
    /// The engine runtime's Podman API socket (`podman --remote --url`,
    /// ADR-0016), instead of the worker's own user's podman. Needs
    /// --scratch-dir.
    #[arg(long, env = "USNM_OCR_PODMAN_URL")]
    podman_url: Option<String>,
    /// With --podman-url: the size-capped tmpfs that pages' scratch
    /// directories go in, which the engine account's containers can mount.
    #[arg(long, env = "USNM_OCR_SCRATCH_DIR")]
    scratch_dir: Option<PathBuf>,
    /// With --podman-url: the engine container's memory limit, with no
    /// swap.
    #[arg(long, env = "USNM_OCR_ENGINE_MEMORY_MIB", default_value_t = Limits::default().memory_mib, value_parser = clap::value_parser!(u32).range(64..))]
    engine_memory_mib: u32,
    /// With --podman-url: CPUs for the engine container.
    #[arg(long, env = "USNM_OCR_ENGINE_CPUS", default_value_t = Limits::default().cpus, value_parser = clap::value_parser!(u32).range(1..))]
    engine_cpus: u32,
    /// With --podman-url: the engine container's process limit.
    #[arg(long, env = "USNM_OCR_ENGINE_PIDS", default_value_t = Limits::default().pids, value_parser = clap::value_parser!(u32).range(8..))]
    engine_pids: u32,
    /// Tesseract page segmentation mode (3: automatic, no orientation detection).
    #[arg(long, default_value_t = 3)]
    psm: u8,
    /// Resolution Tesseract assumes for the scan (IIIF JPEGs carry none).
    #[arg(long, default_value_t = 300)]
    dpi: u32,
    /// OpenMP threads per page (1 when pages run in parallel).
    #[arg(long, default_value_t = 1)]
    threads_per_page: u32,
    /// Give up on a page after this long.
    #[arg(long, env = "USNM_OCR_PAGE_TIMEOUT_SECS", default_value_t = 600)]
    page_timeout_secs: u64,
    /// With --engine contract: the adapter's image, `<name>@sha256:<digest>`
    /// or `sha256:<id>`, run through --podman-url with --scratch-dir.
    #[arg(long, env = "USNM_OCR_ENGINE_IMAGE", conflicts_with = "engine_command")]
    engine_image: Option<String>,
    /// With --engine contract, for development only: the adapter as a
    /// program on this host, with its scratch in --scratch-dir (or the
    /// system's temporary directory). It runs with the worker's rights.
    #[arg(long)]
    engine_command: Option<PathBuf>,
    /// With --engine-command: a flag for the adapter, as `--name=value`
    /// (repeat for more).
    #[arg(long = "engine-command-flag", allow_hyphen_values = true)]
    engine_command_flags: Vec<String>,
    /// With --engine contract: instances of the engine, and so its pages
    /// at once.
    #[arg(long, env = "USNM_OCR_ENGINE_INSTANCES", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=16))]
    engine_instances: u32,
    /// With --engine contract: time from an instance's start to its hello.
    #[arg(long, env = "USNM_OCR_ENGINE_START_TIMEOUT_SECS", default_value_t = 60)]
    engine_start_timeout_secs: u64,
    /// With --engine contract: pages an instance reads before it is
    /// replaced.
    #[arg(long, env = "USNM_OCR_ENGINE_MAX_JOBS", default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..))]
    engine_max_jobs: u32,
}

impl EngineArgs {
    /// The contract engine's options, for `--engine contract`.
    fn contract_options(&self, node: &str) -> anyhow::Result<ContractOptions> {
        let launch = match (
            self.engine_image.as_deref().filter(|s| !s.is_empty()),
            &self.engine_command,
        ) {
            (Some(image), _) => Launch::Podman {
                podman: self.podman_bin.clone(),
                remote: Remote {
                    url: self
                        .podman_url
                        .clone()
                        .filter(|s| !s.is_empty())
                        .context("--engine-image needs --podman-url")?,
                    scratch: self
                        .scratch_dir
                        .clone()
                        .filter(|p| !p.as_os_str().is_empty())
                        .context("--engine-image needs --scratch-dir")?,
                    limits: Limits {
                        memory_mib: self.engine_memory_mib,
                        cpus: self.engine_cpus,
                        pids: self.engine_pids,
                    },
                },
                image: image.to_owned(),
            },
            (None, Some(program)) => Launch::Local {
                program: program.clone(),
                args: self.engine_command_flags.clone(),
                scratch: self.scratch_dir.clone().unwrap_or_else(std::env::temp_dir),
            },
            (None, None) => {
                bail!("--engine contract needs --engine-image or --engine-command")
            }
        };
        Ok(ContractOptions {
            launch,
            instances: self.engine_instances as usize,
            threads: self.threads_per_page,
            start_timeout: Duration::from_secs(self.engine_start_timeout_secs),
            page_timeout: Duration::from_secs(self.page_timeout_secs),
            max_jobs: self.engine_max_jobs,
            dpi: self.dpi,
            node: node.to_owned(),
        })
    }

    async fn engine(&self, node: &str) -> anyhow::Result<Arc<dyn Engine>> {
        let catalog = match &self.engines {
            Some(path) => usnm_ocr_route::Catalog::load(path)?,
            None => usnm_ocr_route::ENGINES.clone(),
        };
        match self.engine {
            EngineName::Contract => {
                let e = ContractEngine::start_with_catalog(self.contract_options(node)?, catalog)
                    .await?;
                Ok(Arc::new(e))
            }
            EngineName::Tesseract => {
                let runner = match self.tesseract_image.as_deref().filter(|s| !s.is_empty()) {
                    Some(image) => {
                        let remote = match self.podman_url.as_deref().filter(|s| !s.is_empty()) {
                            Some(url) => Some(Remote {
                                url: url.to_owned(),
                                scratch: self
                                    .scratch_dir
                                    .clone()
                                    .filter(|p| !p.as_os_str().is_empty())
                                    .context("--podman-url needs --scratch-dir")?,
                                limits: Limits {
                                    memory_mib: self.engine_memory_mib,
                                    cpus: self.engine_cpus,
                                    pids: self.engine_pids,
                                },
                            }),
                            None => None,
                        };
                        Runner::Podman {
                            podman: self.podman_bin.clone(),
                            remote,
                            image: image.to_owned(),
                        }
                    }
                    None => Runner::Host(self.tesseract_bin.clone()),
                };
                let t = Tesseract::detect_with_catalog(
                    TesseractOptions {
                        runner,
                        psm: self.psm,
                        dpi: self.dpi,
                        threads: self.threads_per_page,
                        timeout: Duration::from_secs(self.page_timeout_secs),
                    },
                    catalog,
                )
                .await?;
                tracing::info!(
                    version = t.version(),
                    runner = ?t.runner(),
                    models = ?t.installed(),
                    "tesseract found"
                );
                Ok(Arc::new(t))
            }
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Validate production route licences without starting an engine.
    Routes {
        #[command(subcommand)]
        command: RoutesCommand,
    },
    /// Take pages from the queues and OCR them until stopped.
    Run {
        /// `https://<account>.queue.core.windows.net`, or a directory.
        #[arg(long, env = "USNM_OCR_QUEUE_URL")]
        queue_url: String,
        /// `https://<account>.blob.core.windows.net/<container>`, or a directory.
        #[arg(long, env = "USNM_OCR_OUT_URL")]
        out_url: String,
        /// This node's name, in part names and logs.
        #[arg(long, env = "USNM_OCR_NODE_ID")]
        node_id: String,
        /// Pages in flight at once.
        #[arg(long, env = "USNM_OCR_CONCURRENCY", default_value_t = 1)]
        concurrency: usize,
        /// Stop once both queues are empty.
        #[arg(long)]
        once: bool,
        /// Gap between loc.gov API requests, shared by all pages on this
        /// node (5 s: www.loc.gov's `Crawl-Delay`). With several nodes,
        /// multiply by their number.
        #[arg(long, env = "USNM_OCR_API_GAP_MS", default_value_t = 5000)]
        api_gap_ms: u64,
        /// Gap between image requests to LoC's IIIF server.
        #[arg(long, env = "USNM_OCR_TILE_GAP_MS", default_value_t = 1000)]
        tile_gap_ms: u64,
        /// The scan server on pve-big (`http://<host>:8380`): a page's scan
        /// is read from there when it checks out against its sidecar, else
        /// fetched from LoC. Unset: no cache.
        #[arg(long, env = "USNM_OCR_SCAN_CACHE_URL")]
        scan_cache_url: Option<String>,
        /// How long a taken message stays hidden. It isn't renewed, so it
        /// must cover the slowest page; a page not done by 80% of it is
        /// abandoned without output.
        #[arg(long, env = "USNM_OCR_VISIBILITY_SECS", default_value_t = 1800, value_parser = clap::value_parser!(u64).range(60..=604_800))]
        visibility_secs: u64,
        /// How long in-flight pages may take after SIGTERM before they are
        /// abandoned (they reappear when their visibility runs out).
        #[arg(long, default_value_t = 60)]
        shutdown_grace_secs: u64,
        /// On planned stops, stop taking pages and give in-flight pages
        /// their configured page timeout before abandoning them.
        #[arg(long, env = "USNM_OCR_DRAIN_ON_STOP")]
        drain_on_stop: bool,
        /// Also publish the scan server's fetched list for this prefetch
        /// manifest version to `ocr-out/fetched/` every
        /// --publish-every-secs, for the seeder (12 Step 11). Needs
        /// --scan-cache-url. Set on one node.
        #[arg(long, env = "USNM_OCR_PUBLISH_FETCHED")]
        publish_fetched: Option<String>,
        #[arg(long, env = "USNM_OCR_PUBLISH_EVERY_SECS", default_value_t = 1800, value_parser = clap::value_parser!(u64).range(60..))]
        publish_every_secs: u64,
        /// Serve this worker's status to the LAN dashboard here
        /// (`0.0.0.0:8381`): `GET /status` and `GET /journal`. Unset: none.
        #[arg(long, env = "USNM_OCR_STATUS_LISTEN")]
        status_listen: Option<SocketAddr>,
        /// The only hosts the status is served to (the dashboard's), as
        /// names or addresses, comma-separated. Names are resolved each
        /// time the server starts.
        #[arg(long, env = "USNM_OCR_STATUS_ALLOW", value_delimiter = ',')]
        status_allow: Vec<String>,
        /// The journal of finished pages (doc_id, language, engine tag,
        /// times, outcome; no text), appended to across restarts.
        #[arg(long, env = "USNM_OCR_STATUS_JOURNAL")]
        status_journal: Option<PathBuf>,
        /// Durable local page/engine execution JSON (no OCR text); unset disables capture.
        #[arg(long, env = "USNM_OCR_RUN_METADATA_DIR")]
        run_metadata_dir: Option<PathBuf>,
        #[command(flatten)]
        engine: Box<EngineArgs>,
    },
    /// Publish the scan server's fetched list once (12 Step 11): read
    /// `<scan server>/index/full.jsonl` and write the full-size scans'
    /// pages to `ocr-out/fetched/<manifest>/<node>.<stamp>.parquet`.
    PublishFetched {
        /// The scan server, `http://<host>:8380`.
        #[arg(long, env = "USNM_OCR_SCAN_CACHE_URL")]
        scan_cache_url: String,
        /// `https://<account>.blob.core.windows.net/ocr-out`, or a directory.
        #[arg(long, env = "USNM_OCR_OUT_URL")]
        out_url: String,
        /// This node's name, in the list's path.
        #[arg(long, env = "USNM_OCR_NODE_ID")]
        node_id: String,
        /// The prefetch manifest's index version (`pages-v20261010-2`).
        #[arg(long, env = "USNM_OCR_PUBLISH_FETCHED")]
        manifest: String,
    },
    /// Fetch the scans a prefetch manifest lists onto local disk (#368),
    /// with the worker's LoC client and pacing: each page's original JP2
    /// through its batch directory (bag manifest and issue METS, kept on
    /// disk, md5 checked), and only for a page that route can't place, the
    /// item API and the IIIF JPEG. Until done, stopped,
    /// `--max-total-bytes` is reached or 20 pages from 3 titles fail in a
    /// row. Resumable: scans already there and matching their sidecar are
    /// skipped. An issue whose item loc.gov can't serve while it serves
    /// others is skipped for a cool-off and asked again after it (#414).
    Prefetch {
        /// The manifest (`manifest.parquet` from `usnm-ingest
        /// ocr-prefetch-manifest`).
        #[arg(long, env = "USNM_PREFETCH_MANIFEST")]
        manifest: PathBuf,
        /// Where the scans go: `<out>/<keep|buffer>/<batch>/<lccn>/<date>/<ed>/<seq>.<ext>`.
        #[arg(long, env = "USNM_PREFETCH_OUT")]
        out: PathBuf,
        /// The keep set's resolution: `full`, or `scaled:<1-99>` (IIIF `pct:<n>`).
        #[arg(long, env = "USNM_PREFETCH_RESOLUTION", default_value = "full")]
        resolution: ImageSize,
        /// The buffer set's resolution.
        #[arg(long, env = "USNM_PREFETCH_BUFFER_RESOLUTION", default_value = "full")]
        buffer_resolution: ImageSize,
        /// `keep` (non-English pages and the worst English batches, kept
        /// for good), `buffer` (the rest of English, emptied by the
        /// workers) or `both` (keep first).
        #[arg(long, env = "USNM_PREFETCH_SETS", value_enum, default_value = "both")]
        sets: PrefetchSets,
        /// Wait while the buffer set's scans on disk reach this many bytes.
        #[arg(
            long,
            env = "USNM_PREFETCH_BUFFER_CAP",
            default_value_t = 1_000_000_000_000
        )]
        buffer_cap: u64,
        /// Only the first N pages (after --sample and the order).
        #[arg(long)]
        limit: Option<usize>,
        /// About N pages, stratified by language and decade.
        #[arg(long)]
        sample: Option<usize>,
        /// `priority` (German before the 1920s, the worst English batches,
        /// Czech, Serbian, Russian, the rest) or `issue` (the manifest's).
        #[arg(
            long,
            env = "USNM_PREFETCH_ORDER",
            value_enum,
            default_value = "priority"
        )]
        order: PrefetchOrder,
        /// Stop starting pages once this run's scans on disk add up to this.
        #[arg(long, env = "USNM_PREFETCH_MAX_TOTAL_BYTES")]
        max_total_bytes: Option<u64>,
        /// Pages in flight at once; they share the pacers.
        #[arg(long, env = "USNM_PREFETCH_CONCURRENCY", default_value_t = 4)]
        concurrency: usize,
        /// The lock that keeps it to one fetching process (default
        /// `<out>/.prefetch.lock`).
        #[arg(long, env = "USNM_PREFETCH_LOCK")]
        lock_file: Option<PathBuf>,
        /// A status write and progress line every N pages.
        #[arg(long, default_value_t = 100)]
        progress_every: u64,
        /// Stop after this many pages fail in a row...
        #[arg(long, env = "USNM_PREFETCH_MAX_FAILURES", default_value_t = 20)]
        max_failures_in_a_row: u32,
        /// ...from at least this many titles (1: any pages).
        #[arg(long, env = "USNM_PREFETCH_STOP_MIN_TITLES", default_value_t = 3)]
        stop_min_titles: usize,
        /// How long an issue whose item loc.gov can't serve is skipped.
        #[arg(
            long,
            env = "USNM_PREFETCH_ITEM_COOLOFF_SECS",
            default_value_t = 86_400
        )]
        item_cooloff_secs: u64,
        /// Cool-offs before such an issue is taken as permanent.
        #[arg(long, env = "USNM_PREFETCH_ITEM_MAX_COOLOFFS", default_value_t = 3)]
        item_max_cooloffs: u32,
        /// Gap between loc.gov API requests (5 s: www.loc.gov's
        /// `Crawl-Delay`). Only pages the batch route can't place use the API.
        #[arg(long, env = "USNM_OCR_API_GAP_MS", default_value_t = 5000)]
        api_gap_ms: u64,
        /// Gap between image requests to LoC's IIIF server.
        #[arg(long, env = "USNM_OCR_TILE_GAP_MS", default_value_t = 1000)]
        tile_gap_ms: u64,
        /// Gap between requests to LoC's storage service (batch manifests,
        /// issue METS, JP2s).
        #[arg(long, env = "USNM_OCR_STORAGE_GAP_MS", default_value_t = 1000)]
        storage_gap_ms: u64,
        /// Give the storage service a pacer of its own. Without it, storage
        /// and image requests share one, at the longer of the two gaps, and
        /// a hold on either holds both (LoC publishes a limit for each but
        /// doesn't say whether they're counted together).
        #[arg(long, env = "USNM_OCR_STORAGE_OWN_BUDGET")]
        storage_own_budget: bool,
        /// Where batch manifests and issue METS are kept, so each is
        /// fetched once.
        #[arg(
            long,
            env = "USNM_PREFETCH_BATCH_CACHE",
            default_value = "/tank/ocr/manifests/batches"
        )]
        batch_cache: PathBuf,
        /// The host's global LoC budget (#962): a state file every LoC client
        /// on the host shares, 45 requests a minute in all, 10 once LoC pushes
        /// back. Without it, only this process's own gaps apply.
        #[arg(long, env = "USNM_LOC_GLOBAL_PACER")]
        loc_global_pacer: Option<PathBuf>,
        /// Copies of LoC's JP2s to ask before LoC, comma-separated: the
        /// state partners `nbu`, `pst`, `oru` and `scu` (each for its own
        /// batches) and `wayback`. Only bytes matching the bag's md5 are
        /// taken. None by default.
        #[arg(long, env = "USNM_PREFETCH_MIRRORS", value_delimiter = ',')]
        mirrors: Vec<String>,
        /// Between requests to one state partner.
        #[arg(long, env = "USNM_PREFETCH_PARTNER_GAP_MS", default_value_t = 5000)]
        partner_gap_ms: u64,
        /// Between requests to the Wayback Machine.
        #[arg(long, env = "USNM_PREFETCH_WAYBACK_GAP_MS", default_value_t = 5000)]
        wayback_gap_ms: u64,
        /// LoC's own S3 bucket of NDNP batches
        /// (`https://ndnp-batches.s3.amazonaws.com/`), asked before
        /// tile.loc.gov for the bag manifest, the issue METS and the JP2 of
        /// each batch it holds (1,074 of 3,010 batches, 38.1% of pages, on 24
        /// October 2026), each checked as LoC's are. A manifest from it is
        /// kept under the LoC URL the batch's name gives, marked as a guess
        /// until LoC confirms it. One request at a time, outside the host's
        /// LoC budget; anything it can't serve goes to LoC. Only an
        /// amazonaws.com host. Empty: off (the default).
        #[arg(long, env = "USNM_PREFETCH_LOC_BUCKET")]
        loc_bucket: Option<String>,
        /// From the end of one request to LoC's bucket to the start of the
        /// next; at least 6000.
        #[arg(long, env = "USNM_PREFETCH_LOC_BUCKET_GAP_MS", default_value_t = 6000)]
        loc_bucket_gap_ms: u64,
        /// Only pages of these awardees' batches, comma-separated (the
        /// batch name's first part, e.g. `nbu`); all by default.
        #[arg(long, env = "USNM_PREFETCH_AWARDEES", value_delimiter = ',')]
        awardees: Vec<String>,
    },
    /// Chronicling America's bulk OCR archives (ALTO with word confidence),
    /// one at a time, inside the host's global LoC budget (#962).
    BulkOcr {
        /// Where the archives go.
        #[arg(long, env = "USNM_BULK_OUT")]
        out: PathBuf,
        /// The host's global LoC budget (required: bulk never runs outside it).
        #[arg(long, env = "USNM_LOC_GLOBAL_PACER")]
        loc_global_pacer: PathBuf,
        /// LoC's datasets table.
        #[arg(long, default_value = usnm_ocr_worker::bulk::LIST_URL)]
        list_url: String,
        /// Seconds between archive requests (LoC: 10 bulk requests in 10 minutes).
        #[arg(long, env = "USNM_BULK_GAP_SECS", default_value_t = 60)]
        gap_secs: u64,
        /// Stop starting archives after this many hours (LoC may stop programs
        /// that run past 24).
        #[arg(long, env = "USNM_BULK_MAX_HOURS", default_value_t = 20.0)]
        max_hours: f64,
        /// Keep at least this many GB free on the output's filesystem.
        #[arg(long, env = "USNM_BULK_MIN_FREE_GB", default_value_t = 200)]
        min_free_gb: u64,
        /// At most this many archives this run.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// LoC's word confidence per page from the bulk OCR archives (#962): one Parquet file
    /// per archive, skipping archives already read. Reads only.
    LocAltoStats {
        /// The archives (`bulk-ocr`'s output).
        #[arg(long, env = "USNM_LOC_ALTO_ARCHIVES")]
        archives: PathBuf,
        /// Where the Parquet files go.
        #[arg(long, env = "USNM_LOC_ALTO_OUT")]
        out: PathBuf,
        /// The `wc_below` share counts words under this confidence.
        #[arg(long, env = "USNM_LOC_ALTO_THRESHOLD", default_value_t = 0.5)]
        threshold: f64,
        /// At most this many archives this run.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// The host's global LoC budget: print it, or reset it to the normal rate
    /// after LoC pushed back (an operator's decision).
    LocPacer {
        #[arg(long, env = "USNM_LOC_GLOBAL_PACER")]
        file: PathBuf,
        #[arg(long)]
        reset: bool,
    },
    /// Serve the prefetched scans to the OCR nodes over HTTP (#368): GET
    /// scans and sidecars, DELETE buffer-set scans, PUT manifests, from the
    /// --allow hosts only. Runs in the scan-fetch container on pve-big.
    ServeScans {
        /// `/tank/ocr/scans`, holding `keep/` and `buffer/`.
        #[arg(long, env = "USNM_SCANSERVE_SCANS", default_value = "/tank/ocr/scans")]
        scans: PathBuf,
        /// `/tank/ocr/manifests`.
        #[arg(
            long,
            env = "USNM_SCANSERVE_MANIFESTS",
            default_value = "/tank/ocr/manifests"
        )]
        manifests: PathBuf,
        /// Address and port to listen on.
        #[arg(long, env = "USNM_SCANSERVE_LISTEN", default_value = "0.0.0.0:8380")]
        listen: std::net::SocketAddr,
        /// The only clients served (the OCR nodes), by host name,
        /// comma-separated. Resolved at startup, which fails if one doesn't
        /// resolve, and again every --allow-refresh-secs.
        #[arg(
            long,
            env = "USNM_SCANSERVE_ALLOW",
            value_delimiter = ',',
            required = true
        )]
        allow: Vec<String>,
        /// How often the --allow names are resolved again.
        #[arg(long, env = "USNM_SCANSERVE_ALLOW_REFRESH_SECS", default_value_t = 300)]
        allow_refresh_secs: u64,
    },
    /// Upload the keep-set scans to the scan archive (#442, ADR-0018): Cold
    /// tier, create-only, checked against each scan's sidecar. Prints a JSON
    /// report; exits non-zero if anything was wrong or failed (run it again).
    ArchiveScans {
        /// `/tank/ocr/scans`, holding `keep/`.
        #[arg(long, env = "USNM_ARCHIVE_SCANS", default_value = "/tank/ocr/scans")]
        scans: PathBuf,
        /// The archive container: `https://<account>.blob.core.windows.net/scans`.
        #[arg(long, env = "USNM_ARCHIVE_URL")]
        archive_url: String,
        /// The set to archive. Only `keep` is meant for the archive.
        #[arg(long, default_value = "keep")]
        set: String,
        /// Say what would be uploaded; write nothing.
        #[arg(long, conflicts_with = "verify")]
        dry_run: bool,
        /// Compare each blob's size, md5 and tier with the local file, by
        /// properties only (no download, so no Cold retrieval charge).
        #[arg(long)]
        verify: bool,
        /// Stop after this many scans (0: all).
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Scans in flight.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        /// File of the scans already archived. A scan recorded there with
        /// the same size and mtime is not read or looked up again; an
        /// upload run appends to it. Ignored by --verify.
        #[arg(long, env = "USNM_ARCHIVE_STATE")]
        state: Option<PathBuf>,
    },
    /// Project a manifest's size and fetch time from a sample's index
    /// (`index-<tag>.jsonl` from `prefetch --sample`). Prints JSON.
    PrefetchEstimate {
        /// The whole manifest to project for.
        #[arg(long)]
        manifest: PathBuf,
        /// The sample run's index file.
        #[arg(long)]
        index: PathBuf,
        #[arg(long, default_value_t = 5000)]
        api_gap_ms: u64,
        #[arg(long, default_value_t = 1000)]
        tile_gap_ms: u64,
    },
    /// Queue one page (for testing).
    Enqueue {
        #[arg(long)]
        queue_url: String,
        /// `lccn/YYYY-MM-DD/ed-N/seq-N`.
        #[arg(long)]
        page: String,
        /// The LoC batch the page is in.
        #[arg(long)]
        batch: String,
        /// `user` queues to ocr-priority, `bulk` to ocr-bulk.
        #[arg(long, value_enum)]
        priority: Priority,
        /// ISO 639-3.
        #[arg(long)]
        language: String,
        /// The page's damage score, if known.
        #[arg(long)]
        score: Option<f64>,
    },
    /// OCR one image file (for benchmarking an engine on a node).
    OcrFile {
        #[arg(long)]
        image: PathBuf,
        /// ISO 639-3.
        #[arg(long)]
        language: String,
        /// Directory for `<image stem>.txt` (normalized) and `<image stem>.xml` (ALTO).
        #[arg(long)]
        out: PathBuf,
        #[command(flatten)]
        engine: EngineArgs,
    },
    /// Read a rendered line of English with the engine and fail unless it
    /// comes back: a node's check before the unit is enabled.
    CheckEngine {
        #[command(flatten)]
        engine: EngineArgs,
    },
    /// Engines behind the engine contract.
    Engines {
        #[command(subcommand)]
        command: EnginesCommand,
    },
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    /// Held by every test here that sets the environment: parsing reads
    /// all of the flags' variables, and tests run in parallel.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn run_metadata_flag_and_environment_are_opt_in() {
        let _env = env_lock();
        const NAME: &str = "USNM_OCR_RUN_METADATA_DIR";
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var(NAME, value),
                    None => std::env::remove_var(NAME),
                }
            }
        }
        let _restore = Restore(std::env::var_os(NAME));
        std::env::remove_var(NAME);
        let args = [
            "usnm-ocr-worker",
            "run",
            "--queue-url",
            "queue",
            "--out-url",
            "output",
            "--node-id",
            "node",
        ];
        let path = |cli: Cli| match cli.command {
            Command::Run {
                run_metadata_dir, ..
            } => run_metadata_dir,
            _ => panic!("expected run"),
        };
        assert!(path(Cli::try_parse_from(args).unwrap()).is_none());
        std::env::set_var(NAME, "environment-metadata");
        assert_eq!(
            path(Cli::try_parse_from(args).unwrap()),
            Some(PathBuf::from("environment-metadata"))
        );
        assert_eq!(
            path(
                Cli::try_parse_from(
                    args.into_iter()
                        .chain(["--run-metadata-dir", "flag-metadata"])
                )
                .unwrap()
            ),
            Some(PathBuf::from("flag-metadata"))
        );
    }

    #[test]
    fn drain_on_stop_can_be_enabled_through_environment() {
        let _env = env_lock();
        struct RestoreEnv(Option<std::ffi::OsString>);

        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("USNM_OCR_DRAIN_ON_STOP", value),
                    None => std::env::remove_var("USNM_OCR_DRAIN_ON_STOP"),
                }
            }
        }

        let _restore = RestoreEnv(std::env::var_os("USNM_OCR_DRAIN_ON_STOP"));
        std::env::set_var("USNM_OCR_DRAIN_ON_STOP", "true");
        let cli = Cli::try_parse_from([
            "usnm-ocr-worker",
            "run",
            "--queue-url",
            "https://example.queue.core.windows.net",
            "--out-url",
            "https://example.blob.core.windows.net/ocr-out",
            "--node-id",
            "node",
        ])
        .expect("parse environment-backed drain setting");
        match cli.command {
            Command::Run { drain_on_stop, .. } => assert!(drain_on_stop),
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn locs_bucket_is_off_unless_named_and_never_under_six_seconds_apart() {
        let _env = env_lock();
        const NAMES: [&str; 2] = [
            "USNM_PREFETCH_LOC_BUCKET",
            "USNM_PREFETCH_LOC_BUCKET_GAP_MS",
        ];
        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (name, value) in self.0.drain(..) {
                    match value {
                        Some(v) => std::env::set_var(name, v),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
        let _restore = Restore(NAMES.map(|n| (n, std::env::var_os(n))).to_vec());
        for n in NAMES {
            std::env::remove_var(n);
        }
        let bucket = |extra: &[&str]| {
            let args = [
                "usnm-ocr-worker",
                "prefetch",
                "--manifest",
                "m",
                "--out",
                "o",
            ];
            let cli = Cli::try_parse_from(args.iter().chain(extra)).unwrap();
            match cli.command {
                Command::Prefetch {
                    loc_bucket,
                    loc_bucket_gap_ms,
                    ..
                } => loc_bucket_config(loc_bucket.as_deref(), loc_bucket_gap_ms),
                _ => panic!("expected prefetch"),
            }
        };
        // Off by default, and when empty.
        assert!(bucket(&[]).unwrap().is_none());
        assert!(bucket(&["--loc-bucket", ""]).unwrap().is_none());
        // The flag, 6 s apart unless told more.
        let root = usnm_ocr_worker::loc::bucket::ROOT;
        let b = bucket(&["--loc-bucket", root]).unwrap().unwrap();
        assert_eq!((b.root.as_str(), b.gap), (root, Duration::from_secs(6)));
        let b = bucket(&["--loc-bucket", root, "--loc-bucket-gap-ms", "9000"])
            .unwrap()
            .unwrap();
        assert_eq!(b.gap, Duration::from_secs(9));
        assert!(bucket(&["--loc-bucket", root, "--loc-bucket-gap-ms", "0"]).is_err());
        assert!(bucket(&["--loc-bucket", "https://tile.loc.gov/"]).is_err());
        // The environment, as the unit sets it.
        std::env::set_var(NAMES[0], root);
        std::env::set_var(NAMES[1], "7000");
        let b = bucket(&[]).unwrap().unwrap();
        assert_eq!((b.root.as_str(), b.gap), (root, Duration::from_secs(7)));
        std::env::set_var(NAMES[1], "5999");
        assert!(bucket(&[]).is_err());
        std::env::set_var(NAMES[0], "");
        assert!(bucket(&[]).unwrap().is_none(), "empty: off");
    }
}

#[derive(Subcommand)]
enum RoutesCommand {
    /// Refuse production candidates whose code or models fail the licence gate.
    Check {
        #[arg(long, env = "USNM_OCR_ENGINES")]
        engines: Option<PathBuf>,
        #[arg(long, default_value = "ocr/routing/routes.toml")]
        routes: PathBuf,
    },
}

#[derive(Subcommand)]
enum EnginesCommand {
    /// Drive one instance of an engine through the contract's conformance
    /// cases (13 §13.4.8): a rendered line with its boxes, a blank page, a
    /// corrupt image, an unknown profile, a 1 s deadline, Tesseract's
    /// --dpi, stdout noise, the size caps and stdin EOF. One JSON line per
    /// case; exits non-zero if any fails. The engine is given as for `run`
    /// (`--engine contract` with `--engine-image` or `--engine-command`)
    /// until the engine lock names engines by id (#371).
    Conformance {
        /// The directory with the suite's fixtures.
        #[arg(long, default_value = "ocr/contract/fixtures")]
        fixtures: PathBuf,
        #[command(flatten)]
        engine: EngineArgs,
    },
}

/// The line `check-engine` renders and must read back.
const CHECK_TEXT: &str = "HELLO WORLD";

#[derive(Clone, Copy, ValueEnum)]
enum PrefetchSets {
    Keep,
    Buffer,
    Both,
}

#[derive(Clone, Copy, ValueEnum)]
enum PrefetchOrder {
    Priority,
    Issue,
}

#[derive(Clone, Copy, ValueEnum)]
enum Priority {
    User,
    Bulk,
}

/// LoC's bucket from `--loc-bucket` and `--loc-bucket-gap-ms`: `None` when
/// the flag is absent or empty.
fn loc_bucket_config(
    root: Option<&str>,
    gap_ms: u64,
) -> anyhow::Result<Option<usnm_ocr_worker::loc::bucket::BucketConfig>> {
    usnm_ocr_worker::loc::bucket::BucketConfig::at(
        root.unwrap_or_default(),
        Duration::from_millis(gap_ms),
    )
}

/// Turns true on SIGTERM or Ctrl-C.
fn stop_signal() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("stop requested");
        tx.send_replace(true);
    });
    rx
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    // `run` reports as its node; with no connection string, logs only.
    let node = match &cli.command {
        Command::Run { node_id, .. } if check_node_id(node_id).is_ok() => Some(node_id.as_str()),
        _ => None,
    };
    let telemetry = telemetry::init(node);
    let result = command(cli).await;
    let code = match &result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::info_span!("ocr_worker_exit")
                .in_scope(|| tracing::error!(error = format!("{e:#}"), "usnm-ocr-worker failed"));
            std::process::ExitCode::FAILURE
        }
    };
    // Flush what's buffered before exit, on every path.
    telemetry.shutdown().await;
    code
}

async fn command(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Routes {
            command: RoutesCommand::Check { engines, routes },
        } => {
            let catalog = match engines {
                Some(path) => usnm_ocr_route::Catalog::load(&path)?,
                None => usnm_ocr_route::ENGINES.clone(),
            };
            catalog.check_routes(
                &std::fs::read_to_string(&routes)
                    .with_context(|| format!("reading {}", routes.display()))?,
            )?;
            println!("production route licences cleared");
            Ok(())
        }
        Command::Run {
            queue_url,
            out_url,
            node_id,
            concurrency,
            once,
            api_gap_ms,
            tile_gap_ms,
            scan_cache_url,
            visibility_secs,
            shutdown_grace_secs,
            drain_on_stop,
            publish_fetched,
            publish_every_secs,
            status_listen,
            status_allow,
            status_journal,
            run_metadata_dir,
            engine,
        } => {
            check_node_id(&node_id)?;
            let publish = match publish_fetched.filter(|m| !m.is_empty()) {
                Some(manifest) => Some(publish::Options {
                    scan_server: scan_cache_url
                        .clone()
                        .filter(|u| !u.is_empty())
                        .context("--publish-fetched needs --scan-cache-url")?,
                    manifest,
                    node_id: node_id.clone(),
                }),
                None => None,
            };
            let cfg = Config {
                node_id,
                concurrency: concurrency.max(1),
                once,
                visibility: Duration::from_secs(visibility_secs),
                shutdown_grace: Duration::from_secs(shutdown_grace_secs),
                drain_on_stop,
                page_timeout: Duration::from_secs(engine.page_timeout_secs),
                run_metadata_dir,
                ..Config::default()
            };
            if Duration::from_secs(engine.page_timeout_secs) >= cfg.budget() {
                let _span = tracing::info_span!("ocr_worker_start").entered();
                tracing::warn!(
                    page_timeout_secs = engine.page_timeout_secs,
                    budget_secs = cfg.budget().as_secs(),
                    "--page-timeout-secs is over 80% of --visibility-secs; slow pages will be abandoned"
                );
            }
            let scan_cache_url = scan_cache_url.filter(|u| !u.is_empty());
            let scans: Arc<dyn ScanSource> = {
                let cfg = LocConfig {
                    api_gap: Duration::from_millis(api_gap_ms),
                    tile_gap: Duration::from_millis(tile_gap_ms),
                    html_is_block: true,
                    ..LocConfig::default()
                };
                let iiif_root = cfg.iiif_root.clone();
                let loc: Arc<dyn ScanSource> = Arc::new(LocClient::new(cfg)?);
                match scan_cache_url.as_deref() {
                    Some(url) => {
                        tracing::info_span!("ocr_worker_start")
                            .in_scope(|| tracing::info!(url, "reading prefetched scans first"));
                        Arc::new(CachedScans::new(
                            url,
                            iiif_root,
                            STORAGE_SERVICE_ROOT.into(),
                            loc,
                        )?)
                    }
                    None => loc,
                }
            };
            let engine = engine.engine(&cfg.node_id).await?;
            // Every exit from here on, an error in the setup below included,
            // waits for the engine's instances to stop.
            let stopping = Arc::clone(&engine);
            let done = async {
                let tracker = Arc::new(Tracker::new(
                    About {
                        node_id: cfg.node_id.clone(),
                        version: VERSION.clone(),
                        concurrency: u32::try_from(cfg.concurrency).unwrap_or(u32::MAX),
                        engine: engine.source().to_owned(),
                        models: engine.models(),
                        routes: engine.routes(),
                        scan_cache_url: scan_cache_url.clone(),
                    },
                    status_journal.filter(|p| !p.as_os_str().is_empty()),
                    Some(scans.clone()),
                )?);
                let stop = stop_signal();
                let queue = queue::open(&queue_url)?;
                if let Some(listen) = status_listen {
                    anyhow::ensure!(
                        !status_allow.is_empty(),
                        "--status-listen needs --status-allow (the dashboard's address)"
                    );
                    // The queues' lengths and the front of the dead letters, for
                    // the dashboard: a look a minute, peeks only.
                    {
                        let (tracker, queue) = (tracker.clone(), queue.clone());
                        tokio::spawn(async move {
                            let mut every = tokio::time::interval(status::QUEUE_LOOK_EVERY);
                            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                            loop {
                                every.tick().await;
                                tracker.set_queues(status::look_at_queues(queue.as_ref()).await);
                            }
                        });
                    }
                    // Supervised apart from the OCR loop, which runs whatever
                    // happens to it.
                    status::supervise(
                        listen,
                        tracker.clone(),
                        status_allow,
                        stop.clone(),
                        Metrics::global(),
                        status::RESTART_FIRST,
                    );
                }
                let worker = Arc::new(Worker {
                    queue,
                    out: usnm_store::open(&out_url)?,
                    scans,
                    engine,
                    cfg,
                    metrics: Metrics::global(),
                    tracker,
                });
                if let Some(opts) = publish {
                    tracing::info_span!("ocr_worker_start").in_scope(|| {
                        tracing::info!(
                            manifest = opts.manifest,
                            every_secs = publish_every_secs,
                            "publishing the fetched list"
                        )
                    });
                    tokio::spawn(publish::every(
                        Arc::clone(&worker.out),
                        opts,
                        Duration::from_secs(publish_every_secs),
                        stop.clone(),
                    ));
                }
                worker.run(stop).await
            }
            .await;
            stopping.shutdown().await;
            done
        }
        Command::PublishFetched {
            scan_cache_url,
            out_url,
            node_id,
            manifest,
        } => {
            check_node_id(&node_id)?;
            let out = usnm_store::open(&out_url)?;
            let p = publish::publish(
                out.as_ref(),
                &publish::Options {
                    scan_server: scan_cache_url,
                    manifest,
                    node_id,
                },
            )
            .await?;
            println!(
                "{}",
                serde_json::json!({"path": p.path, "lines": p.lines, "pages": p.pages, "skipped": p.skipped})
            );
            Ok(())
        }
        Command::Prefetch {
            manifest,
            out,
            resolution,
            buffer_resolution,
            sets,
            buffer_cap,
            limit,
            sample,
            order,
            max_total_bytes,
            concurrency,
            lock_file,
            progress_every,
            max_failures_in_a_row,
            stop_min_titles,
            item_cooloff_secs,
            item_max_cooloffs,
            api_gap_ms,
            tile_gap_ms,
            storage_gap_ms,
            storage_own_budget,
            batch_cache,
            loc_global_pacer,
            mirrors,
            partner_gap_ms,
            wayback_gap_ms,
            loc_bucket,
            loc_bucket_gap_ms,
            awardees,
        } => {
            let mirrors = {
                let cfg = usnm_ocr_worker::loc::mirror::MirrorConfig {
                    partner_gap: Duration::from_millis(partner_gap_ms),
                    wayback_gap: Duration::from_millis(wayback_gap_ms),
                    ..usnm_ocr_worker::loc::mirror::MirrorConfig::named(&mirrors)?
                };
                if cfg.is_empty() {
                    None
                } else {
                    Some(Arc::new(usnm_ocr_worker::loc::mirror::Mirrors::new(cfg)?))
                }
            };
            let bucket = loc_bucket_config(loc_bucket.as_deref(), loc_bucket_gap_ms)?;
            let loc = Arc::new(LocClient::new(LocConfig {
                global_pacer: loc_global_pacer,
                bucket,
                api_gap: Duration::from_millis(api_gap_ms),
                tile_gap: Duration::from_millis(tile_gap_ms),
                storage_gap: Duration::from_millis(storage_gap_ms),
                storage_own_budget,
                batch_cache: Some(batch_cache),
                html_is_block: true,
                ..LocConfig::default()
            })?);
            let opts = prefetch::Options {
                manifest,
                out,
                size: resolution,
                buffer_size: buffer_resolution,
                sets: match sets {
                    PrefetchSets::Keep => prefetch::Sets::Keep,
                    PrefetchSets::Buffer => prefetch::Sets::Buffer,
                    PrefetchSets::Both => prefetch::Sets::Both,
                },
                buffer_cap,
                limit,
                sample,
                concurrency: concurrency.max(1),
                progress_every,
                order: match order {
                    PrefetchOrder::Priority => prefetch::Order::Priority,
                    PrefetchOrder::Issue => prefetch::Order::Issue,
                },
                max_total_bytes,
                lock_file,
                max_failures_in_a_row: max_failures_in_a_row.max(1),
                stop_min_titles,
                item_cooloff: Duration::from_secs(item_cooloff_secs),
                item_max_cooloffs,
                mirrors,
                awardees: awardees
                    .into_iter()
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty())
                    .collect(),
            };
            let status = prefetch::run(opts, loc, stop_signal()).await?;
            println!("{}", serde_json::to_string(&status)?);
            Ok(())
        }
        Command::BulkOcr {
            out,
            loc_global_pacer,
            list_url,
            gap_secs,
            max_hours,
            min_free_gb,
            limit,
        } => {
            let status = usnm_ocr_worker::bulk::run(
                usnm_ocr_worker::bulk::Options {
                    out,
                    list_url,
                    gap: Duration::from_secs(gap_secs.max(60)),
                    max_hours: max_hours.clamp(0.0, 23.0),
                    min_free_bytes: min_free_gb.saturating_mul(1_000_000_000),
                    limit,
                    global_pacer: loc_global_pacer,
                },
                stop_signal(),
            )
            .await?;
            println!("{}", serde_json::to_string(&status)?);
            Ok(())
        }
        Command::LocAltoStats {
            archives,
            out,
            threshold,
            limit,
        } => {
            anyhow::ensure!((0.0..=1.0).contains(&threshold), "--threshold: 0 to 1");
            let s = tokio::task::spawn_blocking(move || {
                usnm_ocr_worker::loc_alto::run(&usnm_ocr_worker::loc_alto::Options {
                    archives,
                    out,
                    threshold,
                    limit,
                })
            })
            .await??;
            println!("{}", serde_json::to_string(&s)?);
            Ok(())
        }
        Command::LocPacer { file, reset } => {
            let g = usnm_ocr_worker::loc::global::GlobalPacer::new(file);
            if reset {
                g.reset()?;
            }
            println!("{}", serde_json::to_string(&g.state()?)?);
            Ok(())
        }
        Command::ServeScans {
            scans,
            manifests,
            listen,
            allow,
            allow_refresh_secs,
        } => {
            let allow = usnm_ocr_worker::serve::AllowList::resolve(&allow).await?;
            usnm_ocr_worker::serve::serve(
                listen,
                usnm_ocr_worker::serve::ServeOptions {
                    scans,
                    manifests,
                    allow: Arc::new(allow),
                },
                std::time::Duration::from_secs(allow_refresh_secs),
                stop_signal(),
            )
            .await
        }
        Command::ArchiveScans {
            scans,
            archive_url,
            set,
            dry_run,
            verify,
            limit,
            concurrency,
            state,
        } => {
            let mode = if verify {
                usnm_ocr_worker::archive::Mode::Verify
            } else if dry_run {
                usnm_ocr_worker::archive::Mode::DryRun
            } else {
                usnm_ocr_worker::archive::Mode::Upload
            };
            let report = usnm_ocr_worker::archive::run(
                usnm_ocr_worker::archive::Options {
                    scans,
                    set,
                    container_url: archive_url,
                    mode,
                    limit,
                    concurrency,
                    state,
                },
                usnm_store::credential::from_env(),
            )
            .await?;
            println!("{}", serde_json::to_string(&report)?);
            if report.clean() {
                Ok(())
            } else {
                anyhow::bail!("the archive run found problems; see the report")
            }
        }
        Command::PrefetchEstimate {
            manifest,
            index,
            api_gap_ms,
            tile_gap_ms,
        } => {
            let rows = usnm_ocr_contract::prefetch::decode(bytes::Bytes::from(
                std::fs::read(&manifest)
                    .with_context(|| format!("reading {}", manifest.display()))?,
            ))?;
            let index = prefetch::read_index(
                &std::fs::read_to_string(&index)
                    .with_context(|| format!("reading {}", index.display()))?,
            )?;
            let e = prefetch::estimate(
                &rows,
                &index,
                Duration::from_millis(api_gap_ms),
                Duration::from_millis(tile_gap_ms),
            )?;
            println!("{}", serde_json::to_string_pretty(&e)?);
            Ok(())
        }
        Command::Enqueue {
            queue_url,
            page,
            batch,
            priority,
            language,
            score,
        } => {
            let (reason, queue_name) = match priority {
                Priority::User => (Reason::User, PRIORITY),
                Priority::Bulk => (Reason::Bulk, BULK),
            };
            let msg = OcrMessage {
                v: 1,
                page,
                batch,
                language,
                reason,
                score,
                queued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            };
            msg.validate()?;
            queue::open(&queue_url)?
                .send(queue_name, &msg.encode())
                .await?;
            tracing::info!(page = %msg.page, queue = queue_name, "queued");
            Ok(())
        }
        Command::OcrFile {
            image,
            language,
            out,
            engine,
        } => {
            let engine = engine.engine("local").await?;
            // Instances stop before the command returns, whatever it returns.
            let done = async {
                let stem = image
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .context("--image has no file name")?
                    .to_owned();
                std::fs::create_dir_all(&out)?;
                let work = tempfile::tempdir()?;
                let t0 = Instant::now();
                let mut result = engine.ocr(&image, &language, work.path()).await?;
                let secs = t0.elapsed().as_secs_f64();
                // The same stored ALTO and text the worker uploads.
                let sha = usnm_ocr_worker::prefetch::sha256(&std::fs::read(&image)?);
                let url = image.display().to_string();
                let stored = usnm_ocr_worker::stored::from_output(
                    &mut result,
                    &language,
                    engine.dpi(),
                    &usnm_ocr_worker::stored::Source {
                        url: &url,
                        sha256: &sha,
                        md5: None,
                        run_id: None,
                        damage_before: None,
                    },
                    &usnm_ocr_worker::stored::worker_version(),
                    chrono::Utc::now(),
                )?;
                let text = stored.text;
                std::fs::write(out.join(format!("{stem}.txt")), &text)?;
                std::fs::write(out.join(format!("{stem}.xml")), &stored.alto)?;
                let status = usnm_core::text::text_status(&text);
                println!(
                    "{}",
                    serde_json::json!({
                        "image": image.display().to_string(),
                        "language": language,
                        "engine": result.engine,
                        "secs": (secs * 1000.0).round() / 1000.0,
                        "chars": text.chars().count(),
                        "status": usnm_overlay::status_str(status),
                    })
                );
                Ok::<(), anyhow::Error>(())
            }
            .await;
            engine.shutdown().await;
            done
        }
        Command::Engines {
            command: EnginesCommand::Conformance { fixtures, engine },
        } => {
            if !matches!(engine.engine, EngineName::Contract) {
                bail!("conformance runs an engine behind the contract: give --engine contract");
            }
            let report = usnm_ocr_worker::contract::conformance::run(
                engine.contract_options("local")?,
                &fixtures,
            )
            .await?;
            for case in &report.cases {
                println!("{}", serde_json::to_string(case)?);
            }
            let failed: Vec<_> = report
                .cases
                .iter()
                .filter(|c| c.outcome == usnm_ocr_worker::contract::conformance::Outcome::Fail)
                .map(|c| c.case)
                .collect();
            println!(
                "{}",
                serde_json::json!({"engine": report.engine, "passed": report.passed(), "failed": failed})
            );
            if !report.passed() {
                bail!(
                    "{} failed conformance: {}",
                    report.engine,
                    failed.join(", ")
                );
            }
            Ok(())
        }
        Command::CheckEngine { engine } => {
            let engine = engine.engine("local").await?;
            // Instances stop before the command returns, whatever it returns.
            let done = async {
                let work = tempfile::tempdir()?;
                let image = work.path().join("check.pgm");
                std::fs::write(&image, usnm_ocr_worker::testimg::text_image(CHECK_TEXT))?;
                let t0 = Instant::now();
                let mut result = engine.ocr(&image, "eng", work.path()).await?;
                // Through the stored ALTO, as the worker stores a page.
                let stored = usnm_ocr_worker::stored::from_output(
                    &mut result,
                    "eng",
                    engine.dpi(),
                    &usnm_ocr_worker::stored::Source {
                        url: "check.pgm",
                        sha256: "",
                        md5: None,
                        run_id: None,
                        damage_before: None,
                    },
                    &usnm_ocr_worker::stored::worker_version(),
                    chrono::Utc::now(),
                )
                .context("the engine's ALTO doesn't convert to the stored ALTO")?;
                let text = stored.text;
                anyhow::ensure!(
                    text.contains(CHECK_TEXT),
                    "the engine read {text:?}, not {CHECK_TEXT:?}"
                );
                println!(
                    "{}",
                    serde_json::json!({
                        "engine": result.engine,
                        "secs": (t0.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                        "read": text.trim(),
                    })
                );
                Ok::<(), anyhow::Error>(())
            }
            .await;
            engine.shutdown().await;
            done
        }
    }
}
