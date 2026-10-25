//! `pictures`: Newspaper Navigator's picture boxes into the curated store (#1102, 04 §4.11).
use std::path::PathBuf;

use clap::Args;
use usnm_ingest::pictures::source::{Source, HUB};
use usnm_ingest::pictures::{self, Options};
use usnm_ingest::release::Published;

use crate::Cli;

/// Write Newspaper Navigator's picture boxes (#1102, 04 §4.11) to the curated
/// store: one row per detected photograph, illustration, map, comic or cartoon,
/// read from the five per-kind configs of `biglam/newspaper-navigator` (CC0) at
/// a pinned dataset revision, as
/// `pictures/newspaper-navigator/{revision}/{attempt}/{kind}-{nnnn}.parquet`
/// and a `provenance.json` written last, which marks the attempt finished. Rows
/// are keyed by the scan (LCCN, date, edition, batch, reel, frame), with the
/// crop URL rewritten to tile.loc.gov. Reads the files from `--from-dir`, or
/// downloads them from the Hub one at a time (about 1.4 GB) and deletes each
/// once read. A revision with a finished attempt is left as it is. Run it on an
/// on-prem node. Prints what it did as JSON; logs counts, never rows.
#[derive(Args)]
pub(crate) struct PicturesArgs {
    /// The dataset revision: the full 40-character commit SHA of
    /// biglam/newspaper-navigator, in lower case (a branch or tag does not pin
    /// the data).
    #[arg(long, value_parser = pictures::parse_revision)]
    revision: String,
    /// Read `<config>/*.parquet` from this directory (configs `cartoons`,
    /// `comics`, `illustrations`, `maps` and `photos`, as
    /// `huggingface-cli download` lays them out) instead of downloading.
    #[arg(long)]
    from_dir: Option<PathBuf>,
    /// The Hugging Face host to download from (another only for a copy of its
    /// responses).
    #[arg(long, default_value = HUB, conflicts_with = "from_dir")]
    hub_url: String,
    /// Stop, marking nothing finished, when more than this percent of a kind's
    /// rows have no scan identity or no crop URL: the shapes read here would
    /// not fit the data.
    #[arg(long, default_value_t = pictures::MAX_UNPLACED_PCT, value_parser = percent)]
    max_unplaced_pct: f64,
}

/// A number from 0 to 100.
fn percent(s: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|p| (0.0..=100.0).contains(p))
        .ok_or_else(|| "a percent from 0 to 100".to_owned())
}

pub(crate) async fn pictures_command(
    cli: &Cli,
    args: &PicturesArgs,
) -> anyhow::Result<Option<Published>> {
    let PicturesArgs {
        revision,
        from_dir,
        hub_url,
        max_unplaced_pct,
    } = args;
    let curated = usnm_store::open(&cli.stores.curated)?;
    let source = match from_dir {
        Some(dir) => Source::Dir(dir.clone()),
        None => Source::Hub {
            base: hub_url.clone(),
            scratch: cli.stores.work_dir.join("pictures"),
        },
    };
    let opts = Options {
        revision: revision.clone(),
        source,
        part_rows: pictures::PART_ROWS,
        max_unplaced_pct: *max_unplaced_pct,
        commit: std::env::var("USNM_GIT_SHA").ok().filter(|s| !s.is_empty()),
    };
    let report = pictures::run(curated.as_ref(), &opts, chrono::Utc::now()).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;

    const REVISION: &str = "e2e585824cdd28d52755c481dca69ed0bfa992a1";

    fn parse(extra: &[&str]) -> Result<Cli, clap::Error> {
        let mut args = vec![
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "pictures",
        ];
        args.extend_from_slice(extra);
        Cli::try_parse_from(args)
    }

    #[test]
    fn pictures_takes_named_arguments_and_a_pinned_revision() {
        let Command::Pictures(PicturesArgs {
            revision,
            from_dir,
            hub_url,
            max_unplaced_pct,
        }) = parse(&["--revision", REVISION]).unwrap().command
        else {
            panic!("not pictures");
        };
        assert_eq!(revision, REVISION);
        assert_eq!(from_dir, None);
        assert_eq!(hub_url, HUB);
        assert_eq!(max_unplaced_pct, pictures::MAX_UNPLACED_PCT);

        // The revision is required, and a full SHA in lower case.
        assert!(parse(&[]).is_err());
        assert!(parse(&["--revision", "main"]).is_err());
        assert!(parse(&["--revision", &REVISION[..39]]).is_err());
        assert!(parse(&["--revision", &REVISION.to_uppercase()]).is_err());
        assert!(parse(&[REVISION]).is_err(), "named arguments only");

        let Command::Pictures(PicturesArgs {
            from_dir,
            max_unplaced_pct,
            ..
        }) = parse(&[
            "--revision",
            REVISION,
            "--from-dir",
            "/var/lib/nn",
            "--max-unplaced-pct",
            "5",
        ])
        .unwrap()
        .command
        else {
            panic!("not pictures");
        };
        assert_eq!(from_dir, Some(PathBuf::from("/var/lib/nn")));
        assert_eq!(max_unplaced_pct, 5.0);
        assert!(parse(&["--revision", REVISION, "--max-unplaced-pct", "101"]).is_err());
        assert!(parse(&["--revision", REVISION, "--max-unplaced-pct", "x"]).is_err());
        // A directory and a Hub are two sources.
        assert!(parse(&["--revision", REVISION, "--from-dir", "d", "--hub-url", "u"]).is_err());
    }
}
