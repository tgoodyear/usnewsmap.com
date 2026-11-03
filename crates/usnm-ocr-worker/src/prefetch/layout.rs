//! Scan paths, sidecars, checksums and durable prefetch file I/O.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use usnm_ocr_contract::prefetch::ManifestRow;

use crate::batch;
use crate::loc::{ImageSize, ScanFormat};

use super::{set_of, sha256};

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

/// The size of the scan at `scan`, if it and its sidecar are there and
/// agree (format, size, sha256 and md5).
async fn verified(
    scan: &Path,
    row: &ManifestRow,
    size: ImageSize,
    format: ScanFormat,
) -> Option<u64> {
    let side = tokio::fs::read(sidecar_path(scan)).await.ok()?;
    let side = serde_json::from_slice::<Sidecar>(&side).ok()?;
    if side.doc_id != row.doc_id
        || side.resolution != size.to_string()
        || side.format != format.name()
    {
        return None;
    }
    let b = tokio::fs::read(scan).await.ok()?;
    matches_sidecar(&b, &side).then_some(side.bytes)
}

/// The size of the page's scan, if one is there in any format and checks out.
pub(super) async fn there(out: &Path, row: &ManifestRow, size: ImageSize) -> Option<u64> {
    for f in formats(size) {
        if let Some(n) = verified(&scan_path(out, row, size, *f), row, size, *f).await {
            return Some(n);
        }
    }
    None
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

/// An append-only JSON Lines file, shared by the tasks.
pub(super) struct Lines(Mutex<std::fs::File>);

impl Lines {
    pub(super) fn open(path: &Path) -> anyhow::Result<Self> {
        Ok(Self(Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("opening {}", path.display()))?,
        )))
    }

    pub(super) fn push(&self, v: &impl Serialize) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        let mut f = self.0.lock().expect("lines lock");
        f.write_all(&line)?;
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
