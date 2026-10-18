use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use std::{path::PathBuf, sync::Arc};
use usnm_boinc::{records, ExportOptions};
use usnm_state::{
    cosmos::CosmosDocs,
    docs::{DocStore, FileDocs},
    state::State,
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
    HostTrust {
        #[command(subcommand)]
        command: HostTrust,
    },
    /// Export selected cached pages and reserve their native requested-work ledger entries.
    Export {
        #[arg(long)]
        manifest: PathBuf,
        /// Text file: one canonical page key per line, at most 100 pages from one batch.
        #[arg(long)]
        pages: PathBuf,
        #[arg(long)]
        batch: String,
        #[arg(long)]
        scans: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        plan: String,
        #[arg(long)]
        image: String,
        #[arg(long, default_value_t = 600)]
        timeout_secs: u32,
        #[arg(long)]
        curated_url: String,
        #[arg(long)]
        reference_url: String,
        #[arg(long, required_unless_present = "cosmos", conflicts_with = "cosmos")]
        state_file: Option<PathBuf>,
        #[arg(long)]
        cosmos: Option<String>,
    },
    /// Run one page inside the pinned BOINC worker container.
    Work {
        #[arg(long, default_value = "/work")]
        dir: PathBuf,
    },
    /// Publish a page on the local producer; this is NOT public readiness proof.
    PrepareDownload {
        #[arg(long)]
        page: PathBuf,
        #[arg(long)]
        download_root: PathBuf,
    },
    /// Stage and submit one reserved job with stock create_work.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        job: String,
        /// Public HTTPS prefix serving immutable content-addressed page bytes.
        #[arg(long)]
        download_base_url: Option<String>,
        /// Existing local directory mapped to download-base-url (no symlinks).
        #[arg(long, requires = "download_base_url")]
        download_root: Option<PathBuf>,
    },
    /// Reconcile an uncertain submission after checking the server for its workunit name.
    Reconcile {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long, value_enum)]
        found: Found,
    },
    /// Read stock batch_collect_assimilator's spool and publish native ALTO/Parquet.
    Collect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        curated_url: String,
        #[arg(long)]
        out_url: String,
        /// trust.json from `gold trust`: publish only when --host-id passed the gold check for
        /// the job's engine image; otherwise hold the result in <root>/quarantine/<job>/.
        #[arg(long)]
        trust: Option<PathBuf>,
        /// The BOINC host id of the canonical result (the result table's hostid).
        #[arg(long, requires = "trust")]
        host_id: Option<u64>,
    },
    /// Verify the gold bundle and download the original files it refers to (network; outside the
    /// job container).
    GoldStage {
        /// The bundle directory, or its tar.
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        work: PathBuf,
    },
    /// Score an engine on the gold bundle, offline; writes counts only, no text.
    GoldRun {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        work: PathBuf,
        /// Where gold-result.json goes.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "tesseract")]
        tesseract: PathBuf,
        /// Executable called directly with IMAGE LANGUAGE arguments; UTF-8 text on stdout.
        #[arg(long, requires_all = ["engine_name", "engine_version"], conflicts_with = "tesseract")]
        engine_cmd: Option<PathBuf>,
        #[arg(long, requires = "engine_cmd")]
        engine_name: Option<String>,
        #[arg(long, requires = "engine_cmd")]
        engine_version: Option<String>,
        #[arg(long, default_value_t = 3)]
        psm: u8,
        #[arg(long, default_value_t = 300)]
        dpi: u32,
        #[arg(long, default_value_t = 600)]
        timeout_secs: u32,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// The JP2 against IIIF JPEG assessment (#456), app `usnm_assess`.
    #[command(subcommand)]
    Assess(Assess),
    /// The gold check as BOINC work (#462), app `usnm_gold`.
    #[command(subcommand)]
    Gold(Gold),
    /// The Surya 2 test on volunteer CPUs and GPUs (#516), app `usnm_surya`.
    #[command(subcommand)]
    Surya(Surya),
    /// The OCR quality estimate on volunteer CPUs (#522), app `usnm_quality`.
    #[command(subcommand)]
    Quality(Quality),
    /// The vision-language readings on volunteer GPUs and CPUs (#522), app `usnm_qwen`.
    #[command(subcommand)]
    Qwen(Qwen),
    /// Japanese NDLOCR-Lite staging, redundant validation and ocr-ja publication.
    #[command(subcommand)]
    Ndlocr(Ndlocr),
    /// The remote hand-off between the OCR node and the BOINC server (#469, docs/boinc.md).
    #[command(subcommand)]
    Handoff(Handoff),
    /// Inspect the local submission/publication journal; emits no page text.
    Status {
        #[arg(long)]
        root: PathBuf,
    },
}
#[derive(Subcommand)]
enum HostTrust {
    /// Read existing quarantine reasons without connecting to BOINC or modifying results.
    Quarantine {
        #[arg(long)]
        root: PathBuf,
    },
    /// Discover BOINC hosts and issue one gold unit per untrusted host/campaign.
    Onboard {
        #[arg(long)]
        config: PathBuf,
        /// Dispatch work; without this switch only read-only discovery/planning occurs.
        #[arg(long)]
        execute: bool,
    },
    /// Deterministically sample OCR jobs and rerun on a second same-image trusted host.
    SpotSubmit {
        #[arg(long)]
        config: PathBuf,
        /// JSON array of original job digests.
        #[arg(long)]
        jobs: PathBuf,
        #[arg(long)]
        execute: bool,
    },
    /// Validate both canonical results and record agreement; never publishes or grants trust.
    SpotCompare {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        original_spool: PathBuf,
        #[arg(long)]
        spot_spool: PathBuf,
    },
}
#[derive(Subcommand)]
enum Ndlocr {
    /// Stage reason:jpn:doc_id:batch_verNN pages from the archive or polite IIIF fetch.
    Stage {
        #[arg(long = "page", required = true)]
        pages: Vec<usnm_boinc::surya::Page>,
        #[arg(long)]
        root: PathBuf,
        /// Immutable engine BASE image digest; signed wrapper separately pins the worker.
        #[arg(long)]
        engine_image: String,
        #[arg(long, default_value_t = 600)]
        timeout_secs: u32,
        #[arg(long)]
        scan_archive: Option<String>,
        #[arg(long)]
        batch_cache: Option<PathBuf>,
    },
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        doc: String,
    },
    Check {
        #[arg(long, num_args = 3)]
        files: Vec<PathBuf>,
    },
    Compare {
        #[arg(long, num_args = 3)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3)]
        files2: Vec<PathBuf>,
    },
    Collect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        doc: String,
        #[arg(long)]
        curated_url: String,
    },
}

async fn ndlocr(command: Ndlocr) -> anyhow::Result<()> {
    use usnm_boinc::ndlocr as x;
    match command {
        Ndlocr::Stage {
            pages,
            root,
            engine_image,
            timeout_secs,
            scan_archive,
            batch_cache,
        } => {
            println!(
                "staged {}",
                x::stage(
                    &root,
                    &pages,
                    x::engine(engine_image, timeout_secs),
                    scan_archive.as_deref(),
                    batch_cache
                )
                .await?
            );
        }
        Ndlocr::Submit { root, project, doc } => x::submit(&root, &project, &doc).await?,
        Ndlocr::Check { files } => {
            x::load(&files)?;
        }
        Ndlocr::Compare { files, files2 } => x::compare(&files, &files2)?,
        Ndlocr::Collect {
            root,
            spool,
            doc,
            curated_url,
        } => {
            x::collect(
                &root,
                &spool,
                &doc,
                usnm_store::open(&curated_url)?.as_ref(),
            )
            .await?
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum Handoff {
    /// Copy exactly one object, without downloading adjacent jobs or scans.
    GetFile {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Copy every object under a prefix of the hand-off container into a directory.
    Get {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        prefix: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Write every file under a directory to a prefix of the hand-off container.
    Put {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to_url: String,
        #[arg(long)]
        prefix: String,
    },
    /// Server side: copy one exported job into the hand-off root. Prints `reserved` (submit
    /// it) or the stage the server already had.
    Pull {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        job: String,
    },
    /// Server side: write a job's spool files, its host id and trust.json to the container.
    PutSpool {
        #[arg(long)]
        to_url: String,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        host_id: Option<u64>,
        #[arg(long)]
        trust: Option<PathBuf>,
    },
    /// Hand-off job side: copy a job's spool and trust.json (when there) down; prints the
    /// host id, or `unknown`.
    GetSpool {
        #[arg(long)]
        from_url: String,
        #[arg(long)]
        job: String,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        trust_out: PathBuf,
    },
}
#[derive(Subcommand)]
enum Assess {
    /// Draw the stratified OCR sample and the larger crash-only list.
    Sample {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out_ocr: PathBuf,
        #[arg(long)]
        out_crash: PathBuf,
        #[arg(long, default_value_t = 3000)]
        ocr_pages: usize,
        #[arg(long, default_value_t = 20000)]
        crash_pages: usize,
        #[arg(long, default_value_t = 2)]
        min_per_stratum: usize,
        #[arg(long)]
        seed: String,
        /// JSON object: LCCN to the title's MARC language codes.
        #[arg(long)]
        title_languages: Option<PathBuf>,
    },
    /// Fetch sampled pages from LoC (one request at a time) into unit directories.
    Stage {
        #[arg(long)]
        sample: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long, value_enum)]
        mode: AssessMode,
        /// The worker image's full digest (sha256:...), the pinned engine build.
        #[arg(long)]
        engine_image: String,
        /// The Tesseract image the worker image was built on (its TESSERACT_IMAGE).
        #[arg(long)]
        engine_base: String,
        #[arg(long, default_value = "5.5.0")]
        engine_version: String,
        /// Page deadline; defaults to 1200 for ocr and 20 for crash.
        #[arg(long)]
        timeout_secs: Option<u32>,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
        #[arg(long, default_value_t = 100)]
        max_pages: usize,
        #[arg(long)]
        batch_cache: Option<PathBuf>,
        /// The scan archive's container URL (https://<account>.blob.core.windows.net/scans),
        /// read first with the job's identity; omit for LoC only.
        #[arg(long)]
        scan_archive: Option<String>,
    },
    /// Submit staged units with stock create_work, two results each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, value_enum)]
        mode: AssessMode,
        #[arg(long, default_value_t = 500)]
        max_units: usize,
    },
    /// script_validator's init script: check one result's files.
    Check {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
    },
    /// script_validator's compare script: do two results agree?
    Compare {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3, required = true)]
        files2: Vec<PathBuf>,
    },
    /// Write score.py/aggregate.py inputs from the canonical results.
    Collect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}
#[derive(Subcommand)]
enum Gold {
    /// Mirror only the 18 locked v1 LoC JPEGs from cache; no BLN600 or work creation.
    MirrorLoc {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
    },
    /// Server side, once per bundle: fetch the original files (one request at a time) to learn
    /// their sizes and md5s and the crops' reference counts; writes prepared.json.
    Prepare {
        /// The bundle tar.
        #[arg(long)]
        bundle: PathBuf,
        /// The campaign root.
        #[arg(long)]
        root: PathBuf,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
    },
    /// Make units with stock create_work, one result each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        /// How many units the campaign has; units already submitted are skipped.
        #[arg(long, default_value_t = 20)]
        units: u32,
        /// Optional targeted gold campaign; requires the server's enable_assignment.
        #[arg(long)]
        target_host: Option<u64>,
        /// The worker image's full digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The Tesseract image the worker image was built on (its TESSERACT_IMAGE).
        #[arg(long)]
        engine_base: String,
        #[arg(long, default_value = "5.5.0")]
        engine_version: String,
        /// Per crop.
        #[arg(long, default_value_t = 300)]
        timeout_secs: u32,
    },
    /// script_validator's init script: check one result against the bundle's expectations.
    Check {
        #[arg(long)]
        file: PathBuf,
        /// The project's gold/ directory, which submit writes.
        #[arg(long)]
        expected_dir: PathBuf,
    },
    /// Write per-host report tables from the canonical results.
    Collect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build trust.json for `collect --trust` from one or more reports.jsonl files.
    Trust {
        #[arg(long, required = true)]
        reports: Vec<PathBuf>,
        /// The highest overall CER that passes.
        #[arg(long)]
        max_cer: f64,
        /// The exact challenge whose complete reports calibrated this threshold.
        #[arg(long)]
        manifest_sha256: String,
        /// The source language covered by the calibration (three-letter lower-case code).
        #[arg(long)]
        language: String,
        /// The calibrated model profile; Gold uses PSM 3 / 300 DPI.
        #[arg(long)]
        profile: String,
        #[arg(long)]
        out: PathBuf,
    },
}
#[derive(Subcommand)]
enum Surya {
    /// Stage checksummed benchmark images and private references, with no network requests.
    BenchmarkStage {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        manifest_sha256: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        engine_image: String,
        #[arg(long)]
        engine_llama: String,
        #[arg(long)]
        engine_weights: String,
        #[arg(long, default_value = "0.22.1")]
        engine_surya: String,
    },

    /// Get each page's full-size IIIF JPEG (the scan archive, then tile.loc.gov one request at a
    /// time) and write its unit.
    Stage {
        /// group:lang:doc_id:batch[+batch...], e.g.
        /// serbian:srp:sn85055101_1917-04-03_ed-1_seq-3:dlc_arlington_ver01; repeat.
        #[arg(long = "page", required = true)]
        pages: Vec<usnm_boinc::surya::Page>,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The llama.cpp server image it is built on (its USNM_SURYA_LLAMA_IMAGE).
        #[arg(long)]
        engine_llama: String,
        /// Its USNM_SURYA_WEIGHTS.
        #[arg(long)]
        engine_weights: String,
        #[arg(long, default_value = "0.22.1")]
        engine_surya: String,
        #[arg(long, value_delimiter = ',', default_value = "tiled,whole,capped")]
        gpu_modes: Vec<SuryaMode>,
        #[arg(long, value_delimiter = ',', default_value = "tiled")]
        cpu_modes: Vec<SuryaMode>,
        #[arg(long, default_value_t = 1800)]
        gpu_timeout_secs: u32,
        #[arg(long, default_value_t = 5400)]
        cpu_timeout_secs: u32,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
        #[arg(long, default_value_t = 20)]
        max_pages: usize,
        #[arg(long)]
        batch_cache: Option<PathBuf>,
        /// The scan archive's container URL, read first; omit for LoC only.
        #[arg(long)]
        scan_archive: Option<String>,
    },
    /// Hand staged units to create_work, two results each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value_t = 20)]
        max_units: usize,
        /// Experimental single-replica probe, at most 30 units, no fallback hosts.
        #[arg(long)]
        target_host: Option<u64>,
    },
    /// Write a unit from a local image, to check a host or the image without LoC or BOINC.
    /// Inside the worker image the engine values default to the image's own.
    LocalUnit {
        #[arg(long)]
        image: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        #[arg(
            long,
            default_value = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        )]
        engine_image: String,
        #[arg(long, env = "USNM_SURYA_LLAMA_IMAGE")]
        engine_llama: String,
        #[arg(long, env = "USNM_SURYA_WEIGHTS")]
        engine_weights: String,
        #[arg(long, env = "USNM_SURYA_VERSION", default_value = "0.22.1")]
        engine_surya: String,
        #[arg(long, value_delimiter = ',', default_value = "whole")]
        modes: Vec<SuryaMode>,
        #[arg(long, default_value_t = 1800)]
        timeout_secs: u32,
    },
    /// script_validator's init script: check one result (five files); --keep copies it there.
    Check {
        #[arg(long, num_args = 5, required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        keep: Option<PathBuf>,
    },
    /// script_validator's compare script: two results of one unit; --log appends the pair.
    Compare {
        #[arg(long, num_args = 5, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 5, required = true)]
        files2: Vec<PathBuf>,
        #[arg(long, default_value_t = usnm_ocr_contract::surya::PROBE_MIN_DICE)]
        min_dice: f64,
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Print a Markdown table of the kept results and the logged pairs.
    Summary {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        seen: PathBuf,
        #[arg(long)]
        pairs: PathBuf,
    },
}
#[derive(Subcommand)]
enum Quality {
    /// Draw the campaign's pages from the prefetch manifest, stratified as the assessment's sample.
    Sample {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 500)]
        pages: usize,
        #[arg(long, default_value_t = 2)]
        min_per_stratum: usize,
        #[arg(long)]
        seed: String,
        /// JSON object: LCCN to the title's MARC language codes.
        #[arg(long)]
        title_languages: Option<PathBuf>,
        /// Doc ids that have an American Stories text, one per line; pages not in it are left out.
        #[arg(long)]
        with_as: Option<PathBuf>,
        /// Explicit doc ids, one per line, instead of a random draw.
        #[arg(long)]
        pages_file: Option<PathBuf>,
    },
    /// Export each page's non-LoC texts (American Stories, ocr-local) and our ALTO from the curated store.
    TextsExport {
        #[arg(long)]
        curated_url: String,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long)]
        sample: Option<PathBuf>,
        #[arg(long = "doc-id")]
        doc_ids: Vec<String>,
    },
    /// Write <lang>.txt function-word files from the embedded word lists.
    FunctionWords {
        #[arg(long = "lang", required = true)]
        langs: Vec<String>,
        #[arg(long, default_value_t = 100)]
        top: usize,
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// Fetch each page's JPEG and ALTO from LoC (or read the hand-off scans), join its texts, write its unit.
    Stage {
        /// Repeatable page spec: group:lang:doc_id:batch[+batch...] (batches as <name>_ver<NN>).
        #[arg(long = "page")]
        pages: Vec<usnm_boinc::surya::Page>,
        /// Or a sample (quality sample): its rows become pages of --group.
        #[arg(long)]
        sample: Option<PathBuf>,
        #[arg(long, default_value = "sample")]
        group: String,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The hand-off job's texts/ export: one <doc_id>.json per page.
        #[arg(long)]
        texts_dir: PathBuf,
        /// The hand-off container's scans/ (<doc_id>.jpg, <doc_id>.xml), read instead of LoC when present.
        #[arg(long)]
        scans_dir: Option<PathBuf>,
        /// The campaign's place names: a .txt (one per line) or the catalog's places.json.
        #[arg(long)]
        places: PathBuf,
        /// Directory of <lang>.txt function-word files, one word per line.
        #[arg(long, alias = "function-words")]
        function_words_dir: PathBuf,
        /// The readers every unit compares, in report order.
        #[arg(long, value_delimiter = ',', default_value = "loc,as")]
        readers: Vec<String>,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
        #[arg(long, default_value_t = 5)]
        gap_secs: u64,
        #[arg(long, default_value_t = 20)]
        max_pages: usize,
        /// Pack staged pages into multi-page bundle workunits of up to this many pages (1 to 20).
        #[arg(long, default_value_t = 1)]
        bundle_size: usize,
        /// The scan archive's container URL (https://<account>.blob.core.windows.net/scans); read first with the job's identity.
        #[arg(long)]
        scan_archive: Option<String>,
        /// Where batch manifests and METS are kept between runs.
        #[arg(long)]
        batch_cache: Option<PathBuf>,
    },
    /// Hand staged units to create_work, two results each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value_t = 100)]
        max_units: usize,
    },
    /// Write a unit from local files, to check a host or the image without LoC or BOINC.
    LocalUnit {
        #[arg(long)]
        page: PathBuf,
        #[arg(long)]
        alto: PathBuf,
        /// texts.json.
        #[arg(long)]
        texts: PathBuf,
        #[arg(long)]
        places: PathBuf,
        #[arg(long)]
        function_words: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        #[arg(
            long,
            default_value = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        )]
        engine_image: String,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
    },
    /// script_validator's init script: check one result (three files); --keep copies it there.
    Check {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        keep: Option<PathBuf>,
    },
    /// script_validator's compare script: two results of one unit; --log appends the pair.
    Compare {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3, required = true)]
        files2: Vec<PathBuf>,
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Join both apps' canonical results per page into the campaign's Parquet tables and summary.
    Collect {
        #[arg(long)]
        campaign: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        qwen_root: PathBuf,
        #[arg(long)]
        qwen_spool: PathBuf,
        /// A local directory instead of the curated store.
        #[arg(
            long,
            required_unless_present = "curated_url",
            conflicts_with = "curated_url"
        )]
        out_dir: Option<PathBuf>,
        #[arg(long)]
        curated_url: Option<String>,
    },
    /// Run the estimator on gold crops with a transcription and report its error per reader.
    Calibrate {
        /// Gold-style crops.jsonl with transcriptions.
        #[arg(long)]
        crops: PathBuf,
        /// ENGINE=FILE.jsonl (rows {"crop_id", "text"}); repeat.
        #[arg(long = "hyp", required = true)]
        hyps: Vec<String>,
        /// Vision-language readings per crop (rows {"crop_id", "text"}).
        #[arg(long)]
        readings: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Run the unit's computation offline on <dir>/<doc_id>/{page.jpg, loc.xml, texts.json}.
    Estimate {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        places: PathBuf,
        #[arg(long)]
        function_words: PathBuf,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
        /// Survey layout (<dir>/sample/images): NAME=PATTERN, the * standing for the page name; repeat.
        #[arg(long = "reader")]
        readers: Vec<String>,
    },
    /// Print a Markdown table of the kept results and the logged pairs.
    Summary {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        seen: PathBuf,
        #[arg(long)]
        pairs: PathBuf,
    },
}
#[derive(Subcommand)]
enum Qwen {
    /// Stage public gold crops without sending reference text to volunteers.
    BenchmarkStage {
        #[arg(long)]
        crops: PathBuf,
        #[arg(long)]
        root: PathBuf,
        /// JSON Engine with immutable image/llama/weights digests and exactly one model.
        #[arg(long)]
        engine: PathBuf,
        #[arg(long, default_value_t = 20)]
        max_crops: usize,
        #[arg(long, default_value_t = 1800)]
        timeout_secs: u32,
    },
    /// Score canonical benchmark outputs; missing hypotheses count as deletions.
    BenchmarkCollect {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        spool: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Cut each staged quality page's crops (the same seed) and write its unit.
    Stage {
        /// The usnm_quality staging root whose units to cut crops from.
        #[arg(long)]
        quality_root: PathBuf,
        #[arg(long)]
        root: PathBuf,
        /// The worker image's registry digest (sha256:...).
        #[arg(long)]
        engine_image: String,
        /// The llama.cpp server image it is built on (its USNM_QWEN_LLAMA_IMAGE).
        #[arg(long)]
        engine_llama: String,
        /// Its USNM_QWEN_WEIGHTS.
        #[arg(long)]
        engine_weights: String,
        /// GPU runtime per page, including all crops; allow slower volunteer GPUs.
        #[arg(long, default_value_t = 14_400)]
        gpu_timeout_secs: u32,
        #[arg(long, default_value_t = 7200)]
        cpu_timeout_secs: u32,
        /// Output tokens allowed per 1,000 square pixels of crop.
        #[arg(long, default_value_t = 4)]
        max_tokens_per_1000px2: u32,
        #[arg(long, default_value_t = 20)]
        max_pages: usize,
    },
    /// Hand staged units to create_work, two results each.
    Submit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: PathBuf,
        #[arg(long, default_value_t = 20)]
        max_units: usize,
        #[arg(long, value_enum, default_value_t = usnm_boinc::qwen::submit::Target::Container)]
        target: usnm_boinc::qwen::submit::Target,
        #[arg(long)]
        model_dir: Option<PathBuf>,
    },
    /// Write a unit from a local page and ALTO, to check a host or the image without LoC or BOINC.
    /// Inside the worker image the engine values default to the image's own.
    LocalUnit {
        #[arg(
            long,
            required_unless_present = "synthetic",
            conflicts_with = "synthetic"
        )]
        image: Option<PathBuf>,
        #[arg(
            long,
            required_unless_present = "synthetic",
            conflicts_with = "synthetic"
        )]
        alto: Option<PathBuf>,
        /// Use the built-in synthetic page (two lines of block capitals) instead of --image and --alto.
        #[arg(long)]
        synthetic: bool,
        #[arg(long)]
        dir: PathBuf,
        #[arg(
            long,
            default_value = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        )]
        engine_image: String,
        #[arg(long, env = "USNM_QWEN_LLAMA_IMAGE")]
        engine_llama: String,
        #[arg(long, env = "USNM_QWEN_WEIGHTS")]
        engine_weights: String,
        #[arg(long, default_value_t = 522)]
        crop_seed: u64,
        #[arg(long, default_value_t = 12)]
        k_crops: u32,
        #[arg(long, default_value_t = 1800)]
        timeout_secs: u32,
        #[arg(long, default_value_t = 4)]
        max_tokens_per_1000px2: u32,
    },
    /// Print llama-server's version and the model files in the image, without loading a model.
    Probe,
    /// script_validator's init script: check one result (three files); --keep copies it there.
    Check {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        keep: Option<PathBuf>,
    },
    /// script_validator's compare script: two results of one unit; --log appends the pair.
    Compare {
        #[arg(long, num_args = 3, required = true)]
        files: Vec<PathBuf>,
        #[arg(long, num_args = 3, required = true)]
        files2: Vec<PathBuf>,
        #[arg(long, default_value_t = usnm_ocr_contract::qwen::PROBE_MIN_DICE)]
        min_dice: f64,
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Print a Markdown table of the kept results and the logged pairs.
    Summary {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        seen: PathBuf,
        #[arg(long)]
        pairs: PathBuf,
    },
}

#[cfg(test)]
mod qwen_runtime_tests {
    use super::*;

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
            let Commands::Qwen(Qwen::Stage {
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
#[derive(Clone, Copy, ValueEnum)]
enum SuryaMode {
    Tiled,
    Whole,
    Capped,
}
impl From<SuryaMode> for usnm_ocr_contract::surya::InputMode {
    fn from(m: SuryaMode) -> Self {
        match m {
            SuryaMode::Tiled => Self::Tiled,
            SuryaMode::Whole => Self::Whole,
            SuryaMode::Capped => Self::Capped,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum AssessMode {
    Ocr,
    Crash,
}
impl From<AssessMode> for usnm_ocr_contract::assess::Mode {
    fn from(m: AssessMode) -> Self {
        match m {
            AssessMode::Ocr => Self::Ocr,
            AssessMode::Crash => Self::Crash,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum Found {
    Present,
    Absent,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Commands::HostTrust { command } => {
            use usnm_boinc::host_trust as x;
            match command {
                HostTrust::Quarantine { root } => {
                    println!("{}", serde_json::to_string_pretty(&x::quarantine(&root)?)?);
                }
                HostTrust::Onboard { config, execute } => {
                    let hosts = x::onboard(&x::Config::load(&config)?, execute).await?;
                    println!("{}", serde_json::to_string(&hosts)?);
                }
                HostTrust::SpotSubmit {
                    config,
                    jobs,
                    execute,
                } => {
                    let units = x::spot_submit(&x::Config::load(&config)?, &jobs, execute).await?;
                    println!("{}", serde_json::to_string(&units)?);
                }
                HostTrust::SpotCompare {
                    config,
                    job,
                    original_spool,
                    spot_spool,
                } => {
                    anyhow::ensure!(
                        x::spot_compare(&x::Config::parse(&config)?, &job, &original_spool, &spot_spool).await?,
                        "independent OCR spot check disagrees; inspect comparison.json; no trust or publication changed"
                    );
                }
            }
        }
        Commands::Export {
            manifest,
            pages,
            batch,
            scans,
            root,
            plan,
            image,
            timeout_secs,
            curated_url,
            reference_url,
            state_file,
            cosmos,
        } => {
            let docs: Arc<dyn DocStore> = if let Some(endpoint) = cosmos {
                Arc::new(CosmosDocs::new(
                    &endpoint,
                    usnm_state::DATABASE,
                    usnm_store::credential::from_env_for(usnm_store::credential::COSMOS_RESOURCE),
                )?)
            } else {
                Arc::new(FileDocs::open(
                    state_file.context("--state-file required")?,
                )?)
            };
            let store = usnm_store::open(&curated_url)?;
            let reference = usnm_store::open(&reference_url)?;
            for id in usnm_boinc::export(
                &ExportOptions {
                    manifest,
                    pages,
                    batch,
                    scans,
                    root,
                    plan,
                    image,
                    timeout_secs,
                },
                &State::new(docs),
                store.as_ref(),
                reference.as_ref(),
            )
            .await?
            {
                println!("{id}");
            }
        }
        Commands::Work { dir } => usnm_boinc::worker::run(&dir).await?,
        Commands::PrepareDownload {
            page,
            download_root,
        } => {
            println!(
                "{}",
                serde_json::to_string(&usnm_boinc::submit::prepare_download(
                    &page,
                    &download_root
                )?)?
            );
        }
        Commands::Submit {
            root,
            project,
            job,
            download_base_url,
            download_root,
        } => {
            let download = match (download_base_url, download_root) {
                (Some(base), Some(root)) => Some(usnm_boinc::submit::Download::new(&base, &root)?),
                (Some(base), None) => Some(usnm_boinc::submit::Download::remote(&base)?),
                (None, None) => None,
                (None, Some(_)) => anyhow::bail!("download-root requires download-base-url"),
            };
            usnm_boinc::submit::submit_with_download(&root, &project, &job, download.as_ref())
                .await?
        }
        Commands::Reconcile { root, job, found } => {
            usnm_boinc::submit::reconcile(&root, &job, matches!(found, Found::Present))?
        }
        Commands::Collect {
            root,
            spool,
            job,
            curated_url,
            out_url,
            trust,
            host_id,
        } => {
            let curated = usnm_store::open(&curated_url)?;
            let out = usnm_store::open(&out_url)?;
            if let Some(trust) = trust {
                let t = usnm_boinc::trust::Trust::load(&trust)?;
                usnm_boinc::trust::collect(
                    &root,
                    &spool,
                    &job,
                    &t,
                    host_id,
                    curated.as_ref(),
                    out.as_ref(),
                )
                .await?;
            } else {
                usnm_boinc::collect(&root, &spool, &job, curated.as_ref(), out.as_ref()).await?;
            }
        }
        Commands::GoldStage {
            bundle,
            cache,
            work,
        } => {
            let s = usnm_boinc::gold::stage(&bundle, &cache, &work).await?;
            println!("{}", serde_json::to_string(&s)?);
            anyhow::ensure!(s.failed.is_empty(), "{} downloads failed", s.failed.len());
        }
        Commands::GoldRun {
            bundle,
            cache,
            work,
            out,
            tesseract,
            engine_cmd,
            engine_name,
            engine_version,
            psm,
            dpi,
            timeout_secs,
            limit,
        } => {
            let report = usnm_boinc::gold::run(&usnm_boinc::gold::RunOptions {
                bundle,
                cache,
                work,
                tesseract,
                command: engine_cmd.map(|executable| usnm_boinc::gold::CommandEngine {
                    executable,
                    name: engine_name.expect("required by clap"),
                    version: engine_version.expect("required by clap"),
                }),
                psm,
                dpi,
                timeout_secs,
                limit,
            })
            .await?;
            usnm_boinc::atomic(&out, &serde_json::to_vec_pretty(&report)?)?;
            println!("{}", serde_json::to_string(&report.summary)?);
        }
        Commands::Assess(a) => assess(a).await?,
        Commands::Gold(g) => gold(g).await?,
        Commands::Surya(x) => surya(x).await?,
        Commands::Ndlocr(x) => ndlocr(x).await?,
        Commands::Quality(q) => quality(q).await?,
        Commands::Qwen(q) => qwen(q).await?,
        Commands::Handoff(h) => handoff(h).await?,
        Commands::Status { root } => {
            for r in records(&root)? {
                println!(
                    "{}",
                    serde_json::json!({"job":r.job.id,"workunit":r.job.workunit(),"page":r.job.message.page,"stage":r.stage,"error":r.error})
                );
            }
        }
    }
    Ok(())
}

async fn handoff(h: Handoff) -> anyhow::Result<()> {
    use usnm_boinc::handoff as x;
    match h {
        Handoff::GetFile { from_url, path, to } => {
            x::get_file(usnm_store::open(&from_url)?.as_ref(), &path, &to).await?;
        }
        Handoff::Get {
            from_url,
            prefix,
            to,
        } => {
            let n = x::get(usnm_store::open(&from_url)?.as_ref(), &prefix, &to).await?;
            eprintln!("{n} objects copied from {prefix}/ to {}", to.display());
        }
        Handoff::Put {
            from,
            to_url,
            prefix,
        } => {
            let n = x::put(usnm_store::open(&to_url)?.as_ref(), &from, &prefix).await?;
            eprintln!("{n} files written from {} to {prefix}/", from.display());
        }
        Handoff::Pull {
            from_url,
            root,
            job,
        } => match x::pull(usnm_store::open(&from_url)?.as_ref(), &root, &job).await? {
            x::Pulled::Reserved => println!("reserved"),
            x::Pulled::Already(stage) => {
                println!("{}", serde_json::to_value(stage)?.as_str().unwrap_or("?"))
            }
        },
        Handoff::PutSpool {
            to_url,
            spool,
            job,
            host_id,
            trust,
        } => {
            let store = usnm_store::open(&to_url)?;
            let n = x::put_spool(store.as_ref(), &spool, &job, host_id, trust.as_deref()).await?;
            eprintln!("{n} spool files written for {job}");
        }
        Handoff::GetSpool {
            from_url,
            job,
            spool,
            trust_out,
        } => {
            let store = usnm_store::open(&from_url)?;
            let host = x::get_spool(store.as_ref(), &job, &spool).await?;
            if !x::get_trust(store.as_ref(), &trust_out).await? {
                eprintln!("no gold/trust.json in the hand-off container");
            }
            println!("{}", host.map_or("unknown".into(), |h| h.to_string()));
        }
    }
    Ok(())
}

async fn gold(g: Gold) -> anyhow::Result<()> {
    use usnm_boinc::gold as x;
    match g {
        Gold::MirrorLoc { root, project } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&x::mirror::loc(&root, &project).await?)?
            );
        }
        Gold::Prepare {
            bundle,
            root,
            gap_secs,
        } => {
            let p = x::prepare::run(&x::prepare::Options {
                bundle,
                root,
                gap: std::time::Duration::from_secs(gap_secs),
            })
            .await?;
            eprintln!(
                "bundle v{} manifest {}: {} crops, {} original files",
                p.bundle_version,
                p.manifest_sha256,
                p.crops.len(),
                p.remotes.len()
            );
        }
        Gold::Submit {
            root,
            project,
            units,
            target_host,
            engine_image,
            engine_base,
            engine_version,
            timeout_secs,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                units,
                target_host,
                engine: usnm_boinc::gold::job::Engine {
                    base: engine_base,
                    dpi: 300,
                    image: engine_image,
                    psm: 3,
                    timeout_secs,
                    version: engine_version,
                },
            })
            .await?;
            eprintln!("{s:?}");
        }
        Gold::Check { file, expected_dir } => x::validate::check(&file, &expected_dir)?,
        Gold::Collect {
            root,
            project,
            spool,
            out,
        } => {
            let s = x::collect::run(&x::collect::Options {
                root,
                project,
                spool,
                out,
            })?;
            eprintln!("{s:?}");
        }
        Gold::Trust {
            reports,
            max_cer,
            manifest_sha256,
            language,
            profile,
            out,
        } => {
            let t = usnm_boinc::trust::Trust::build(
                &reports,
                max_cer,
                &manifest_sha256,
                usnm_boinc::trust::Scope {
                    language,
                    profile,
                    psm: 3,
                    dpi: 300,
                },
            )?;
            let out = if out.parent().is_some_and(|p| p.as_os_str().is_empty()) {
                std::path::Path::new(".").join(out)
            } else {
                out
            };
            usnm_boinc::atomic(&out, &serde_json::to_vec_pretty(&t)?)?;
            for (image, hosts) in &t.hosts {
                eprintln!("{image}: {} trusted hosts", hosts.len());
            }
        }
    }
    Ok(())
}

async fn surya(c: Surya) -> anyhow::Result<()> {
    use usnm_boinc::surya as x;
    match c {
        Surya::BenchmarkStage {
            manifest,
            manifest_sha256,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
        } => {
            let count = x::stage::benchmark(
                &manifest,
                &manifest_sha256,
                &root,
                usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    weights: engine_weights,
                    surya: engine_surya,
                },
            )?;
            eprintln!("staged {count} immutable benchmark inputs");
        }

        Surya::Stage {
            pages,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
            gpu_modes,
            cpu_modes,
            gpu_timeout_secs,
            cpu_timeout_secs,
            gap_secs,
            max_pages,
            batch_cache,
            scan_archive,
        } => {
            let s = x::stage::run(&x::stage::Options {
                pages,
                root,
                engine: usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    surya: engine_surya,
                    weights: engine_weights,
                },
                gpu_modes: gpu_modes.into_iter().map(Into::into).collect(),
                cpu_modes: cpu_modes.into_iter().map(Into::into).collect(),
                gpu_timeout_secs,
                cpu_timeout_secs,
                gap: std::time::Duration::from_secs(gap_secs),
                max_pages,
                batch_cache,
                scan_archive,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Surya::Submit {
            root,
            project,
            max_units,
            target_host,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                max_units,
                target_host,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Surya::LocalUnit {
            image,
            dir,
            engine_image,
            engine_llama,
            engine_weights,
            engine_surya,
            modes,
            timeout_secs,
        } => {
            let j = x::stage::local_unit(
                &image,
                &dir,
                usnm_ocr_contract::surya::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    surya: engine_surya,
                    weights: engine_weights,
                },
                modes.into_iter().map(Into::into).collect(),
                timeout_secs,
            )?;
            println!("{}", j.id);
        }
        Surya::Check { files, keep } => x::validate::check(&files, keep.as_deref())?,
        Surya::Compare {
            files,
            files2,
            min_dice,
            log,
        } => x::validate::compare(&files, &files2, min_dice, log.as_deref())?,
        Surya::Summary { root, seen, pairs } => {
            print!("{}", x::summary::run(&root, &seen, &pairs)?)
        }
    }
    Ok(())
}

async fn quality(c: Quality) -> anyhow::Result<()> {
    use usnm_boinc::quality as x;
    match c {
        Quality::Sample {
            manifest,
            out,
            pages,
            min_per_stratum,
            seed,
            title_languages,
            with_as,
            pages_file,
        } => {
            let s = x::sample::run(&x::sample::Options {
                manifest,
                out,
                pages,
                min_per_stratum,
                seed,
                title_languages,
                with_as,
                pages_file,
            })?;
            eprintln!("{s:?}");
        }
        Quality::TextsExport {
            curated_url,
            out_dir,
            sample,
            doc_ids,
        } => {
            let s = x::stage::texts_export(&x::stage::TextsExportOptions {
                curated_url,
                out_dir,
                sample,
                doc_ids,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Quality::FunctionWords {
            langs,
            top,
            out_dir,
        } => {
            x::stage::write_function_words(&langs, top, &out_dir)?;
        }
        Quality::Stage {
            pages,
            sample,
            group,
            root,
            engine_image,
            texts_dir,
            scans_dir,
            places,
            function_words_dir,
            readers,
            crop_seed,
            k_crops,
            gap_secs,
            max_pages,
            bundle_size,
            scan_archive,
            batch_cache,
        } => {
            let s = x::stage::run(&x::stage::Options {
                pages,
                sample,
                group,
                root,
                engine: usnm_ocr_contract::quality::Engine {
                    image: engine_image,
                },
                texts_dir,
                scans_dir,
                places,
                function_words_dir,
                readers,
                crop_seed,
                k_crops,
                gap: std::time::Duration::from_secs(gap_secs),
                max_pages,
                bundle_size,
                scan_archive,
                batch_cache,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Quality::Submit {
            root,
            project,
            max_units,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                max_units,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Quality::LocalUnit {
            page,
            alto,
            texts,
            places,
            function_words,
            dir,
            engine_image,
            crop_seed,
            k_crops,
        } => {
            let texts: usnm_ocr_contract::quality::Texts = serde_json::from_slice(
                &usnm_boinc::read(&texts, usnm_ocr_contract::quality::MAX_TEXTS)?,
            )?;
            let j = x::stage::local_unit(&x::stage::LocalUnit {
                page: &usnm_boinc::read(&page, usnm_ocr_contract::quality::MAX_SCAN)?,
                alto: &usnm_boinc::read(&alto, usnm_ocr_contract::quality::MAX_ALTO)?,
                texts: &texts,
                places: &std::fs::read_to_string(&places)?,
                dir: &dir,
                engine: usnm_ocr_contract::quality::Engine {
                    image: engine_image,
                },
                function_words: x::stage::read_words(&function_words)?,
                crop_seed,
                k_crops,
            })?;
            println!("{}", j.id);
        }
        Quality::Check { files, keep } => x::validate::check(&files, keep.as_deref())?,
        Quality::Compare { files, files2, log } => {
            x::validate::compare(&files, &files2, log.as_deref())?
        }
        Quality::Collect {
            campaign,
            root,
            spool,
            qwen_root,
            qwen_spool,
            out_dir,
            curated_url,
        } => {
            let s = x::collect::run(&x::collect::Options {
                campaign,
                root,
                spool,
                qwen_root,
                qwen_spool,
                out_dir,
                curated_url,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Quality::Calibrate {
            crops,
            hyps,
            readings,
            out,
        } => {
            let rows = x::calibrate::run(&x::calibrate::Options {
                crops,
                hyps,
                readings,
                out,
            })?;
            for r in rows {
                println!("{}", serde_json::to_string(&r)?);
            }
        }
        Quality::Estimate {
            dir,
            out,
            places,
            function_words,
            crop_seed,
            k_crops,
            readers,
        } => {
            let s = x::estimate::run(&x::estimate::Options {
                dir,
                out,
                places,
                function_words,
                crop_seed,
                k_crops,
                readers,
            })?;
            eprintln!("{s:?}");
        }
        Quality::Summary { root, seen, pairs } => {
            print!("{}", x::summary::run(&root, &seen, &pairs)?)
        }
    }
    Ok(())
}

async fn qwen(c: Qwen) -> anyhow::Result<()> {
    use usnm_boinc::qwen as x;
    match c {
        Qwen::BenchmarkStage {
            crops,
            root,
            engine,
            max_crops,
            timeout_secs,
        } => {
            let engine = serde_json::from_slice(&std::fs::read(engine)?)?;
            let staged = x::benchmark::stage(&x::benchmark::Options {
                crops,
                root,
                engine,
                max_crops,
                timeout_secs,
            })?;
            eprintln!("{staged} benchmark units staged");
        }
        Qwen::BenchmarkCollect { root, spool, out } => {
            let summary = x::benchmark::collect(&root, &spool, &out)?;
            eprintln!("{}", serde_json::to_string(&summary)?);
        }
        Qwen::Stage {
            quality_root,
            root,
            engine_image,
            engine_llama,
            engine_weights,
            gpu_timeout_secs,
            cpu_timeout_secs,
            max_tokens_per_1000px2,
            max_pages,
        } => {
            let s = x::stage::run(&x::stage::Options {
                quality_root,
                root,
                engine: usnm_ocr_contract::qwen::Engine {
                    image: engine_image,
                    llama: engine_llama,
                    models: x::default_models(),
                    weights: engine_weights,
                },
                gpu_timeout_secs,
                cpu_timeout_secs,
                max_tokens_per_1000px2,
                max_pages,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Qwen::Submit {
            root,
            project,
            max_units,
            target,
            model_dir,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                max_units,
                target,
                model_dir,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Qwen::LocalUnit {
            image,
            alto,
            synthetic,
            dir,
            engine_image,
            engine_llama,
            engine_weights,
            crop_seed,
            k_crops,
            timeout_secs,
            max_tokens_per_1000px2,
        } => {
            let engine = usnm_ocr_contract::qwen::Engine {
                image: engine_image,
                llama: engine_llama,
                models: x::default_models(),
                weights: engine_weights,
            };
            let j = if synthetic {
                x::stage::synthetic_unit(
                    &dir,
                    engine,
                    k_crops,
                    timeout_secs,
                    max_tokens_per_1000px2,
                )?
            } else {
                x::stage::local_unit(
                    &image.context("--image")?,
                    &alto.context("--alto")?,
                    &dir,
                    engine,
                    crop_seed,
                    k_crops,
                    timeout_secs,
                    max_tokens_per_1000px2,
                )?
            };
            println!("{}", j.id);
        }
        Qwen::Probe => print!("{}", x::work::probe()),
        Qwen::Check { files, keep } => x::validate::check(&files, keep.as_deref())?,
        Qwen::Compare {
            files,
            files2,
            min_dice,
            log,
        } => x::validate::compare(&files, &files2, min_dice, log.as_deref())?,
        Qwen::Summary { root, seen, pairs } => {
            print!("{}", x::summary::run(&root, &seen, &pairs)?)
        }
    }
    Ok(())
}

async fn assess(a: Assess) -> anyhow::Result<()> {
    use usnm_boinc::assess as x;
    match a {
        Assess::Sample {
            manifest,
            out_ocr,
            out_crash,
            ocr_pages,
            crash_pages,
            min_per_stratum,
            seed,
            title_languages,
        } => {
            let s = x::sample::run(&x::sample::Options {
                manifest,
                out_ocr,
                out_crash,
                ocr_pages,
                crash_pages,
                min_per_stratum,
                seed,
                title_languages,
            })?;
            eprintln!("{s:?}");
        }
        Assess::Stage {
            sample,
            root,
            mode,
            engine_image,
            engine_base,
            engine_version,
            timeout_secs,
            gap_secs,
            max_pages,
            batch_cache,
            scan_archive,
        } => {
            let mode = mode.into();
            let s = x::stage::run(&x::stage::Options {
                sample,
                root,
                mode,
                engine_image,
                engine_base,
                engine_version,
                timeout_secs: timeout_secs.unwrap_or(match mode {
                    usnm_ocr_contract::assess::Mode::Ocr => 1200,
                    usnm_ocr_contract::assess::Mode::Crash => 20,
                }),
                gap: std::time::Duration::from_secs(gap_secs),
                max_pages,
                batch_cache,
                scan_archive,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Assess::Submit {
            root,
            project,
            mode,
            max_units,
        } => {
            let s = x::submit::run(&x::submit::Options {
                root,
                project,
                mode: mode.into(),
                max_units,
            })
            .await?;
            eprintln!("{s:?}");
        }
        Assess::Check { files } => x::validate::check(&files)?,
        Assess::Compare { files, files2 } => x::validate::compare(&files, &files2)?,
        Assess::Collect { root, spool, out } => {
            let s = x::collect::run(&x::collect::Options { root, spool, out })?;
            eprintln!("{s:?}");
        }
    }
    Ok(())
}
