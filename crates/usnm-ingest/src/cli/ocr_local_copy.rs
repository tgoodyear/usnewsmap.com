//! `ocr-local-copy`: our OCR's output, copied to a dataset on `tank`
//! (scripts/onprem/ocr-local-copy/README.md).
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use usnm_ingest::ocr_local_copy;
use usnm_ingest::release::Published;

/// Copy `ocr-local/` from the lake's `curated` container into a dataset on this host, as this
/// host's Entra identity. Each object the dataset lacks is streamed into a `.partial` file, its
/// size and Content-MD5 checked, and linked into place read-only. Nothing is replaced or deleted:
/// a copy that differs is reported and left, and hashed again by every run until it matches. Fails
/// if an object is still missing, different or unverified more than `--alert-after-hours` after it
/// was written.
#[derive(Args)]
pub(crate) struct OcrLocalCopyArgs {
    /// The `curated` container URL (`https://<account>.blob.core.windows.net/curated`) or a
    /// directory.
    #[arg(long, env = "USNM_OCR_LOCAL_COPY_FROM")]
    from: String,
    /// The root of the dataset, which must be a mount point; the copy is
    /// `<to>/alto/...`, `<to>/pages/...` and so on.
    #[arg(long, env = "USNM_OCR_LOCAL_COPY_DIR")]
    to: PathBuf,
    /// Write the status JSON (counts, the last success, the copies that differ) here, replacing
    /// the previous one. The next run reads it back.
    #[arg(long, env = "USNM_OCR_LOCAL_COPY_STATUS")]
    status: Option<PathBuf>,
    /// Hours an object may be missing from the copy, differ or stay unverified before the command
    /// fails.
    #[arg(long, default_value_t = 48, value_parser = clap::value_parser!(u64).range(1..=24 * 365))]
    alert_after_hours: u64,
    /// Read every copy and compare its MD5 with the listing's (the default checks the size), and
    /// copy nothing. Reads only the copy, so it checks a seeded dataset without downloading it. An
    /// object listed with no MD5 is reported as unverified, and an absent one as missing.
    #[arg(long)]
    verify: bool,
    /// Accept a destination that is not a mount point: for a test or a trial run, never for the
    /// real copy.
    #[arg(long)]
    allow_unmounted: bool,
}

pub(crate) async fn ocr_local_copy_command(
    args: &OcrLocalCopyArgs,
) -> anyhow::Result<Option<Published>> {
    let OcrLocalCopyArgs {
        from,
        to,
        status,
        alert_after_hours,
        verify,
        allow_unmounted,
    } = args;
    let report = ocr_local_copy::copy_objects(
        usnm_store::open(from)?.as_ref(),
        &ocr_local_copy::Options {
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
        source_objects = report.source_objects,
        copied_objects = report.copied_objects,
        copied_this_run = report.copied_this_run,
        bytes_copied = report.bytes_copied,
        missing = report.missing_objects,
        mismatched = report.mismatched_objects,
        unverified = report.unverified_objects,
        overdue = report.overdue_objects,
        "OCR output copy checked"
    );
    anyhow::ensure!(
        report.overdue_objects == 0,
        "{} object(s) missing from the copy, different or unverified for more than \
         {alert_after_hours} hours; the first: {}",
        report.overdue_objects,
        report.overdue_sample.join(", ")
    );
    Ok(None)
}
