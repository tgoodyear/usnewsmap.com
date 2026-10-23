//! `congress-globe` and `congress-serialset`: the Congress fetches (`usnm_ingest::congress`, the
//! `usnm-congress` crate).
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Args;
use usnm_ingest::owner_id;
use usnm_ingest::release::Published;
use usnm_ingest::state::State;

use super::common::deadline;
use crate::Cli;

/// Fetch the Congressional Globe from UNT's page OCR into curated
/// Congress pages (#229, 04 §4.10): list the collection over OAI-PMH,
/// leave out the items another holds, then fetch each item's pages at
/// UNT's crawl delay and write a curated attempt per item. Items with a
/// finished attempt are skipped. With USNM_RAW_URL every response is
/// kept there and read back on a later run. Holds the
/// `congress-globe` lock. Prints what it did as JSON.
#[derive(Args)]
pub(crate) struct CongressGlobeArgs {
    /// Only these items (`metadc29274`), comma separated.
    #[arg(long, value_delimiter = ',')]
    items: Vec<String>,
    /// Fetch items that already have a finished attempt again.
    #[arg(long)]
    refetch: bool,
    /// Stop after this many items.
    #[arg(long)]
    max_items: Option<usize>,
    /// Seconds after the start to stop: an item stopped between pages
    /// writes nothing, and the next run resumes it (from the raw store's
    /// copies, when there is one). Keep it under the job's replica timeout.
    #[arg(long)]
    max_runtime_secs: Option<u64>,
    /// Seconds between requests; UNT's robots.txt asks for 3, the least
    /// this takes.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u64).range(3..=60))]
    crawl_delay_secs: u64,
    /// Where to fetch from (UNT; another host only for a copy of its responses).
    #[arg(long, default_value = usnm_ingest::congress::globe::UNT)]
    unt_url: String,
}

/// Fetch the Congressional Serial Set from GovInfo into curated Congress
/// pages (#229, 04 §4.10): read the yearly sitemaps, then fetch each
/// volume's documents (MODS and PDF, about one request a second), read
/// the PDFs page by page with pdftotext and delete them, and write a
/// curated attempt per volume. Volumes with a finished attempt are
/// skipped unless their packages changed since (ids and sitemap
/// `lastmod`s). With
/// USNM_RAW_URL the MODS and page texts are kept there and read back on
/// a later run. Holds the `congress-serialset` lock. Prints what it did
/// as JSON.
#[derive(Args)]
pub(crate) struct CongressSerialsetArgs {
    /// Only the volumes with a package in these years' sitemaps
    /// (`1820`), comma separated; default all. Every sitemap is still
    /// read, and each volume is made of its packages in all of them.
    #[arg(long, value_delimiter = ',')]
    years: Vec<u16>,
    /// Only these serial numbers' volumes (`33`), comma separated.
    #[arg(long, value_delimiter = ',')]
    serials: Vec<u32>,
    /// Fetch volumes that already have a finished attempt again.
    #[arg(long)]
    refetch: bool,
    /// Stop after this many volumes.
    #[arg(long)]
    max_volumes: Option<usize>,
    /// Seconds after the start to stop: a volume stopped between
    /// documents writes nothing, and the next run resumes it (from the
    /// raw store's copies, when there is one). Keep it under the job's
    /// replica timeout.
    #[arg(long)]
    max_runtime_secs: Option<u64>,
    /// Seconds between requests; at least 1.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..=60))]
    delay_secs: u64,
    /// Where to fetch from (GovInfo; another host only for a copy of its responses).
    #[arg(long, default_value = usnm_ingest::congress::serialset::GOVINFO)]
    govinfo_url: String,
    /// The pdftotext binary (poppler-utils).
    #[arg(long, env = "USNM_PDFTOTEXT", default_value = "pdftotext")]
    pdftotext: PathBuf,
    /// Write this file (a small JSON record) when the run finishes the
    /// whole fetch: every volume written or left out for good, none
    /// left or incomplete. The on-prem unit doesn't start while it
    /// exists. Refused with `--years` or `--serials`, which fetch only
    /// part.
    #[arg(long, conflicts_with_all = ["years", "serials"])]
    done_file: Option<PathBuf>,
}

/// SIGTERM, as a stream (`systemctl stop`).
#[cfg(unix)]
fn sigterm() -> anyhow::Result<Sigterm> {
    Ok(Sigterm(tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    )?))
}

#[cfg(unix)]
struct Sigterm(tokio::signal::unix::Signal);

#[cfg(unix)]
impl Sigterm {
    async fn recv(&mut self) {
        self.0.recv().await;
    }
}

/// Elsewhere SIGTERM isn't caught.
#[cfg(not(unix))]
fn sigterm() -> anyhow::Result<Sigterm> {
    Ok(Sigterm)
}

#[cfg(not(unix))]
struct Sigterm;

#[cfg(not(unix))]
impl Sigterm {
    async fn recv(&mut self) {
        std::future::pending().await
    }
}

pub(crate) async fn congress_globe_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    args: &CongressGlobeArgs,
) -> anyhow::Result<Option<Published>> {
    let CongressGlobeArgs {
        items,
        refetch,
        max_items,
        max_runtime_secs,
        crawl_delay_secs,
        unt_url,
    } = args;
    use usnm_ingest::congress::globe;
    let curated = usnm_store::open(&cli.stores.curated)?;
    let raw = cli
        .stores
        .raw
        .as_deref()
        .map(usnm_store::open)
        .transpose()?;
    if raw.is_none() {
        tracing::warn!(
            "no raw store (USNM_RAW_URL): UNT's responses aren't kept, and an item \
             stopped by the deadline is fetched again from its first page"
        );
    }
    let run_deadline = deadline(start, *max_runtime_secs);
    // 120 s a try, and every request ends within the grace after the
    // deadline, short of the job's hard timeout.
    let fetch = globe::Http::new(
        std::time::Duration::from_secs(*crawl_delay_secs),
        globe::USER_AGENT,
        "UNT",
    )?
    .with_cutoff(run_deadline.map(|d| d + usnm_ingest::congress::fetch::REQUEST_GRACE));
    let opts = globe::Options {
        base: unt_url.trim_end_matches('/').to_owned(),
        items: items.clone(),
        refetch: *refetch,
        deadline: run_deadline,
        max_items: *max_items,
        ..globe::Options::default()
    };
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let report = globe::locked(
        &state,
        &owner_id(),
        globe::run(
            &fetch,
            curated.as_ref(),
            raw.as_deref(),
            &opts,
            lost.as_ref(),
        ),
        lost.clone(),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(None)
}

pub(crate) async fn congress_serialset_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    args: &CongressSerialsetArgs,
) -> anyhow::Result<Option<Published>> {
    let CongressSerialsetArgs {
        years,
        serials,
        refetch,
        max_volumes,
        max_runtime_secs,
        delay_secs,
        govinfo_url,
        pdftotext,
        done_file,
    } = args;
    use usnm_ingest::congress::{fetch::or_stopped, serialset};
    use usnm_ingest::local_holder::LocalHolder;
    let curated = usnm_store::open(&cli.stores.curated)?;
    let raw = cli
        .stores
        .raw
        .as_deref()
        .map(usnm_store::open)
        .transpose()?;
    if raw.is_none() {
        tracing::warn!(
            "no raw store (USNM_RAW_URL): GovInfo's MODS and page texts aren't kept, \
             and a volume stopped by the deadline is fetched again from its first PDF"
        );
    }
    // PDFs are streamed here and deleted once read.
    let scratch = cli.stores.work_dir.join("serialset");
    std::fs::create_dir_all(&scratch).with_context(|| format!("creating {}", scratch.display()))?;
    let run_deadline = deadline(start, *max_runtime_secs);
    // Sitemaps and MODS get 120 s a try, a PDF 30 minutes; every
    // request ends within the grace after the deadline.
    let fetch = usnm_ingest::congress::fetch::Http::new(
        std::time::Duration::from_secs(*delay_secs),
        serialset::USER_AGENT,
        "GovInfo",
    )?
    .with_cutoff(run_deadline.map(|d| d + usnm_ingest::congress::fetch::REQUEST_GRACE));
    let pdf = serialset::Pdftotext::new(pdftotext.clone());
    let opts = serialset::Options {
        base: govinfo_url.trim_end_matches('/').to_owned(),
        years: years.clone(),
        serials: serials.clone(),
        refetch: *refetch,
        deadline: run_deadline,
        max_volumes: *max_volumes,
        scratch,
    };
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // With a local state file (the on-prem runner) the lock's holder
    // names this host, boot and process, so a later run here takes
    // over the lock of one that is gone without waiting out its
    // lease, and SIGTERM releases it. A Cosmos lock (the Azure job)
    // is held as before.
    let local = match &cli.stores.cosmos {
        None => LocalHolder::current(),
        Some(_) => None,
    };
    let owner = local.as_ref().map_or_else(owner_id, LocalHolder::id);
    let gone = |holder: &str| {
        local
            .as_ref()
            .is_some_and(|me| me.is_gone(holder, LocalHolder::running))
    };
    // Registered before the lock is taken, so a SIGTERM from then on
    // is caught.
    let mut term = match local {
        Some(_) => Some(sigterm()?),
        None => None,
    };
    let stop = async {
        match term.as_mut() {
            Some(t) => t.recv().await,
            None => std::future::pending().await,
        }
    };
    let body = serialset::run(
        &fetch,
        &pdf,
        curated.as_ref(),
        raw.as_deref(),
        &opts,
        lost.as_ref(),
    );
    let report =
        serialset::locked(&state, &owner, &gone, or_stopped(body, stop), lost.clone()).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if let Some(path) = done_file {
        if report.finished() {
            serialset::write_done(path, &report)?;
            tracing::info!(marker = %path.display(), "the Serial Set fetch is finished; marker written");
        } else {
            tracing::warn!(
                left = report.left,
                incomplete = report.incomplete.len(),
                "the Serial Set fetch isn't finished; no marker, and the next start fetches what's left"
            );
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;

    #[test]
    fn congress_globe_keeps_unt_s_crawl_delay() {
        let parse = |extra: &[&str]| {
            let mut args = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "congress-globe",
            ];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        let Command::CongressGlobe(CongressGlobeArgs {
            items,
            crawl_delay_secs,
            unt_url,
            refetch,
            ..
        }) = parse(&["--items", "metadc29274,metadc29275"])
            .unwrap()
            .command
        else {
            panic!("not congress-globe");
        };
        assert_eq!(items, ["metadc29274", "metadc29275"]);
        assert_eq!(crawl_delay_secs, 3);
        assert_eq!(unt_url, usnm_ingest::congress::globe::UNT);
        assert!(!refetch);
        assert!(parse(&["--crawl-delay-secs", "2"]).is_err());
        assert!(parse(&["--crawl-delay-secs", "5"]).is_ok());
    }

    #[test]
    fn congress_serialset_paces_at_a_second_at_least() {
        let parse = |extra: &[&str]| {
            let mut args = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "congress-serialset",
            ];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        let Command::CongressSerialset(CongressSerialsetArgs {
            years,
            serials,
            delay_secs,
            govinfo_url,
            pdftotext,
            refetch,
            ..
        }) = parse(&["--years", "1820,1990", "--serials", "33"])
            .unwrap()
            .command
        else {
            panic!("not congress-serialset");
        };
        assert_eq!(years, [1820, 1990]);
        assert_eq!(serials, [33]);
        assert_eq!(delay_secs, 1);
        assert_eq!(govinfo_url, usnm_ingest::congress::serialset::GOVINFO);
        assert_eq!(pdftotext, PathBuf::from("pdftotext"));
        assert!(!refetch);
        assert!(parse(&["--delay-secs", "0"]).is_err());
        assert!(parse(&["--delay-secs", "2"]).is_ok());
        assert!(parse(&["1820"]).is_err(), "named arguments only");
        // The marker is for a run over every volume.
        let Command::CongressSerialset(CongressSerialsetArgs { done_file, .. }) =
            parse(&["--done-file", "/var/lib/usnm-sset/done"])
                .unwrap()
                .command
        else {
            panic!("not congress-serialset");
        };
        assert_eq!(done_file, Some(PathBuf::from("/var/lib/usnm-sset/done")));
        assert!(parse(&["--done-file", "d", "--years", "1820"]).is_err());
        assert!(parse(&["--done-file", "d", "--serials", "33"]).is_err());
    }
}
