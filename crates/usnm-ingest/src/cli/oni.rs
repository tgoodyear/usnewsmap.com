//! `oni-fetch`: batches from Penn State's Open ONI site (#1592), made into retained archives that
//! `enqueue` and `curate` take as they take LoC's.
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use chrono::NaiveDate;
use clap::Args;
use usnm_congress::fetch::Http;
use usnm_ingest::oni;
use usnm_ingest::release::Published;

use crate::Cli;

/// Copy batches from an Open ONI site, by default Penn State's Pennsylvania
/// Newspaper Archive (docs/design/04 §4.12), into a raw store: each batch's
/// BagIt bag (by default its page images, JP2, and its XML: ALTO and METS),
/// every file checked against the bag's MD5, and each page's OCR text as a
/// retained archive. Writes the batches' list for `enqueue --list`; with
/// USNM_RAW_URL at the same store, `curate` reads the archives from there.
/// Batches LoC lists (any version) are left out, and so are issues from
/// --issued-before on. Stops at the first batch that fails; running it
/// again resumes, and a batch fetched before is only listed.
#[derive(Args)]
pub(crate) struct OniFetchArgs {
    /// The site's batches, as its /batches/ page names them
    /// (`batch_pst_bonaDea_ver03`).
    #[arg(long, value_delimiter = ',', required = true)]
    batches: Vec<String>,
    /// The raw store to fetch into: a Blob container URL or a directory
    /// (default: USNM_RAW_URL).
    #[arg(long, env = "USNM_RAW_URL")]
    to: String,
    /// Where to write the batch list (JSON) for `enqueue --list`.
    #[arg(long)]
    list_out: PathBuf,
    /// The Open ONI site.
    #[arg(long, default_value = oni::PSU_SITE)]
    site: String,
    /// Milliseconds between requests to the site.
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(250..))]
    gap_ms: u64,
    /// The bag's files to copy, by extension: `jp2` (page images), `xml`
    /// (ALTO and METS), `pdf`, `tif`. Each issue's METS is copied anyway.
    #[arg(long = "ext", value_delimiter = ',', default_value = "jp2,xml")]
    exts: Vec<String>,
    /// Leave out issues from this day on, and their files.
    #[arg(long, default_value = "1931-01-01")]
    issued_before: NaiveDate,
    /// LoC's batch list, whose batches are left out (a file holding `[]`
    /// leaves out none).
    #[arg(long, default_value = usnm_ingest::source::LOC_DATASETS)]
    loc_list: String,
}

pub(crate) async fn oni_fetch_command(
    cli: &Cli,
    args: &OniFetchArgs,
) -> anyhow::Result<Option<Published>> {
    let loc: HashSet<String> = super::common::read_list(&args.loc_list, None)
        .await?
        .iter()
        .map(|b| oni::base_name(&b.name))
        .collect();
    let store = usnm_store::open(&args.to)?;
    let fetch = Http::new(
        Duration::from_millis(args.gap_ms),
        oni::USER_AGENT,
        "Open ONI",
    )?;
    let opts = oni::Options {
        site: args.site.trim_end_matches('/').to_owned(),
        exts: args.exts.clone(),
        issued_before: args.issued_before,
    };
    let tmp = cli.stores.work_dir.join("oni");
    let mut list = Vec::new();
    for batch in &args.batches {
        if loc.contains(&oni::base_name(batch)) {
            tracing::warn!(batch, "LoC lists this batch; left out");
            continue;
        }
        let (listed, fetched) =
            oni::fetch_batch(&fetch, store.as_ref(), &args.to, &opts, batch, &tmp)
                .await
                .with_context(|| format!("fetching {batch}"))?;
        tracing::info!(batch = %listed.name, ?fetched, "batch fetched");
        list.push(listed);
    }
    std::fs::write(&args.list_out, serde_json::to_vec_pretty(&list)?)
        .with_context(|| args.list_out.display().to_string())?;
    tracing::info!(batches = list.len(), list_out = %args.list_out.display(), "batch list written");
    Ok(None)
}
