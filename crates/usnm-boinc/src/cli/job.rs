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
    cosmos::CosmosDocs,
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
    #[arg(long, default_value_t = 600)]
    timeout_secs: u32,
    /// A directory of `claim pull` files (`<doc_id>.json`) for these pages: they came off
    /// the work queues, so each job keeps its queue message and ledger row.
    #[arg(long)]
    claims: Option<PathBuf>,
    #[arg(long)]
    curated_url: String,
    #[arg(long)]
    reference_url: String,
    #[arg(long, required_unless_present = "cosmos", conflicts_with = "cosmos")]
    state_file: Option<PathBuf>,
    #[arg(long)]
    cosmos: Option<String>,
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
        curated_url,
        reference_url,
        state_file,
        cosmos,
    } = args;
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
}

pub async fn submit(args: SubmitArgs) -> anyhow::Result<()> {
    let SubmitArgs {
        root,
        project,
        job,
        download_base_url,
        download_root,
    } = args;
    let download = match (download_base_url, download_root) {
        (Some(base), Some(root)) => Some(crate::submit::Download::new(&base, &root)?),
        (Some(base), None) => Some(crate::submit::Download::remote(&base)?),
        (None, None) => None,
        (None, Some(_)) => anyhow::bail!("download-root requires download-base-url"),
    };
    crate::submit::submit_with_download(&root, &project, &job, download.as_ref()).await?;
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
    if let Some(bundle) = out.bundle {
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
    /// the job's engine image; otherwise hold the result in <root>/quarantine/<job>/.
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
        crate::trust::collect(
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
        crate::collect(&root, &spool, &job, curated.as_ref(), out.as_ref()).await?;
    }
    Ok(())
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
