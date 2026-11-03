//! Scan paths, sidecars, checksums and durable prefetch file I/O.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use usnm_ocr_contract::prefetch::ManifestRow;

use crate::batch;
use crate::loc::{ImageSize, ScanFormat};
use crate::repair_io::{
    sync_directories as sync_repair_directories, sync_file as sync_repair_file,
};

use super::{set_of, sha256};

#[cfg(test)]
pub(super) use crate::repair_io::sync_test;

/// The tag of a size in file names: `full` or `pct50`.
pub fn tag(size: ImageSize) -> String {
    match size {
        ImageSize::Full => "full".into(),
        ImageSize::Pct(p) => format!("pct{p}"),
    }
}

/// Where a page's scan goes: under its set's directory.
pub fn scan_path(out: &Path, row: &ManifestRow, size: ImageSize, format: ScanFormat) -> PathBuf {
    page_path(
        &out.join(set_of(row).dir()),
        &row.batch,
        &row.lccn,
        row.date,
        row.edition,
        row.seq,
        size,
        format,
    )
}

/// The formats a scan at `size` may be on disk in, the one fetched now
/// first: a full scan as the batch's JP2 or the IIIF JPEG; a scaled one
/// only as a JPEG.
pub fn formats(size: ImageSize) -> &'static [ScanFormat] {
    match size {
        ImageSize::Full => &[ScanFormat::Jp2, ScanFormat::Jpeg],
        ImageSize::Pct(_) => &[ScanFormat::Jpeg],
    }
}

/// Where the scan of page `seq` of an issue goes, under a set's directory.
#[allow(clippy::too_many_arguments)]
pub fn page_path(
    set_dir: &Path,
    batch: &str,
    lccn: &str,
    date: chrono::NaiveDate,
    edition: u16,
    seq: u16,
    size: ImageSize,
    format: ScanFormat,
) -> PathBuf {
    let ext = match size {
        ImageSize::Full => format.ext().to_owned(),
        ImageSize::Pct(p) => format!("pct{p}.jpg"),
    };
    set_dir
        .join(batch)
        .join(lccn)
        .join(date.format("%Y-%m-%d").to_string())
        .join(edition.to_string())
        .join(format!("{seq}.{ext}"))
}

/// The bytes of scans (`*.jpg`, `*.jp2`) under `dir`, 0 if it isn't there. Any
/// other error is returned, so a cap is never checked against a count that
/// missed files.
pub fn dir_scan_bytes(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0;
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && d == dir => return Ok(0),
            // Deleted while counting.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for e in entries {
            let e = e?;
            let t = e.file_type()?;
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() && {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.ends_with(".jpg") || n.ends_with(".jp2")
            } {
                match e.metadata() {
                    Ok(m) => total += m.len(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(total)
}

/// The marker a consumed buffer scan leaves (`<scan>.done`, its sidecar
/// renamed by the scan server's delete), so a later run doesn't fetch it
/// again.
pub fn consumed_path(scan: &Path) -> PathBuf {
    let mut p = scan.as_os_str().to_owned();
    p.push(".done");
    PathBuf::from(p)
}

/// The sidecar beside a scan.
pub fn sidecar_path(scan: &Path) -> PathBuf {
    let mut p = scan.as_os_str().to_owned();
    p.push(".json");
    PathBuf::from(p)
}

/// What is recorded beside each scan, and in the index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sidecar {
    pub doc_id: String,
    pub batch: String,
    pub batch_version: u16,
    /// The image URL fetched, on tile.loc.gov: the IIIF JPEG, or the JP2
    /// on the storage service.
    pub loc_url: String,
    /// The page's IIIF base: its id, whichever file was fetched.
    pub iiif_base: String,
    /// `full` or `scaled:<n>`.
    pub resolution: String,
    pub fetched_at: DateTime<Utc>,
    pub bytes: u64,
    pub sha256: String,
    /// The image request's time, sent to last byte.
    pub download_ms: u64,
    pub user_agent: String,
    /// `jpeg` (the IIIF JPEG; sidecars from before the batch route have
    /// none) or `jp2` (the batch's original).
    #[serde(default = "jpeg")]
    pub format: String,
    /// For a JP2, the md5 the batch's bag manifest lists, which the bytes
    /// matched when fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
}

fn jpeg() -> String {
    ScanFormat::Jpeg.name().into()
}

/// Whether `bytes` are what `side` records: the size, the sha256 and, if
/// it has one, the md5, which a JP2's sidecar must have.
pub fn matches_sidecar(bytes: &[u8], side: &Sidecar) -> bool {
    // A JP2 is only ever written with the bag's md5.
    (side.format != ScanFormat::Jp2.name() || side.md5.is_some())
        && bytes.len() as u64 == side.bytes
        && sha256(bytes) == side.sha256
        && side
            .md5
            .as_ref()
            .is_none_or(|m| batch::md5_hex(bytes) == *m)
}

/// The sidecar at `scan`, if it and the scan are there and
/// agree (format, size, sha256 and md5).
async fn verified(
    scan: &Path,
    row: &ManifestRow,
    size: ImageSize,
    format: ScanFormat,
) -> Option<Sidecar> {
    let side = tokio::fs::read(sidecar_path(scan)).await.ok()?;
    let side = serde_json::from_slice::<Sidecar>(&side).ok()?;
    if side.doc_id != row.doc_id
        || side.resolution != size.to_string()
        || side.format != format.name()
    {
        return None;
    }
    let b = tokio::fs::read(scan).await.ok()?;
    matches_sidecar(&b, &side).then_some(side)
}

/// The size of the page's scan, if one is there in any format and checks out.
pub(super) async fn there(out: &Path, row: &ManifestRow, size: ImageSize) -> Option<u64> {
    for f in formats(size) {
        if let Some(side) = verified(&scan_path(out, row, size, *f), row, size, *f).await {
            return Some(side.bytes);
        }
    }
    None
}

async fn occupied(scan: &Path) -> std::io::Result<bool> {
    for path in [sidecar_path(scan), scan.to_owned()] {
        match tokio::fs::symlink_metadata(path).await {
            Ok(_) => return Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// A completed repair may be replayed only if every occupied format agrees.
pub(super) async fn repair_there(
    out: &Path,
    row: &ManifestRow,
    size: ImageSize,
) -> std::io::Result<Option<Sidecar>> {
    let mut found: Option<Sidecar> = None;
    for f in formats(size) {
        let scan = scan_path(out, row, size, *f);
        if !occupied(&scan).await? {
            continue;
        }
        let Some(side) = verified(&scan, row, size, *f).await else {
            return Ok(None);
        };
        if side.batch != row.batch
            || side.batch_version != row.batch_version
            || found
                .as_ref()
                .is_some_and(|s| s.bytes != side.bytes || s.sha256 != side.sha256)
        {
            return Ok(None);
        }
        if found.is_none() {
            found = Some(side);
        }
    }
    Ok(found)
}

/// Validate both formats before indexing; the caller holds the output-wide lock.
pub(super) async fn index_repair(
    out: &Path,
    row: &ManifestRow,
    size: ImageSize,
    side: &Sidecar,
    index: &Lines,
) -> anyhow::Result<bool> {
    if repair_there(out, row, size).await?.as_ref() != Some(side) {
        return Ok(false);
    }
    fence_repair(out, row, size).await?;
    let scans: Vec<_> = formats(size)
        .iter()
        .map(|format| scan_path(out, row, size, *format))
        .collect();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        for scan in scans {
            match std::fs::symlink_metadata(&scan) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            }
            for path in [&scan, &sidecar_path(&scan)] {
                sync_repair_file(&std::fs::File::open(path)?, path)?;
            }
            sync_repair_directories(scan.parent().context("no repair scan parent")?)?;
        }
        Ok(())
    })
    .await??;
    index.ensure_sidecar(side)?;
    // An unsupported writer may ignore the lock. Preserve any appended history,
    // but do not report success if it changed the target while we read the index.
    anyhow::ensure!(
        repair_there(out, row, size).await?.as_ref() == Some(side),
        "repair target changed during indexing; preserve files and index for operator review"
    );
    Ok(true)
}

/// A repair may not replace even an incomplete or unverifiable retained copy.
pub(super) async fn repair_occupied(
    out: &Path,
    row: &ManifestRow,
    size: ImageSize,
) -> std::io::Result<bool> {
    for f in formats(size) {
        if occupied(&scan_path(out, row, size, *f)).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn repair_fence_path(out: &Path, row: &ManifestRow, size: ImageSize) -> PathBuf {
    scan_path(out, row, size, ScanFormat::Jpeg).with_extension("decode-repair")
}

/// A durable, extension-neutral claim survives a repair dying between publications.
pub(super) async fn repair_fenced(
    out: &Path,
    row: &ManifestRow,
    size: ImageSize,
) -> std::io::Result<bool> {
    match tokio::fs::symlink_metadata(repair_fence_path(out, row, size)).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub(super) async fn fence_repair(
    out: &Path,
    row: &ManifestRow,
    size: ImageSize,
) -> anyhow::Result<()> {
    let path = repair_fence_path(out, row, size);
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let dir = path.parent().context("no repair fence parent")?;
        std::fs::create_dir_all(dir)?;
        let fence = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    std::fs::symlink_metadata(&path)?.file_type().is_file(),
                    "repair fence is not a regular file: {}",
                    path.display()
                );
                std::fs::File::open(&path)?
            }
            Err(e) => return Err(e).context("creating repair fence"),
        };
        sync_repair_file(&fence, &path)?;
        sync_repair_directories(dir)?;
        Ok(())
    })
    .await?
}

/// Write `body` to `path` through a synced `.part` file and a rename.
pub(super) async fn write_atomic(path: &Path, body: Vec<u8>) -> anyhow::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let dir = path.parent().context("no parent directory")?;
        std::fs::create_dir_all(dir)?;
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut f = std::fs::File::create(&part)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&part, &path)?;
        Ok(())
    })
    .await?
}

/// Publish without replacing a late target; return false when that target exists.
pub(super) async fn write_new_atomic(path: &Path, body: Vec<u8>) -> anyhow::Result<bool> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let dir = path.parent().context("no parent directory")?;
        std::fs::create_dir_all(dir)?;
        let mut part = path.as_os_str().to_owned();
        part.push(format!(".repair-{}.part", std::process::id()));
        let part = PathBuf::from(part);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)?;
        let written = file
            .write_all(&body)
            .map_err(anyhow::Error::from)
            .and_then(|_| sync_repair_file(&file, &part));
        drop(file);
        let published = match written {
            Ok(()) => match std::fs::hard_link(&part, &path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error.into()),
            },
            Err(error) => Err(error),
        };
        let cleanup = std::fs::remove_file(&part);
        match (published, cleanup) {
            (Ok(published), Ok(())) => {
                sync_repair_directories(dir)?;
                Ok(published)
            }
            (Err(error), Ok(())) => Err(error)
                .with_context(|| format!("publishing repair without replacing {}", path.display())),
            (Ok(_), Err(error)) => {
                Err(error).with_context(|| format!("removing repair temporary {}", part.display()))
            }
            (Err(error), Err(cleanup_error)) => Err(error).with_context(|| {
                format!(
                    "publishing repair {}; removing {} also failed: {cleanup_error}",
                    path.display(),
                    part.display()
                )
            }),
        }
    })
    .await?
}

/// Publish a repair and its sidecar, leaving any partial result for review.
pub(super) async fn write_repair_atomic(
    path: &Path,
    body: Vec<u8>,
    side: Vec<u8>,
) -> anyhow::Result<bool> {
    if !write_new_atomic(path, body).await? {
        return Ok(false);
    }
    write_new_atomic(&sidecar_path(path), side).await
}

/// An append-only JSON Lines file, shared by the tasks.
pub(super) struct Lines {
    file: Mutex<std::fs::File>,
    path: PathBuf,
}

impl Lines {
    pub(super) fn open(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            file: Mutex::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("opening {}", path.display()))?,
            ),
            path: path.to_owned(),
        })
    }

    pub(super) fn push(&self, v: &impl Serialize) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        let mut f = self.file.lock().expect("lines lock");
        f.write_all(&line)?;
        Ok(())
    }

    /// Validate/sync a fresh repair append or recover one omitted by a crash.
    pub(super) fn ensure_sidecar(&self, side: &Sidecar) -> anyhow::Result<()> {
        let mut f = self.file.lock().expect("lines lock");
        let mut reader = std::io::BufReader::new(std::fs::File::open(&self.path)?);
        let mut line = String::new();
        let mut found = false;
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            anyhow::ensure!(
                line.ends_with('\n'),
                "incomplete repair index {}; operator review required",
                self.path.display()
            );
            let indexed: serde_json::Value = serde_json::from_str(&line)
                .with_context(|| format!("reading repair index {}", self.path.display()))?;
            anyhow::ensure!(
                indexed.is_object(),
                "non-object record in repair index {}; operator review required",
                self.path.display()
            );
            // Older index entries may lack fields that current sidecars require.
            if serde_json::from_value::<Sidecar>(indexed).ok().as_ref() == Some(side) {
                found = true;
            }
        }
        if found {
            sync_repair_file(&f, &self.path)?;
            sync_repair_directories(self.path.parent().context("no repair index parent")?)?;
            return Ok(());
        }
        // Stream the existing history only for repairs; never rewrite it.
        let mut line = serde_json::to_vec(side)?;
        line.push(b'\n');
        f.write_all(&line)?;
        sync_repair_file(&f, &self.path)?;
        sync_repair_directories(self.path.parent().context("no repair index parent")?)?;
        Ok(())
    }
}

/// Hold the lock file for the life of the process, so only one prefetch
/// fetches at a time.
pub(super) fn lock(path: &Path) -> anyhow::Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(|_| {
        anyhow::anyhow!(
            "another prefetch holds {}: one fetching process at a time",
            path.display()
        )
    })?;
    Ok(f)
}

/// A custom cross-output lock supplements, never replaces, the output-wide lock.
pub(super) fn lock_additional(
    path: &Path,
    held: &std::fs::File,
) -> anyhow::Result<Option<std::fs::File>> {
    let held = rustix::fs::fstat(held)?;
    if let Ok(other) = rustix::fs::stat(path) {
        if (held.st_dev, held.st_ino) == (other.st_dev, other.st_ino) {
            return Ok(None);
        }
    }
    lock(path).map(Some)
}

/// The last lines of an index file (read from its last 64 KB): the last
/// JPEG and the last JP2 fetched, each if there is one.
pub(super) fn last_fetched(path: &Path) -> Vec<Sidecar> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let mut tail = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)))
        .is_err()
        || f.read_to_end(&mut tail).is_err()
    {
        return Vec::new();
    }
    let mut out: Vec<Sidecar> = Vec::new();
    for s in String::from_utf8_lossy(&tail)
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Sidecar>(l).ok())
    {
        if !out.iter().any(|o| o.format == s.format) {
            out.push(s);
        }
        if out.len() == 2 {
            break;
        }
    }
    out
}

/// Read index lines (JSON Lines of [`Sidecar`]).
pub fn read_index(text: &str) -> anyhow::Result<Vec<Sidecar>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).with_context(|| format!("index line {}", i + 1)))
        .collect()
}
