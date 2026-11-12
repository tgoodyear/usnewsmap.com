//! The commands that carry one reserved job from export to publication: `export`,
//! `prepare-download`, `submit`, `reconcile`, `collect` and `status` (the submission and
//! publication journal), and `work`, which runs a page inside the pinned worker container. Their
//! clap definitions and the dispatch live here; `main.rs` only names them, and each variant's
//! help text is the doc comment on that variant there.
use crate::{records, ExportOptions};
use anyhow::Context;
use clap::{Args, ValueEnum};
use std::{path::PathBuf, sync::Arc};
use usnm_state::{
    docs::{DocStore, FileDocs},
    state::State,
};

#[derive(Args)]
pub struct ExportArgs {
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
    /// The page deadline the volunteer's Tesseract gets (the job's `engine.timeout_secs`, at
    /// most 3600). 1800: a dense 1860s page ran past 600 s on every host (#1159).
    #[arg(long, default_value_t = 1800)]
    timeout_secs: u32,
    /// A directory of `claim pull` files (`<doc_id>.json`) for these pages: they came off
    /// the work queues, so each job keeps its queue message and ledger row.
    #[arg(long)]
    claims: Option<PathBuf>,
    /// A dead-lettered job of this plan whose page an operator retries with another deadline
    /// (`claim deadline-retry`, #1417); repeat for more. Its journal and its
    /// `deadline-retry/<message_sha256>.json` are under --root; the selected page's job takes
    /// the recorded deadline in place of the one the plan journaled for the page.
    #[arg(long = "deadline-retry", requires = "claims")]
    deadline_retries: Vec<String>,
    #[arg(long)]
    curated_url: String,
    #[arg(long)]
    reference_url: String,
    #[arg(long, required_unless_present = "cosmos", conflicts_with = "cosmos")]
    state_file: Option<PathBuf>,
    #[arg(long)]
    cosmos: Option<String>,
    /// Read only the page's lines in this language, one LoC ALTO line box at a time at PSM 13,
    /// with its line model (#1621): `chr`. A page without such lines is skipped and reported.
    #[arg(long, requires = "loc_alto", conflicts_with_all = ["claims", "deadline_retries"])]
    line_read: Option<String>,
    /// LoC's bulk OCR archives unpacked (`<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`),
    /// for --line-read.
    #[arg(long, requires = "line_read")]
    loc_alto: Option<PathBuf>,
}

pub async fn export(args: ExportArgs) -> anyhow::Result<()> {
    let ExportArgs {
        manifest,
        pages,
        batch,
        scans,
        root,
        plan,
        image,
        timeout_secs,
        claims,
        deadline_retries,
        curated_url,
        reference_url,
        state_file,
        cosmos,
        line_read,
        loc_alto,
    } = args;
    let docs: Arc<dyn DocStore> = if let Some(endpoint) = cosmos {
        usnm_state::docs::open_endpoint(&endpoint)?
    } else {
        Arc::new(FileDocs::open(
            state_file.context("--state-file required")?,
        )?)
    };
    let store = usnm_store::open(&curated_url)?;
    let reference = usnm_store::open(&reference_url)?;
    for id in crate::export(
        &ExportOptions {
            manifest,
            pages,
            batch,
            scans,
            root,
            plan,
            image,
            timeout_secs,
            claims,
            deadline_retries,
            line_read,
            loc_alto,
        },
        &State::new(docs),
        store.as_ref(),
        reference.as_ref(),
    )
    .await?
    {
        println!("{id}");
    }
    Ok(())
}

#[derive(Args)]
pub struct RetryArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    spool: PathBuf,
    #[arg(long)]
    job: String,
    /// Reviewed assimilator mask, in decimal (for example 2 for error mask 0x2).
    #[arg(
        long,
        required_unless_present = "expected_no_output_files",
        conflicts_with = "expected_no_output_files"
    )]
    expected_error_mask: Option<u32>,
    #[arg(long)]
    expected_no_output_files: bool,
    /// Already signed and installed usnm_ocr worker version supporting retry contract v2.
    #[arg(long)]
    app_version_num: u32,
    /// Full released BOINC worker image digest, not its Tesseract base digest.
    #[arg(long)]
    image: String,
    #[arg(long)]
    reviewed_reason: String,
    /// The retry's page deadline (`engine.timeout_secs`, 1 to 3600), for a page that ran out of
    /// time (#1159). Default: the failed job's. A resumed retry must give the same value.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=i64::from(crate::claim::MAX_TIMEOUT_SECS)))]
    timeout_secs: Option<u32>,
    #[arg(long)]
    handoff_url: String,
    #[arg(long)]
    curated_url: String,
    #[arg(long)]
    reference_url: String,
    #[arg(long, required_unless_present = "cosmos", conflicts_with = "cosmos")]
    state_file: Option<PathBuf>,
    #[arg(long)]
    cosmos: Option<String>,
}

pub async fn retry(args: RetryArgs) -> anyhow::Result<()> {
    let docs: Arc<dyn DocStore> = if let Some(endpoint) = args.cosmos {
        usnm_state::docs::open_endpoint(&endpoint)?
    } else {
        Arc::new(FileDocs::open(
            args.state_file.context("--state-file required")?,
        )?)
    };
    let expected = match (args.expected_error_mask, args.expected_no_output_files) {
        (Some(mask), false) => crate::retry::ExpectedTerminal::Mask(mask),
        (None, true) => crate::retry::ExpectedTerminal::NoOutputFiles,
        _ => anyhow::bail!("review exactly one terminal error mask or no-output disposition"),
    };
    let curated = usnm_store::open(&args.curated_url)?;
    let reference = usnm_store::open(&args.reference_url)?;
    let handoff = usnm_store::open(&args.handoff_url)?;
    let id = crate::retry::prepare(
        &crate::retry::Options {
            root: &args.root,
            spool: &args.spool,
            previous_job: &args.job,
            expected,
            app_version_num: args.app_version_num,
            image: &args.image,
            reviewed_reason: &args.reviewed_reason,
            timeout_secs: args.timeout_secs,
            handoff: handoff.as_ref(),
        },
        &State::new(docs),
        curated.as_ref(),
        reference.as_ref(),
    )
    .await?;
    println!("{id}");
    Ok(())
}

#[derive(Args)]
pub struct WorkArgs {
    #[arg(long, default_value = "/work")]
    dir: PathBuf,
}

pub async fn work(args: WorkArgs) -> anyhow::Result<()> {
    let WorkArgs { dir } = args;
    crate::worker::run(&dir).await?;
    Ok(())
}

#[derive(Args)]
pub struct PrepareDownloadArgs {
    #[arg(long)]
    page: PathBuf,
    #[arg(long)]
    download_root: PathBuf,
}

pub fn prepare_download(args: PrepareDownloadArgs) -> anyhow::Result<()> {
    let PrepareDownloadArgs {
        page,
        download_root,
    } = args;
    println!(
        "{}",
        serde_json::to_string(&crate::submit::prepare_download(&page, &download_root)?)?
    );
    Ok(())
}

#[derive(Args)]
pub struct SubmitArgs {
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
    /// Native stores are mandatory for retry jobs; ordinary submissions stay compatible.
    #[arg(long)]
    curated_url: Option<String>,
    #[arg(long, conflicts_with = "cosmos")]
    state_file: Option<PathBuf>,
    #[arg(long)]
    cosmos: Option<String>,
    /// The signed usnm_ocr version whose worker reads line jobs (#1621): a line job is pinned
    /// to it, so an older worker never gets one. Required for a line job, unused otherwise.
    #[arg(long)]
    app_version_num: Option<u32>,
    /// A line job, or a `usnm_ocr_native` unit (required there): send it to this one host
    /// (`--target_host`), one calibrated on that worker. Needs
    /// `<enable_assignment>1</enable_assignment>`.
    #[arg(long)]
    target_host: Option<u64>,
    /// The BOINC app: `usnm_ocr` (the worker image) or `usnm_ocr_native` (the native build,
    /// #1780), whose unit is always for `--target-host` and needs `--trust`.
    #[arg(long, value_enum, default_value_t = SubmitApp::UsnmOcr)]
    app: SubmitApp,
    /// The host trust list (`gold/trust.json`): a native unit is created only when it admits
    /// the target host for the job's exact scope and native digest.
    #[arg(long)]
    trust: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum SubmitApp {
    UsnmOcr,
    UsnmOcrNative,
}

pub async fn submit(args: SubmitArgs) -> anyhow::Result<()> {
    let SubmitArgs {
        root,
        project,
        job,
        download_base_url,
        download_root,
        curated_url,
        state_file,
        cosmos,
        app_version_num,
        target_host,
        app,
        trust,
    } = args;
    if app == SubmitApp::UsnmOcrNative {
        anyhow::ensure!(
            app_version_num.is_none()
                && curated_url.is_none()
                && state_file.is_none()
                && cosmos.is_none(),
            "a usnm_ocr_native unit takes no --app-version-num or retry stores"
        );
    }
    let trust = match (app, trust) {
        (SubmitApp::UsnmOcrNative, Some(path)) => Some(crate::trust::Trust::load(&path)?),
        (SubmitApp::UsnmOcrNative, None) => {
            anyhow::bail!("a usnm_ocr_native unit needs --trust, the host trust list")
        }
        (SubmitApp::UsnmOcr, Some(_)) => anyhow::bail!("--trust is for usnm_ocr_native units"),
        (SubmitApp::UsnmOcr, None) => None,
    };
    let download = match (download_base_url, download_root) {
        (Some(base), Some(root)) => Some(crate::submit::Download::new(&base, &root)?),
        (Some(base), None) => Some(crate::submit::Download::remote(&base)?),
        (None, None) => None,
        (None, Some(_)) => anyhow::bail!("download-root requires download-base-url"),
    };
    let native = match (curated_url, state_file, cosmos) {
        (None, None, None) => None,
        (Some(curated), Some(file), None) => Some((
            State::new(Arc::new(FileDocs::open(file)?)),
            usnm_store::open(&curated)?,
        )),
        (Some(curated), None, Some(endpoint)) => Some((
            State::new(usnm_state::docs::open_endpoint(&endpoint)?),
            usnm_store::open(&curated)?,
        )),
        _ => anyhow::bail!("submission native stores require --curated-url and exactly one of --state-file/--cosmos"),
    };
    let sources = native
        .as_ref()
        .map(|(state, curated)| crate::submit::RetryNative {
            state,
            curated: curated.as_ref(),
        });
    if let Some(trust) = &trust {
        crate::submit::submit_native(&root, &project, &job, download.as_ref(), target_host, trust)
            .await?;
        return Ok(());
    }
    let pin = match crate::load(&root, &job)?.job.lines {
        Some(_) => app_version_num,
        None => None,
    };
    crate::submit::submit_placed(
        &root,
        &project,
        &job,
        download.as_ref(),
        sources.as_ref(),
        pin,
        target_host,
    )
    .await?;
    Ok(())
}

#[derive(Args)]
pub struct SubmitBundleArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    project: PathBuf,
    /// A reserved page, in bundle order; 1 to 32. Pages taken out of an earlier bundle, and
    /// a page left alone, go as single-page units.
    #[arg(long = "job", required = true)]
    jobs: Vec<String>,
    /// The `usnm_ocr` app version whose worker reads bundles (`--app_version_num`); required
    /// to submit a bundle, so an older worker never gets one.
    #[arg(long)]
    app_version_num: Option<u32>,
}

pub async fn submit_bundle(args: SubmitBundleArgs) -> anyhow::Result<()> {
    let SubmitBundleArgs {
        root,
        project,
        jobs,
        app_version_num,
    } = args;
    let out = crate::bundle::submit::submit(&root, &project, &jobs, app_version_num).await?;
    for bundle in out.bundles {
        println!("bundle {bundle}");
    }
    for id in out.alone {
        println!("alone {id}");
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Found {
    Present,
    Absent,
}

#[derive(Args)]
pub struct ReconcileArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    job: String,
    #[arg(long, value_enum)]
    found: Found,
}

pub fn reconcile(args: ReconcileArgs) -> anyhow::Result<()> {
    let ReconcileArgs { root, job, found } = args;
    crate::submit::reconcile(&root, &job, matches!(found, Found::Present))?;
    Ok(())
}

#[derive(Args)]
pub struct CollectArgs {
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
    /// the job's engine image; otherwise hold the result in <root>/quarantine/<job>/. A result
    /// of a held scope is kept there too, its record marked held, and this succeeds.
    #[arg(long)]
    trust: Option<PathBuf>,
    /// The BOINC host id of the canonical result (the result table's hostid).
    #[arg(long, requires = "trust")]
    host_id: Option<u64>,
}

pub async fn collect(args: CollectArgs) -> anyhow::Result<()> {
    let CollectArgs {
        root,
        spool,
        job,
        curated_url,
        out_url,
        trust,
        host_id,
    } = args;
    let curated = usnm_store::open(&curated_url)?;
    let out = usnm_store::open(&out_url)?;
    if let Some(trust) = trust {
        let t = crate::trust::Trust::load(&trust)?;
        if let crate::trust::Collected::Held(reason) = crate::trust::collect(
            &root,
            &spool,
            &job,
            &t,
            host_id,
            curated.as_ref(),
            out.as_ref(),
        )
        .await?
        {
            eprintln!("kept, not published: {reason}");
        }
    } else {
        crate::collect(&root, &spool, &job, curated.as_ref(), out.as_ref()).await?;
    }
    Ok(())
}

#[derive(Args)]
pub struct RejectOutputArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    spool: PathBuf,
    #[arg(long)]
    job: String,
    #[arg(long)]
    expect_result_sha256: String,
    #[arg(long)]
    expect_text_sha256: String,
    #[arg(long)]
    expect_alto_sha256: String,
}

pub fn reject_output(args: RejectOutputArgs) -> anyhow::Result<()> {
    crate::reject::reject(
        &args.root,
        &args.spool,
        &args.job,
        [
            &args.expect_result_sha256,
            &args.expect_text_sha256,
            &args.expect_alto_sha256,
        ],
    )
}

#[derive(Args)]
pub struct StatusArgs {
    #[arg(long)]
    root: PathBuf,
}

pub fn status(args: StatusArgs) -> anyhow::Result<()> {
    let StatusArgs { root } = args;
    for r in records(&root)? {
        println!(
            "{}",
            serde_json::json!({"job":r.job.id,"workunit":r.workunit(),"page":r.job.message.page,"stage":r.stage,"error":r.error})
        );
    }
    Ok(())
}

#[derive(Args)]
pub struct LineReportArgs {
    /// The dispatch root `export` journaled the line jobs in.
    #[arg(long)]
    root: PathBuf,
    /// The assimilator's spool, for results not yet imported.
    #[arg(long)]
    spool: Option<PathBuf>,
}

/// One JSON line per line job with a result: its merge counts and rule v1's verdict on the
/// merged page, then a summary line.
pub fn line_report(args: LineReportArgs) -> anyhow::Result<()> {
    let rows = crate::lines::report(&args.root, args.spool.as_deref())?;
    let mut verdicts = std::collections::BTreeMap::<String, usize>::new();
    let mut line_merge = std::collections::BTreeMap::<&str, usize>::new();
    let (mut lines, mut model) = (0, 0);
    for row in &rows {
        println!("{}", serde_json::to_string(row)?);
        *verdicts.entry(row.verdict.clone()).or_default() += 1;
        *line_merge.entry(row.line_merge.verdict).or_default() += 1;
        lines += row.stats.lines;
        model += row.stats.model;
    }
    println!(
        "{}",
        serde_json::json!({
            "pages": rows.len(),
            "lines_read": lines,
            "lines_model": model,
            "lines_fallback": lines - model,
            "line_merge_verdicts": line_merge,
            "verdicts": verdicts,
        })
    );
    Ok(())
}

#[derive(Args)]
pub struct LinePagesArgs {
    /// LoC's bulk OCR for the titles, unpacked: `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`.
    #[arg(long)]
    loc_alto: PathBuf,
    /// `<lccn>=<versioned batch>:<reel>`, e.g. `sn83020866=dlc_misctopsn83020866_ver03:print`.
    #[arg(long = "title", required = true)]
    titles: Vec<crate::lines::stage::Title>,
    /// A directory of local scan copies named `<doc_id>.jpg` (LoC's full-size IIIF JPEGs).
    #[arg(long = "scans")]
    scans: Vec<PathBuf>,
    /// JSONL whose `page` fields are the frozen test set's pages (#1620).
    #[arg(long)]
    test_lines: Option<PathBuf>,
    /// The line route's language.
    #[arg(long, default_value = "chr")]
    language: String,
}

/// One JSON line per page, sorted by page key; a summary on stderr.
pub fn line_pages(args: LinePagesArgs) -> anyhow::Result<()> {
    let mut test = std::collections::BTreeSet::new();
    if let Some(path) = &args.test_lines {
        for line in String::from_utf8(crate::read(path, 64 << 20)?)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(line)?;
            let page = v["page"]
                .as_str()
                .context("a test-set line without a page")?;
            test.insert(page.to_owned());
        }
    }
    let pages = crate::lines::stage::inventory(
        &args.loc_alto,
        &args.titles,
        &args.scans,
        &test,
        &args.language,
    )?;
    for p in &pages {
        println!("{}", serde_json::to_string(p)?);
    }
    let with_lines = pages.iter().filter(|p| p.lines > 0).count();
    let ready = pages
        .iter()
        .filter(|p| p.lines > 0 && p.scan.is_some())
        .count();
    let test_set = pages.iter().filter(|p| p.test_set).count();
    eprintln!(
        "{} pages, {with_lines} with lines to read, {ready} of them with a local scan, {test_set} in the test set",
        pages.len()
    );
    Ok(())
}

#[derive(Args)]
pub struct LineStageArgs {
    /// The page list `line-pages` wrote.
    #[arg(long)]
    pages: PathBuf,
    /// The page keys to stage, one per line: 1 to 100, of one batch.
    #[arg(long)]
    select: PathBuf,
    /// Directories of local scan copies, as for `line-pages`.
    #[arg(long = "scans", required = true)]
    scans: Vec<PathBuf>,
    /// LoC's bulk OCR, unpacked, as for `line-pages`.
    #[arg(long)]
    loc_alto: PathBuf,
    /// The prefetch manifest's directory name under `manifests/`.
    #[arg(long)]
    manifest_name: String,
    /// A new directory for the staging tree.
    #[arg(long)]
    out: PathBuf,
}

/// Writes the staging tree and prints the hand-off export's inputs as `NAME=value` lines.
pub fn line_stage(args: LineStageArgs) -> anyhow::Result<()> {
    let inventory = crate::lines::stage::read_inventory(&args.pages)?;
    let select = String::from_utf8(crate::read(&args.select, 64 * 1024)?)?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let staged = crate::lines::stage::stage(
        &inventory,
        &select,
        &args.scans,
        &args.loc_alto,
        &args.manifest_name,
        &args.out,
    )?;
    println!("HANDOFF_BATCH={}", staged.batch);
    println!("HANDOFF_MANIFEST={}", staged.manifest);
    println!("HANDOFF_PAGES={}", staged.pages.join(","));
    println!("HANDOFF_SCANS={}", staged.scans.join(","));
    Ok(())
}
