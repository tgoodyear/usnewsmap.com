use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::{ensure, Context};
use clap::Args;
use tokio::process::Command;
use tokio::time::{Instant, MissedTickBehavior};
use usnm_ingest::release::Published;
use usnm_ingest::telemetry;

/// Supervise one on-prem Japanese OCR invocation, exporting its active heartbeat
/// and, on error, the normal ingest.command_failures metric. Child output stays
/// in the local journal, not Application Insights. Requires telemetry enabled.
#[derive(Args)]
pub(crate) struct JaOcrRunArgs {
    /// Trusted runner executable. The path must be absolute.
    #[arg(long)]
    executable: PathBuf,
    /// One runner argument; repeat --arg=VALUE, including for flags.
    #[arg(long = "arg", allow_hyphen_values = true)]
    args: Vec<OsString>,
}

pub(crate) async fn ja_ocr_run(args: &JaOcrRunArgs) -> anyhow::Result<Option<Published>> {
    ensure!(
        args.executable.is_absolute(),
        "Japanese OCR runner executable must be absolute"
    );
    let mut command = Command::new(&args.executable);
    command.args(&args.args);
    let status = supervise(command, Duration::from_secs(60), || {
        telemetry::metrics().ja_ocr_heartbeat.record(1, &[]);
    })
    .await?;
    ensure!(status.success(), "Japanese OCR runner ended with {status}");
    Ok(None)
}

async fn supervise(
    mut command: Command,
    period: Duration,
    mut heartbeat: impl FnMut(),
) -> anyhow::Result<ExitStatus> {
    let mut child = command
        .kill_on_drop(true)
        .spawn()
        .context("starting the Japanese OCR runner")?;
    heartbeat();
    let mut interval = tokio::time::interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            status = child.wait() => return status.context("waiting for the Japanese OCR runner"),
            _ = interval.tick() => heartbeat(),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use clap::Parser;

    #[tokio::test]
    async fn dispatch_does_not_require_pipeline_state_or_open_stores() {
        let cli = crate::Cli::try_parse_from([
            "usnm-ingest",
            "--curated",
            "/nonexistent/usnm-test-curated",
            "--reference",
            "/nonexistent/usnm-test-reference",
            "ja-ocr-run",
            "--executable",
            "/bin/sh",
            "--arg=-c",
            "--arg=exit 0",
        ])
        .unwrap();
        crate::run(&cli).await.unwrap();
    }

    #[tokio::test]
    async fn heartbeats_only_while_one_child_is_active() {
        let directory = tempfile::tempdir().unwrap();
        let gate = directory.path().join("finish");
        let starts = directory.path().join("starts");
        let mut command = Command::new("/bin/sh");
        command.args([
            OsString::from("-c"),
            OsString::from("echo started >> \"$2\"; while [ ! -f \"$1\" ]; do sleep 0.01; done"),
            OsString::from("ja-ocr-test"),
            gate.clone().into_os_string(),
            starts.clone().into_os_string(),
        ]);
        let mut beats = 0;
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            supervise(command, Duration::from_millis(10), || {
                beats += 1;
                if beats == 2 {
                    std::fs::write(&gate, b"finish").unwrap();
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(status.success());
        assert!(beats >= 2);
        assert_eq!(std::fs::read_to_string(starts).unwrap(), "started\n");
    }

    #[tokio::test]
    async fn failed_and_missing_runners_are_not_successes_or_retried() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 7"]);
        let status = supervise(command, Duration::from_secs(60), || {})
            .await
            .unwrap();
        assert_eq!(status.code(), Some(7));
        let mut beats = 0;
        let missing = Command::new("/nonexistent/usnm-ja-ocr-test");
        let error = supervise(missing, Duration::from_secs(60), || beats += 1)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("starting the Japanese OCR runner"));
        assert_eq!(beats, 0);
    }
}
