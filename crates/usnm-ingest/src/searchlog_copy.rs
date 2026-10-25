//! The search log's second copy: a daily, verified copy of `searches/days/` into a restricted
//! dataset on `tank` (#1097; ADR-0027 settled decision 3, ADR-0022 decision 5).
//!
//! The day files (`days/{day}.jsonl`, ADR-0012) cannot be rebuilt and have one copy, in the
//! lake. [`copy_days`] lists them with the host's Entra identity (Storage Blob Data Reader on
//! `searches`) and brings every one the destination directory lacks across, without ever
//! changing what is already there:
//!
//! - a missing day is streamed into `{day}.jsonl.partial` in the destination, its size checked
//!   against the listing, its sha256 taken from the stream and checked against the file as
//!   written (read back after `fsync`), and only then linked into place as `{day}.jsonl`. The
//!   link fails if the name exists, so a day file is never replaced, and the copy is read-only
//!   (`0440`);
//! - a day already there is left alone. Its size must equal the listing's; with `verify`, its
//!   sha256 must equal that of the day file read again from the lake. A copy that differs is
//!   reported and left exactly as it is: the operator moves it aside by hand and runs the copy
//!   again;
//! - the tool deletes nothing from the lake or from the copy. The one exception is its own
//!   `{day}.jsonl.partial`, left by a run that died: a partial is never resumed (Blob Storage
//!   is read whole), so the next run removes it and copies the day again;
//! - a day that is in the copy but no longer in the lake is reported (`local_only_days`) and
//!   kept.
//!
//! The comparison is therefore the listed size against the bytes received, and the sha256 of the
//! bytes received against the file on disk; `verify` adds the sha256 of the lake's bytes against
//! every copy. There is no stored hash to compare with instead: the listing gives a size, an
//! ETag and a creation time, and the API sends no Content-MD5 when it writes a day file.
//!
//! A day counts as *overdue* when it is still missing from the copy, or the copy differs, more
//! than `alert_after` after the day file was written (the lake's creation time; an hour after
//! the day's end when the store does not say, [`WRITTEN_AFTER_DAY_END`]). A run reports its
//! overdue days and the command then fails, so the unit shows as failed; the status file
//! ([`Report`]) carries the same for anything that watches the host.
//!
//! Search text is the one thing this must not leak. The day files are never parsed, only
//! counted and hashed, and nothing here logs or serializes a line of them: errors and the report
//! hold day names, sizes and counts. The destination must be the root of the restricted dataset
//! (checked: a directory that is not a mount point is refused), and every byte is written into
//! it, never to a temporary directory.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use chrono::{DateTime, Days, NaiveDate, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use usnm_store::{ObjectInfo, ObjectStore};

use crate::source::hex;

/// Where the day files are, in the `searches` container.
const DAYS: &str = "days";

/// How long after a day ends its file is written (`CLOSE_AFTER` in `usnm-api`'s `searchlog`).
/// Used as the day file's written time when the store does not give one.
pub const WRITTEN_AFTER_DAY_END: Duration = Duration::from_secs(3600);

/// A copy is read-only: the tool never opens one for writing again.
const FILE_MODE: u32 = 0o440;
/// The `days` directory the tool creates in a destination that has none.
const DIR_MODE: u32 = 0o750;
/// The status file is readable by whatever watches the host; it holds day names and counts.
const STATUS_MODE: u32 = 0o644;

const PARTIAL: &str = ".partial";
const CHUNK: usize = 1024 * 1024;

/// What a run is told.
#[derive(Debug, Clone)]
pub struct Options<'a> {
    /// The root of the restricted dataset: the copy is `{dest}/days/{day}.jsonl`.
    pub dest: &'a Path,
    /// Where to write the status file ([`Report`]), if anywhere.
    pub status: Option<&'a Path>,
    /// The time now, for the overdue test and the status file. Injected, so tests do not wait.
    pub now: DateTime<Utc>,
    /// How long a day may be missing from the copy, or differ, before it is overdue.
    pub alert_after: Duration,
    /// Also compare every copy's sha256 with the lake's bytes, not only its size.
    pub verify: bool,
    /// Refuse a destination that is not a mount point, so a dataset that is not mounted cannot
    /// fill the host's own disk with search text.
    pub require_mount: bool,
}

/// What a run found and did, as logged and as written to the status file. Day names, counts and
/// times only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub checked_at: DateTime<Utc>,
    /// The last run that left no day missing or different, this one included.
    pub last_success: Option<DateTime<Utc>>,
    /// No day is overdue.
    pub ok: bool,
    /// Day files in the lake.
    pub source_days: u64,
    /// Of those, in the copy with the same size (and, with `verify`, the same sha256).
    pub copied_days: u64,
    /// Copied by this run.
    pub copied_this_run: u64,
    pub bytes_copied: u64,
    pub newest_source_day: Option<NaiveDate>,
    pub newest_copied_day: Option<NaiveDate>,
    /// In the lake, not in the copy after this run: this run tried to copy them and could not.
    pub missing_days: Vec<NaiveDate>,
    /// In both, and the copy differs (left as it is).
    pub mismatched_days: Vec<NaiveDate>,
    /// Missing or different for longer than `alert_after`; the command fails on these.
    pub overdue_days: Vec<NaiveDate>,
    /// In the copy, not in the lake (kept).
    pub local_only_days: Vec<NaiveDate>,
    /// Objects under `days/` that are not `{day}.jsonl` (counted, never named).
    pub ignored_objects: u64,
    /// Files in the copy's `days/` that are neither a day file nor its partial (left alone).
    pub foreign_files: u64,
    /// Partials of an interrupted run removed at the start.
    pub stale_partials_removed: u64,
}

/// What an earlier status file says that this run keeps.
#[derive(Deserialize)]
struct Previous {
    last_success: Option<DateTime<Utc>>,
}

/// The day a path under `days/` names, if it is exactly `days/{YYYY-MM-DD}.jsonl`.
fn day_of_path(path: &str) -> Option<NaiveDate> {
    day_of_name(path.strip_prefix("days/")?)
}

/// The day a file name names, if it is exactly `{YYYY-MM-DD}.jsonl`.
fn day_of_name(name: &str) -> Option<NaiveDate> {
    let stem = name.strip_suffix(".jsonl")?;
    let day = NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()?;
    // Only the canonical spelling: not `2026-1-4`.
    (day.to_string() == stem).then_some(day)
}

fn name_of(day: NaiveDate) -> String {
    format!("{day}.jsonl")
}

/// The path of a day file in the lake.
pub fn source_path(day: NaiveDate) -> String {
    format!("{DAYS}/{}", name_of(day))
}

/// When the day file was written: the store's creation time, or an hour after the day ends.
fn written_at(info: &ObjectInfo, day: NaiveDate) -> DateTime<Utc> {
    match info.created {
        Some(t) => t.into(),
        None => {
            let end = day
                .checked_add_days(Days::new(1))
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map_or(DateTime::<Utc>::MIN_UTC, |t| t.and_utc());
            end + chrono::TimeDelta::from_std(WRITTEN_AFTER_DAY_END).unwrap_or_default()
        }
    }
}

/// Whether something that has been wrong since `since` is overdue at `now`.
fn is_overdue(now: DateTime<Utc>, since: DateTime<Utc>, after: Duration) -> bool {
    now.signed_duration_since(since)
        .to_std()
        .is_ok_and(|age| age > after)
}

/// Copy the lake's day files that the destination lacks (module docs). `Err` for what stops the
/// whole run: a destination that is not the dataset, another run holding it, the lake not
/// listing, the status file not written. A day that cannot be copied is in the report
/// (`missing_days`) and the run goes on to the others; the caller fails the command on
/// `overdue_days`.
pub async fn copy_days(source: &dyn ObjectStore, o: &Options<'_>) -> anyhow::Result<Report> {
    let (days_dir, _lock) = prepare(o)?;
    let listed = source
        .list_info(DAYS)
        .await
        .context("listing the search log's day files")?;
    let mut report = Report {
        checked_at: o.now,
        ..Report::default()
    };
    let mut in_lake: BTreeMap<NaiveDate, ObjectInfo> = BTreeMap::new();
    for info in listed {
        match day_of_path(&info.path) {
            Some(day) => {
                in_lake.insert(day, info);
            }
            None => report.ignored_objects += 1,
        }
    }
    let local = survey(&days_dir)?;
    report.stale_partials_removed = local.stale_partials;
    report.foreign_files = local.foreign;
    report.source_days = in_lake.len() as u64;
    report.newest_source_day = in_lake.keys().next_back().copied();
    report.local_only_days = local
        .days
        .iter()
        .filter(|d| !in_lake.contains_key(d))
        .copied()
        .collect();

    for (day, info) in &in_lake {
        let here = days_dir.join(name_of(*day));
        let held = if local.days.contains(day) {
            compare(source, info, &here, o.verify).await
        } else {
            Ok(Held::Absent)
        };
        let confirmed = match held {
            Ok(Held::Matches) => true,
            Ok(Held::Differs) => {
                tracing::warn!(%day, "the copy of a day file differs from the lake's; left as it is");
                report.mismatched_days.push(*day);
                false
            }
            Ok(Held::Absent) => match fetch(source, info, &days_dir, *day).await {
                Ok(bytes) => {
                    report.copied_this_run += 1;
                    report.bytes_copied += bytes;
                    tracing::info!(%day, bytes, "day file copied");
                    true
                }
                Err(e) => {
                    tracing::warn!(%day, error = %format!("{e:#}"), "day file not copied");
                    report.missing_days.push(*day);
                    false
                }
            },
            // The copy could not be read to compare it: not confirmed.
            Err(e) => {
                tracing::warn!(%day, error = %format!("{e:#}"), "the copy of a day file could not be checked");
                report.mismatched_days.push(*day);
                false
            }
        };
        if confirmed {
            report.copied_days += 1;
            report.newest_copied_day = Some(*day);
        } else if is_overdue(o.now, written_at(info, *day), o.alert_after) {
            report.overdue_days.push(*day);
        }
    }

    report.ok = report.overdue_days.is_empty();
    let caught_up = report.missing_days.is_empty() && report.mismatched_days.is_empty();
    report.last_success = if caught_up {
        Some(o.now)
    } else {
        o.status.and_then(previous_success)
    };
    if let Some(path) = o.status {
        write_status(path, &report)
            .with_context(|| format!("writing the status file {}", path.display()))?;
    }
    Ok(report)
}

/// Check the destination, make `days/` and take its lock. The lock is held (by the returned
/// file) until the run ends: a second run, a manual one beside the timer's, is refused.
fn prepare(o: &Options<'_>) -> anyhow::Result<(PathBuf, std::fs::File)> {
    let meta = std::fs::metadata(o.dest)
        .with_context(|| format!("the destination {} is not there", o.dest.display()))?;
    ensure!(
        meta.is_dir(),
        "the destination {} is not a directory",
        o.dest.display()
    );
    if o.require_mount {
        ensure!(
            is_mount_point(o.dest)?,
            "the destination {} is not a mount point: refusing to write search text to a directory \
             that is not the restricted dataset (--allow-unmounted for a test)",
            o.dest.display()
        );
    }
    let days = o.dest.join(DAYS);
    match std::fs::DirBuilder::new().mode(DIR_MODE).create(&days) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", days.display())),
    }
    let lock = std::fs::File::open(&days).with_context(|| format!("opening {}", days.display()))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
        |_| {
            anyhow::anyhow!(
                "another search log copy is running on {}: one at a time",
                days.display()
            )
        },
    )?;
    Ok((days, lock))
}

/// Whether `dir` is the root of a mounted file system: on another device than its parent. A
/// bind mount of a directory on the parent's own file system is not told apart.
fn is_mount_point(dir: &Path) -> std::io::Result<bool> {
    Ok(std::fs::metadata(dir)?.dev() != std::fs::metadata(dir.join(".."))?.dev())
}

/// What is in the copy's `days/`.
struct Local {
    days: BTreeSet<NaiveDate>,
    stale_partials: u64,
    foreign: u64,
}

/// List the copy's `days/`. Holding the lock, no partial is another run's: each is removed.
/// Anything else that is not a day file is counted and left.
fn survey(days_dir: &Path) -> anyhow::Result<Local> {
    let mut local = Local {
        days: BTreeSet::new(),
        stale_partials: 0,
        foreign: 0,
    };
    for entry in std::fs::read_dir(days_dir).with_context(|| days_dir.display().to_string())? {
        let entry = entry.with_context(|| days_dir.display().to_string())?;
        let name = entry.file_name();
        let name = name.to_str();
        if let Some(day) = name.and_then(day_of_name) {
            local.days.insert(day);
        } else if name
            .and_then(|n| n.strip_suffix(PARTIAL))
            .and_then(day_of_name)
            .is_some()
        {
            std::fs::remove_file(entry.path())
                .with_context(|| format!("removing a stale {PARTIAL} file"))?;
            local.stale_partials += 1;
        } else {
            local.foreign += 1;
        }
    }
    if local.stale_partials > 0 {
        tracing::info!(
            removed = local.stale_partials,
            "stale partial copies of an interrupted run removed"
        );
    }
    Ok(local)
}

/// How a day file already in the copy compares with the lake's.
enum Held {
    Absent,
    Matches,
    Differs,
}

/// Compare the copy `here` of a day file with its listing: a regular file of the listed size,
/// and with `verify` the lake's bytes hashed again against the file's.
async fn compare(
    source: &dyn ObjectStore,
    info: &ObjectInfo,
    here: &Path,
    verify: bool,
) -> anyhow::Result<Held> {
    let meta = std::fs::symlink_metadata(here)?;
    if !meta.is_file() || meta.len() != info.size {
        return Ok(Held::Differs);
    }
    if !verify {
        return Ok(Held::Matches);
    }
    let (size, local) = sha256_file(here).await?;
    let (remote_size, remote) = sha256_object(source, info).await?;
    Ok(if size == remote_size && local == remote {
        Held::Matches
    } else {
        Held::Differs
    })
}

/// Stream a day file from the lake into `{day}.jsonl.partial`, check it, and link it into place
/// as `{day}.jsonl` unless that name has appeared. Returns its size. A failure removes the
/// partial.
async fn fetch(
    source: &dyn ObjectStore,
    info: &ObjectInfo,
    days_dir: &Path,
    day: NaiveDate,
) -> anyhow::Result<u64> {
    let partial = days_dir.join(format!("{}{PARTIAL}", name_of(day)));
    let result = fetch_into(source, info, days_dir, day, &partial).await;
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

async fn fetch_into(
    source: &dyn ObjectStore,
    info: &ObjectInfo,
    days_dir: &Path,
    day: NaiveDate,
    partial: &Path,
) -> anyhow::Result<u64> {
    let mut stream = source
        .get_stream(&info.path)
        .await?
        .context("it was listed but is gone")?;
    // `create_new`: the lock makes any file here ours, but a name taken is never written into.
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(partial)
        .await
        .with_context(|| format!("creating {}", partial.display()))?;
    let (mut hash, mut n) = (Sha256::new(), 0u64);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        n += chunk.len() as u64;
        ensure!(
            n <= info.size,
            "the stream is longer than the {} bytes listed",
            info.size
        );
        hash.update(&chunk);
        file.write_all(&chunk).await?;
    }
    ensure!(n == info.size, "read {n} of the {} bytes listed", info.size);
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    let received = hex(&hash.finalize());
    let (on_disk_size, on_disk) = sha256_file(partial).await?;
    ensure!(
        on_disk_size == n && on_disk == received,
        "the file written ({on_disk_size} bytes, sha256 {on_disk}) is not the bytes received \
         ({n} bytes, sha256 {received})"
    );
    let here = days_dir.join(name_of(day));
    match std::fs::hard_link(partial, &here) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            bail!(
                "a file named {} appeared meanwhile; left as it is",
                name_of(day)
            )
        }
        Err(e) => return Err(e).with_context(|| format!("linking {}", here.display())),
    }
    std::fs::remove_file(partial)?;
    // The link and the removal are in the directory, which is synced so they survive a crash.
    std::fs::File::open(days_dir)?.sync_all()?;
    Ok(n)
}

/// The size and sha256 of a file, read in chunks.
async fn sha256_file(path: &Path) -> anyhow::Result<(u64, String)> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let (mut hash, mut n) = (Sha256::new(), 0u64);
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        hash.update(&buf[..read]);
        n += read as u64;
    }
    Ok((n, hex(&hash.finalize())))
}

/// The size and sha256 of an object in the lake, streamed.
async fn sha256_object(
    source: &dyn ObjectStore,
    info: &ObjectInfo,
) -> anyhow::Result<(u64, String)> {
    let mut stream = source
        .get_stream(&info.path)
        .await?
        .context("it was listed but is gone")?;
    let (mut hash, mut n) = (Sha256::new(), 0u64);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hash.update(&chunk);
        n += chunk.len() as u64;
    }
    Ok((n, hex(&hash.finalize())))
}

/// `last_success` of the status file an earlier run left, if it can be read.
fn previous_success(path: &Path) -> Option<DateTime<Utc>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Previous>(&bytes)
        .ok()?
        .last_success
}

/// Replace the status file whole: readers see the old one or the new one.
fn write_status(path: &Path, report: &Report) -> anyhow::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&serde_json::to_vec_pretty(report)?)?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(STATUS_MODE))?;
    tmp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
