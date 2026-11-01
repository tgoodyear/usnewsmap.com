//! A second copy of our OCR's output: a daily, verified copy of `ocr-local/` in the lake's
//! `curated` container into a dataset on `tank`.
//!
//! The volunteers' and nodes' output costs real work to make again, and the lake's copy is one
//! Standard_LRS account. The model is [`crate::searchlog_copy`], with what differs here:
//!
//! - the objects are under nested paths, and there are tens of thousands of them, so the run
//!   stats the objects it lists and never walks the dataset;
//! - the listing's `Content-MD5` is the check on the bytes. Every object here has one, so a copy
//!   is compared with the lake's own hash and `verify` reads only the local side: it reports what
//!   is absent and copies nothing, so a seeded dataset is checked without downloading it;
//! - a copy that failed that check stays reported: the status file names every mismatch, and each
//!   later run hashes those copies again until they match, were moved aside or left the lake;
//! - `requests/` is left out: it's rewritten in place, and it can be made again from the
//!   `ocr-requests` container.
//!
//! Nothing here replaces or deletes a copy. An object is streamed into `{name}.partial`, checked
//! by size and MD5 as it arrives and again as read back after `fsync`, and linked into place
//! read-only. A copy that differs is reported and left for an operator. The one exception is the
//! tool's own stale `.partial`.
//!
//! No symbolic link is followed under the destination: each directory is opened from its parent
//! without following one, and files are reached from that handle by bare name.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use base64::Engine;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use md5::{Digest, Md5};
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use usnm_store::{is_safe_segment, ObjectInfo, ObjectStore};

use crate::searchlog_copy::{is_mount_point, write_status_file};

/// The prefix in `curated`.
const PREFIX: &str = "ocr-local";
/// Rewritten in place, and derived from the `ocr-requests` container.
const SKIPPED: &str = "requests/";

const FILE_MODE: Mode = Mode::RUSR.union(Mode::RGRP);
const DIR_MODE: Mode = Mode::RWXU.union(Mode::RGRP).union(Mode::XGRP);
const PARTIAL: &str = ".partial";
const CHUNK: usize = 1024 * 1024;
/// How many paths of overdue objects the status file names.
const SAMPLE: usize = 20;

/// What a run is told.
#[derive(Debug, Clone)]
pub struct Options<'a> {
    /// The root of the dataset: `ocr-local/alto/x.xml` is copied to `{dest}/alto/x.xml`.
    pub dest: &'a Path,
    /// Where to write the status file ([`Report`]), if anywhere.
    pub status: Option<&'a Path>,
    /// The time now, for the overdue test and the status file.
    pub now: DateTime<Utc>,
    /// How long an object may be missing from the copy, differ or stay unverified before it is
    /// overdue.
    pub alert_after: Duration,
    /// Read every copy and compare its MD5 with the listing's, and copy nothing: what's absent is
    /// reported, and nothing but the listing touches the lake.
    pub verify: bool,
    /// Refuse a destination that is not a mount point.
    pub require_mount: bool,
}

/// What a run found and did, as logged and as written to the status file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub checked_at: DateTime<Utc>,
    /// The last run that left nothing missing, different or unverified, this one included.
    pub last_success: Option<DateTime<Utc>>,
    /// Nothing is overdue.
    pub ok: bool,
    /// Objects in the lake this copies.
    pub source_objects: u64,
    /// Of those, in the copy and matching.
    pub copied_objects: u64,
    /// Copied by this run.
    pub copied_this_run: u64,
    pub bytes_copied: u64,
    /// In the lake, not in the copy after this run.
    pub missing_objects: u64,
    /// In both, and the copy differs (left as it is).
    pub mismatched_objects: u64,
    /// With `verify`: in both at the right size, with no Content-MD5 to check the bytes against.
    pub unverified_objects: u64,
    /// Missing, different or unverified for longer than `alert_after`; the command fails on these.
    pub overdue_objects: u64,
    /// The first paths of those.
    pub overdue_sample: Vec<String>,
    /// Listed under `ocr-local/`, and not copied: `requests/`, or a path the tool won't write.
    pub ignored_objects: u64,
    /// Listed with no Content-MD5, so checked by size alone, or not at all with `verify`.
    pub no_md5_objects: u64,
    /// Every path counted in `mismatched_objects`. The next run hashes these copies whether or
    /// not it's told to verify, so a same-sized one isn't taken for good on its size.
    pub mismatched_paths: Vec<String>,
}

#[derive(Default, Deserialize)]
struct Previous {
    last_success: Option<DateTime<Utc>>,
    #[serde(default)]
    mismatched_paths: HashSet<String>,
}

/// The directories of a path relative to the prefix and its file name, or `None` for a path this
/// doesn't copy.
fn split(info: &ObjectInfo) -> Option<(Vec<&str>, &str)> {
    let relative = info.path.strip_prefix(PREFIX)?.strip_prefix('/')?;
    if relative.starts_with(SKIPPED) || relative.ends_with(PARTIAL) {
        return None;
    }
    let mut parts: Vec<&str> = relative.split('/').collect();
    let name = parts.pop()?;
    let safe = parts.iter().chain([&name]).all(|s| is_safe_segment(s));
    safe.then_some((parts, name))
}

fn is_overdue(now: DateTime<Utc>, info: &ObjectInfo, after: Duration) -> bool {
    let created = info.created.map_or(DateTime::<Utc>::MIN_UTC, Into::into);
    now.signed_duration_since(created)
        .to_std()
        .is_ok_and(|age| age > after)
}

fn md5_of(digest: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// The open directories of the current object's path. Listings are sorted, so runs of objects
/// share a parent and one handle is kept.
struct Dirs {
    root: OwnedFd,
    parent: Option<(String, OwnedFd)>,
}

/// Open `name` under `dir` as a directory, without following a link.
fn open_dir(dir: &OwnedFd, name: &str) -> std::io::Result<OwnedFd> {
    Ok(rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

impl Dirs {
    /// The directory `parts`, made if `create` and it's missing. `None` if it's not there.
    fn enter(&mut self, parts: &[&str], create: bool) -> anyhow::Result<Option<&OwnedFd>> {
        let key = parts.join("/");
        if self.parent.as_ref().is_none_or(|(held, _)| *held != key) {
            let mut dir = rustix::io::dup(&self.root)?;
            for part in parts {
                dir = match open_dir(&dir, part) {
                    Ok(next) => next,
                    Err(e) if e.kind() == ErrorKind::NotFound && !create => return Ok(None),
                    Err(e) if e.kind() == ErrorKind::NotFound => {
                        match rustix::fs::mkdirat(&dir, *part, DIR_MODE) {
                            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                            Err(e) => {
                                return Err(std::io::Error::from(e))
                                    .with_context(|| format!("creating {part}"))
                            }
                        }
                        rustix::fs::fsync(&dir)
                            .map_err(std::io::Error::from)
                            .with_context(|| format!("syncing the entry for {part}"))?;
                        open_dir(&dir, part).with_context(|| format!("opening {part}"))?
                    }
                    Err(e) => return Err(e).with_context(|| format!("opening {part}")),
                };
            }
            self.parent = Some((key, dir));
        }
        Ok(self.parent.as_ref().map(|(_, dir)| dir))
    }
}

/// How a copy compares with the listing.
enum Held {
    Absent,
    Matches,
    Differs,
    /// Asked for a check of the bytes, with no Content-MD5 to check them against.
    Unverified,
}

/// The size of `name` if it's a regular file, `None` if it's anything else.
fn regular_size(dir: &OwnedFd, name: &str) -> std::io::Result<Option<u64>> {
    let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
    Ok(
        (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile)
            .then(|| u64::try_from(stat.st_size).unwrap_or_default()),
    )
}

fn open_read(dir: &OwnedFd, name: &str) -> anyhow::Result<std::fs::File> {
    let file = rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from)
    .with_context(|| format!("reading {name}"))?;
    ensure!(file.metadata()?.is_file(), "{name} is not a regular file");
    Ok(file)
}

async fn md5_file(file: std::fs::File) -> anyhow::Result<String> {
    let mut file = tokio::fs::File::from_std(file);
    let mut hash = Md5::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            return Ok(md5_of(&hash.finalize()));
        }
        hash.update(&buf[..read]);
    }
}

/// `suspect` is a copy a run found different: its bytes are read as with `verify`, and it stays
/// different until they're shown to match.
async fn compare(
    dirs: &mut Dirs,
    parts: &[&str],
    name: &str,
    info: &ObjectInfo,
    verify: bool,
    suspect: bool,
) -> anyhow::Result<Held> {
    let Some(dir) = dirs.enter(parts, false)? else {
        return Ok(Held::Absent);
    };
    match regular_size(dir, name) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Held::Absent),
        Err(e) => return Err(e.into()),
        Ok(size) if size != Some(info.size) => return Ok(Held::Differs),
        Ok(_) => {}
    }
    if !verify && !suspect {
        return Ok(Held::Matches);
    }
    match &info.content_md5 {
        Some(expected) if md5_file(open_read(dir, name)?).await? == *expected => Ok(Held::Matches),
        Some(_) => Ok(Held::Differs),
        None if suspect => Ok(Held::Differs),
        None => Ok(Held::Unverified),
    }
}

/// Stream an object into a partial, check it, and link it into place. Returns its size.
async fn fetch(
    source: &dyn ObjectStore,
    dirs: &mut Dirs,
    parts: &[&str],
    name: &str,
    info: &ObjectInfo,
) -> anyhow::Result<u64> {
    let dir = dirs.enter(parts, true)?.context("no directory")?;
    let partial = format!("{name}{PARTIAL}");
    match regular_size(dir, &partial) {
        Ok(Some(_)) => rustix::fs::unlinkat(dir, partial.as_str(), AtFlags::empty())
            .map_err(std::io::Error::from)
            .with_context(|| format!("removing a stale {partial}"))?,
        Ok(None) => bail!("{partial} is not a regular file: left as it is"),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let stream = source
        .get_stream(&info.path)
        .await?
        .context("it was listed but is gone")?;
    let file = rustix::fs::openat(
        dir,
        partial.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        FILE_MODE,
    )
    .map(std::fs::File::from)
    .map_err(std::io::Error::from)
    .with_context(|| format!("creating {partial}"))?;
    let result = fetch_into(stream, file, dir, name, &partial, info).await;
    if result.is_err() {
        let _ = rustix::fs::unlinkat(dir, partial.as_str(), AtFlags::empty());
    }
    result
}

async fn fetch_into(
    mut stream: usnm_store::ByteStream,
    file: std::fs::File,
    dir: &OwnedFd,
    name: &str,
    partial: &str,
    info: &ObjectInfo,
) -> anyhow::Result<u64> {
    let mut file = tokio::fs::File::from_std(file);
    let (mut hash, mut n) = (Md5::new(), 0u64);
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
    let received = md5_of(&hash.finalize());
    if let Some(expected) = &info.content_md5 {
        ensure!(
            received == *expected,
            "the bytes received are not the listed Content-MD5"
        );
    }
    ensure!(
        md5_file(open_read(dir, partial)?).await? == received,
        "the file written is not the bytes received"
    );
    match rustix::fs::linkat(dir, partial, dir, name, AtFlags::empty()) {
        Ok(()) => {}
        Err(rustix::io::Errno::EXIST) => bail!("{name} appeared meanwhile; left as it is"),
        Err(e) => return Err(std::io::Error::from(e)).with_context(|| format!("linking {name}")),
    }
    rustix::fs::unlinkat(dir, partial, AtFlags::empty()).map_err(std::io::Error::from)?;
    rustix::fs::fsync(dir).map_err(std::io::Error::from)?;
    Ok(n)
}

/// Open the destination and take its lock, held by the returned handle until the run ends.
fn prepare(o: &Options<'_>) -> anyhow::Result<OwnedFd> {
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
            "the destination {} is not a mount point (--allow-unmounted for a test)",
            o.dest.display()
        );
    }
    let root: OwnedFd = std::fs::File::open(o.dest)?.into();
    rustix::fs::flock(&root, FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| anyhow::anyhow!("another OCR output copy is running: one at a time"))?;
    Ok(root)
}

/// Copy what the destination lacks, or with `verify` only check it. `Err` for what stops the whole run; an object that can't be
/// copied is in the report and the run goes on.
pub async fn copy_objects(source: &dyn ObjectStore, o: &Options<'_>) -> anyhow::Result<Report> {
    let root = prepare(o)?;
    let previous = o.status.map(read_previous).unwrap_or_default();
    let listed = source
        .list_info(PREFIX)
        .await
        .context("listing the OCR output")?;
    let mut report = Report {
        checked_at: o.now,
        ..Report::default()
    };
    let mut dirs = Dirs { root, parent: None };
    for info in &listed {
        let Some((parts, name)) = split(info) else {
            report.ignored_objects += 1;
            continue;
        };
        report.source_objects += 1;
        report.no_md5_objects += u64::from(info.content_md5.is_none());
        let suspect = previous.mismatched_paths.contains(&info.path);
        let confirmed = match compare(&mut dirs, &parts, name, info, o.verify, suspect).await {
            Ok(Held::Matches) => true,
            Ok(Held::Absent) if o.verify => {
                tracing::warn!(path = %info.path, "not in the copy");
                report.missing_objects += 1;
                false
            }
            Ok(Held::Absent) => match fetch(source, &mut dirs, &parts, name, info).await {
                Ok(bytes) => {
                    report.copied_this_run += 1;
                    report.bytes_copied += bytes;
                    true
                }
                Err(e) => {
                    tracing::warn!(path = %info.path, error = %format!("{e:#}"), "not copied");
                    report.missing_objects += 1;
                    false
                }
            },
            Ok(Held::Differs) => {
                tracing::warn!(path = %info.path, "the copy differs from the lake's; left as it is");
                report.mismatched_objects += 1;
                report.mismatched_paths.push(info.path.clone());
                false
            }
            Ok(Held::Unverified) => {
                tracing::warn!(path = %info.path, "no Content-MD5 to check the copy against");
                report.unverified_objects += 1;
                false
            }
            Err(e) => {
                tracing::warn!(path = %info.path, error = %format!("{e:#}"), "could not be checked");
                report.mismatched_objects += 1;
                report.mismatched_paths.push(info.path.clone());
                false
            }
        };
        if confirmed {
            report.copied_objects += 1;
        } else if is_overdue(o.now, info, o.alert_after) {
            report.overdue_objects += 1;
            if report.overdue_sample.len() < SAMPLE {
                report.overdue_sample.push(info.path.clone());
            }
        }
    }
    report.ok = report.overdue_objects == 0;
    let caught_up = report.missing_objects == 0
        && report.mismatched_objects == 0
        && report.unverified_objects == 0;
    report.last_success = if caught_up {
        Some(o.now)
    } else {
        previous.last_success
    };
    if let Some(path) = o.status {
        write_status_file(path, &report)
            .with_context(|| format!("writing the status file {}", path.display()))?;
    }
    if let Err(error) = rustix::fs::flock(&dirs.root, FlockOperation::Unlock) {
        tracing::warn!(%error, "releasing the OCR output copy lock failed");
    }
    Ok(report)
}

fn read_previous(path: &Path) -> Previous {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
