//! `congress-globe` and `congress-serialset`: the Congress fetches (`usnm_congress`, the
//! `usnm-congress` crate); and `congress-damage` (#922), the damage metric over what they wrote.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Args;
use usnm_ingest::release::Published;
use usnm_ingest::state::State;
use usnm_state::owner_id;

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
    #[arg(long, default_value = usnm_congress::globe::UNT)]
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
    #[arg(long, default_value = usnm_congress::serialset::GOVINFO)]
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

/// Curate Annals or Register PDFs an operator downloaded in a browser.
/// Reads only the matching manifest/PDFs from USNM_RAW_URL; it never
/// contacts Congress.gov. Each complete PDF becomes one curated unit.
#[derive(Args)]
pub(crate) struct CongressManualArgs {
    /// `annals` or `register`.
    #[arg(long, value_enum)]
    series: usnm_congress::manual::Series,
    /// Only these unit ids from the manifest, comma separated.
    #[arg(long, value_delimiter = ',')]
    units: Vec<String>,
    /// Reprocess units even when their manifest fingerprint is unchanged.
    #[arg(long)]
    refetch: bool,
    /// Stop after this many units.
    #[arg(long)]
    max_units: Option<usize>,
    /// Stop between volumes by this deadline; the manual job uses 22 h.
    #[arg(long, required = true)]
    max_runtime_secs: u64,
    /// Poppler and Tesseract executables.
    #[arg(long, env = "USNM_PDFTOTEXT", default_value = "pdftotext")]
    pdftotext: PathBuf,
    #[arg(long, env = "USNM_PDFINFO", default_value = "pdfinfo")]
    pdfinfo: PathBuf,
    #[arg(long, env = "USNM_PDFTOPPM", default_value = "pdftoppm")]
    pdftoppm: PathBuf,
    #[arg(long, env = "USNM_TESSERACT", default_value = "tesseract")]
    tesseract: PathBuf,
}

/// Score every page of the curated Congress pages with the OCR damage
/// metric (#922, 04 §4.10): per volume, a table of the page's damage
/// rate (the forced English score of pages of at least 200 characters),
/// function-word share, language, decision and garbage share, and a
/// summary written last that marks the volume done, under
/// `audit/damage/v<VERSION>/congress/<series>/` in `--out` (default: the
/// curated store). Reads each unit's newest finished attempt, less those
/// a later empty attempt supersedes, with every part checked against its
/// `counts.json`. Volumes with a summary are skipped, so a stopped run is
/// continued by the next; everything is written create-only. A run over
/// every volume that finishes them all also writes the roll-up
/// `corpus/<key>.json`, keyed by the attempts it covers. Run it once,
/// after the fetch has finished. Prints what it did as JSON; logs counts
/// and volume ids, never text.
#[derive(Args)]
pub(crate) struct CongressDamageArgs {
    /// The series to score.
    #[arg(
        long,
        default_value = usnm_congress::serialset::SERIES,
        value_parser = clap::builder::PossibleValuesParser::new(usnm_congress::SERIES.iter().copied())
    )]
    series: String,
    /// Only these volumes (`00033_00_00`), comma separated; with `--serials`,
    /// the volumes either names. The roll-up is written only by a run over
    /// every volume.
    #[arg(long, value_delimiter = ',')]
    volumes: Vec<String>,
    /// Only the volumes with these serial numbers (`33`), comma separated
    /// (Serial Set only).
    #[arg(long, value_delimiter = ',')]
    serials: Vec<u32>,
    /// Where the tables go: a directory or a Blob container URL. Default:
    /// the curated store.
    #[arg(long)]
    out: Option<String>,
    /// Seconds after the start to stop starting volumes; the next run
    /// continues.
    #[arg(long)]
    max_runtime_secs: Option<u64>,
    /// Volumes scored at once (default: the CPUs).
    #[arg(long)]
    parallel: Option<usize>,
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
    use usnm_congress::globe;
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
    .with_cutoff(run_deadline.map(|d| d + usnm_congress::fetch::REQUEST_GRACE));
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
    use usnm_congress::{fetch::or_stopped, serialset};
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
    let fetch = usnm_congress::fetch::Http::new(
        std::time::Duration::from_secs(*delay_secs),
        serialset::USER_AGENT,
        "GovInfo",
    )?
    .with_cutoff(run_deadline.map(|d| d + usnm_congress::fetch::REQUEST_GRACE));
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

pub(crate) async fn congress_manual_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    args: &CongressManualArgs,
) -> anyhow::Result<Option<Published>> {
    use usnm_congress::manual;
    let CongressManualArgs {
        series,
        units,
        refetch,
        max_units,
        max_runtime_secs,
        pdftotext,
        pdfinfo,
        pdftoppm,
        tesseract,
    } = args;
    let raw_url = cli
        .stores
        .raw
        .as_deref()
        .context("congress-manual requires USNM_RAW_URL with the manually downloaded PDFs")?;
    let raw = usnm_store::open(raw_url)?;
    let curated = usnm_store::open(&cli.stores.curated)?;
    let scratch = cli.stores.work_dir.join("congress-lcg");
    std::fs::create_dir_all(&scratch).with_context(|| format!("creating {}", scratch.display()))?;
    let pdf = manual::TesseractPdf::new(
        pdftotext.clone(),
        pdfinfo.clone(),
        pdftoppm.clone(),
        tesseract.clone(),
    )
    .await?;
    let opts = manual::Options {
        series: *series,
        units: units.clone(),
        refetch: *refetch,
        max_units: *max_units,
        deadline: deadline(start, Some(*max_runtime_secs)),
        work_dir: scratch,
    };
    let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let report = manual::locked(
        &state,
        &owner_id(),
        manual::run(raw.as_ref(), curated.as_ref(), &pdf, &opts, lost.as_ref()),
        lost.clone(),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(None)
}

pub(crate) async fn congress_damage_command(
    cli: &Cli,
    start: tokio::time::Instant,
    args: &CongressDamageArgs,
) -> anyhow::Result<Option<Published>> {
    let CongressDamageArgs {
        series,
        volumes,
        serials,
        out,
        max_runtime_secs,
        parallel,
    } = args;
    let opts = usnm_ingest::congress_damage::Options {
        series: series.clone(),
        volumes: volumes.iter().map(|v| v.trim().to_owned()).collect(),
        serials: serials.clone(),
        deadline: deadline(start, *max_runtime_secs),
        parallel: parallel.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
        }),
    };
    let curated = usnm_store::open(&cli.stores.curated)?;
    let out = match out {
        Some(url) => usnm_store::open(url)?,
        None => Arc::clone(&curated),
    };
    let report = usnm_ingest::congress_damage::run(curated, out, &opts).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
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
        assert_eq!(unt_url, usnm_congress::globe::UNT);
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
        assert_eq!(govinfo_url, usnm_congress::serialset::GOVINFO);
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

    #[test]
    fn congress_damage_takes_named_arguments() {
        let parse = |extra: &[&str]| {
            let mut args = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "congress-damage",
            ];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        let Command::CongressDamage(CongressDamageArgs {
            series,
            volumes,
            serials,
            out,
            ..
        }) = parse(&[]).unwrap().command
        else {
            panic!("not congress-damage");
        };
        assert_eq!(series, "serialset");
        assert!(volumes.is_empty() && serials.is_empty() && out.is_none());
        let Command::CongressDamage(CongressDamageArgs {
            series,
            volumes,
            serials,
            out,
            max_runtime_secs,
            parallel,
        }) = parse(&[
            "--series",
            "globe",
            "--volumes",
            "metadc29274,metadc29275",
            "--serials",
            "33,34",
            "--out",
            "/tmp/damage",
            "--max-runtime-secs",
            "60",
            "--parallel",
            "2",
        ])
        .unwrap()
        .command
        else {
            panic!("not congress-damage");
        };
        assert_eq!(series, "globe");
        assert_eq!(volumes, ["metadc29274", "metadc29275"]);
        assert_eq!(serials, [33, 34]);
        assert_eq!(out.as_deref(), Some("/tmp/damage"));
        assert_eq!((max_runtime_secs, parallel), (Some(60), Some(2)));
        assert!(parse(&["33"]).is_err(), "named arguments only");
        assert!(parse(&["--series", "newspapers"]).is_err());
        assert!(parse(&["--serials", "x"]).is_err());
    }

    #[test]
    fn congress_manual_requires_series_and_a_deadline() {
        let parse = |extra: &[&str]| {
            let mut args = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                "r",
                "congress-manual",
            ];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        assert!(parse(&[]).is_err());
        assert!(parse(&["--series", "annals"]).is_err());
        let Command::CongressManual(CongressManualArgs {
            series,
            units,
            refetch,
            max_units,
            max_runtime_secs,
            pdftotext,
            ..
        }) = parse(&[
            "--series",
            "register",
            "--units",
            "register-c18-s01-v01-b01,register-c18-s01-v02-b01",
            "--refetch",
            "--max-units",
            "2",
            "--max-runtime-secs",
            "79200",
        ])
        .unwrap()
        .command
        else {
            panic!("not congress-manual");
        };
        assert_eq!(series, usnm_congress::manual::Series::Register);
        assert_eq!(
            units,
            ["register-c18-s01-v01-b01", "register-c18-s01-v02-b01"]
        );
        assert!(refetch);
        assert_eq!(max_units, Some(2));
        assert_eq!(max_runtime_secs, 79_200);
        assert_eq!(pdftotext, PathBuf::from("pdftotext"));
        assert!(parse(&["--series", "globe", "--max-runtime-secs", "10"]).is_err());
    }
}
