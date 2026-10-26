//! `searchlog-copy`: the search log's day files, copied to a restricted dataset on `tank`
//! (#1097; scripts/onprem/searchlog-copy/README.md).
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use usnm_ingest::release::Published;
use usnm_ingest::searchlog_copy;

/// Copy the search log's day files (`searches/days/{day}.jsonl`) that are not in the copy yet
/// into a restricted dataset on this host (ADR-0027, settled decision 3), as this host's Entra
/// identity (Storage Blob Data Reader on `searches`). Each is streamed into a `.partial` file,
/// its size and sha256 checked, and linked into place read-only; a day already there is never
/// replaced or deleted, and one that differs is reported and left. Fails if a day is still
/// missing or different more than `--alert-after-hours` after it was written. Logs and
/// reports day names and counts, never a search.
#[derive(Args)]
pub(crate) struct SearchlogCopyArgs {
    /// The search log: the `searches` container URL
    /// (`https://<account>.blob.core.windows.net/searches`) or a directory.
    #[arg(long, env = "USNM_SEARCH_LOG_URL")]
    from: String,
    /// The root of the restricted dataset, which must be a mount point; the copy is
    /// `<to>/days/{day}.jsonl`.
    #[arg(long, env = "USNM_SEARCH_LOG_COPY_DIR")]
    to: PathBuf,
    /// Write the status JSON (day names, counts, the last success) here, replacing the previous
    /// one. Keep it outside the dataset, where a watcher can read it.
    #[arg(long, env = "USNM_SEARCH_LOG_COPY_STATUS")]
    status: Option<PathBuf>,
    /// Hours a day file may be missing from the copy, or differ, before the command fails.
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u64).range(1..=24 * 365))]
    alert_after_hours: u64,
    /// Also read every day file again from the search log and compare its sha256 with the copy's
    /// (the default checks the size). Reads the whole log.
    #[arg(long)]
    verify: bool,
    /// Accept a destination that is not a mount point: for a test or a trial run, never for the
    /// real copy.
    #[arg(long)]
    allow_unmounted: bool,
}

pub(crate) async fn searchlog_copy_command(
    args: &SearchlogCopyArgs,
) -> anyhow::Result<Option<Published>> {
    let SearchlogCopyArgs {
        from,
        to,
        status,
        alert_after_hours,
        verify,
        allow_unmounted,
    } = args;
    let report = searchlog_copy::copy_days(
        usnm_store::open(from)?.as_ref(),
        &searchlog_copy::Options {
            dest: to,
            status: status.as_deref(),
            now: chrono::Utc::now(),
            alert_after: Duration::from_secs(alert_after_hours * 3600),
            verify: *verify,
            require_mount: !allow_unmounted,
        },
    )
    .await?;
    tracing::info!(
        source_days = report.source_days,
        copied_days = report.copied_days,
        copied_this_run = report.copied_this_run,
        bytes_copied = report.bytes_copied,
        missing = report.missing_days.len(),
        mismatched = report.mismatched_days.len(),
        overdue = report.overdue_days.len(),
        local_only = report.local_only_days.len(),
        stale_partials_removed = report.stale_partials_removed,
        "search log copy checked"
    );
    anyhow::ensure!(
        report.overdue_days.is_empty(),
        "{} day file(s) missing from the copy, or different, for more than {alert_after_hours} hours: {}",
        report.overdue_days.len(),
        report
            .overdue_days
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Command};
    use clap::Parser;

    /// Parse `usnm-ingest ... searchlog-copy <extra>`. The top-level options every command
    /// takes are placeholders, as in the unit.
    fn parse(extra: &[&str]) -> Result<Cli, clap::Error> {
        let mut args = vec![
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "--state-file",
            "s",
            "searchlog-copy",
        ];
        args.extend_from_slice(extra);
        Cli::try_parse_from(args)
    }

    async fn run(extra: &[&str]) -> anyhow::Result<Option<Published>> {
        let cli = parse(extra).unwrap();
        let Command::SearchlogCopy(args) = &cli.command else {
            panic!("not searchlog-copy");
        };
        searchlog_copy_command(args).await
    }

    /// A lake directory with `days/{day}.jsonl` for each day, written long ago, and an empty
    /// destination.
    fn dirs(days: &[&str]) -> (tempfile::TempDir, tempfile::TempDir) {
        let (lake, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir(lake.path().join("days")).unwrap();
        for d in days {
            let path = lake.path().join(format!("days/{d}.jsonl"));
            std::fs::write(path, format!("{{\"q\":\"SENTINEL {d}\"}}\n")).unwrap();
        }
        (lake, dest)
    }

    #[tokio::test]
    async fn copies_and_writes_the_status() {
        let (lake, dest) = dirs(&["2026-10-20", "2026-10-21"]);
        let out = tempfile::tempdir().unwrap();
        let status = out.path().join("status.json");
        let (from, to, st) = (
            lake.path().to_str().unwrap(),
            dest.path().to_str().unwrap(),
            status.to_str().unwrap(),
        );

        // The lake's files were written just now, so nothing is overdue yet.
        run(&[
            "--from",
            from,
            "--to",
            to,
            "--status",
            st,
            "--allow-unmounted",
        ])
        .await
        .unwrap();

        assert!(dest.path().join("days/2026-10-20.jsonl").is_file());
        assert!(dest.path().join("days/2026-10-21.jsonl").is_file());
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(json["copied_days"], 2);
        assert_eq!(json["ok"], true);
        assert!(!json.to_string().contains("SENTINEL"));
    }

    #[tokio::test]
    async fn fails_naming_the_days_that_are_overdue() {
        let (lake, dest) = dirs(&["2026-10-20"]);
        let (from, to) = (lake.path().to_str().unwrap(), dest.path().to_str().unwrap());
        // A differing copy of a day written just now is not overdue ...
        std::fs::create_dir(dest.path().join("days")).unwrap();
        std::fs::write(dest.path().join("days/2026-10-20.jsonl"), "other\n").unwrap();
        let args = ["--from", from, "--to", to, "--allow-unmounted"];
        run(&args).await.unwrap();

        // ... and is once the limit is shorter than the day file's age. Set it back a day.
        let old = std::time::SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
        std::fs::File::options()
            .write(true)
            .open(lake.path().join("days/2026-10-20.jsonl"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let err = run(&args).await.unwrap_err();

        let text = format!("{err:#}");
        assert!(
            text.contains("1 day file(s)") && text.contains("2026-10-20"),
            "{text}"
        );
        assert!(!text.contains("SENTINEL"));
        assert_eq!(
            std::fs::read_to_string(dest.path().join("days/2026-10-20.jsonl")).unwrap(),
            "other\n",
            "the copy is left as it was"
        );
    }

    #[tokio::test]
    async fn refuses_a_destination_that_is_not_a_mount_point() {
        let (lake, dest) = dirs(&["2026-10-20"]);
        let err = run(&[
            "--from",
            lake.path().to_str().unwrap(),
            "--to",
            dest.path().to_str().unwrap(),
        ])
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("not a mount point"), "{err:#}");
        assert_eq!(std::fs::read_dir(dest.path()).unwrap().count(), 0);
    }

    #[test]
    fn takes_named_arguments_only() {
        assert!(parse(&["--to", "/d"]).is_err(), "--from is required");
        assert!(parse(&["--from", "/l"]).is_err(), "--to is required");
        assert!(parse(&["/l", "/d"]).is_err(), "no positional arguments");
        assert!(parse(&["--from", "/l", "--to", "/d", "extra"]).is_err());
        assert!(parse(&["--from", "/l", "--to", "/d", "--alert-after-hours", "0"]).is_err());
        assert!(parse(&["--from", "/l", "--to", "/d", "--alert-after-hours", "day"]).is_err());
        let cli = parse(&["--from", "/l", "--to", "/d"]).unwrap();
        let Command::SearchlogCopy(a) = &cli.command else {
            panic!("not searchlog-copy");
        };
        assert_eq!(a.alert_after_hours, 24);
        assert!(!a.verify && !a.allow_unmounted && a.status.is_none());
    }
}
