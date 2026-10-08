//! Retained batch archives, for benchmark sets (`USNM_RETAIN_RAW`; ADR-0006
//! keeps no raw archives in production, and that is unchanged there).
//!
//! With a raw store, curation writes each archive it downloads, byte for byte
//! as fetched, to `{batch}/{archive file}` (Blob access tier Cold), as it
//! streams: a tee of the download, uploaded in blocks, never held whole. The
//! upload is committed only once the archive's sha256 checks out, and then
//! `{batch}/manifest.json` records where it came from. The batch is marked
//! curated only after both. A later curation of the same batch version (a
//! re-curation, or another environment's run for a benchmark) reads the
//! retained copy instead of LoC when its manifest's sha256 matches the one
//! LoC lists, and checks it again as it reads.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use usnm_store::ObjectStore;

use crate::source::{self, Download};

/// The access tier of retained archives: read rarely, kept long.
pub const TIER: &str = "Cold";

/// What `{batch}/manifest.json` says about a retained archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The batch with its version, `az_fireant_ver01`.
    pub batch: String,
    /// The archive's path in the raw store.
    pub path: String,
    pub source_url: String,
    pub bytes: u64,
    pub sha256: String,
    pub fetched_at: DateTime<Utc>,
    /// The download's response headers (last-modified, etag, ...).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// The archive's file name: the last segment of its URL, or `{batch}.tar.bz2`
/// when that isn't a plain name.
pub fn archive_name(batch: &str, url: &str) -> String {
    url.rsplit('/')
        .next()
        .map(|n| n.split(['?', '#']).next().unwrap_or(n))
        .filter(|n| usnm_store::is_safe_segment(n))
        .map_or_else(|| format!("{batch}.tar.bz2"), str::to_owned)
}

fn manifest_path(batch: &str) -> String {
    format!("{batch}/manifest.json")
}

/// The retained copy of `batch`, if there is one and it is the archive
/// `expected` names (LoC's sha256, when listed).
pub async fn find(
    raw: &dyn ObjectStore,
    batch: &str,
    expected: Option<&str>,
) -> anyhow::Result<Option<Manifest>> {
    let path = manifest_path(batch);
    let Some(bytes) = raw.get(&path).await? else {
        return Ok(None);
    };
    let m: Manifest = serde_json::from_slice(&bytes).with_context(|| path.clone())?;
    match expected {
        Some(want) if !want.eq_ignore_ascii_case(&m.sha256) => {
            tracing::warn!(
                batch,
                retained = %m.sha256,
                listed = want,
                "the retained archive isn't the listed one; downloading"
            );
            Ok(None)
        }
        _ => Ok(Some(m)),
    }
}

/// Stream a retained archive as a download.
pub async fn open(raw: &dyn ObjectStore, m: &Manifest) -> anyhow::Result<Download> {
    let stream = raw
        .get_stream(&m.path)
        .await?
        .with_context(|| format!("the retained archive `{}` is missing", m.path))?;
    Ok(source::from_store(stream, &m.path, Some(m.bytes)))
}

/// An archive being retained as it downloads.
pub struct Upload {
    path: String,
    commit: tokio::sync::oneshot::Sender<bool>,
    task: tokio::task::JoinHandle<Result<u64, usnm_store::StoreError>>,
}

/// Start retaining an archive at `path`: the sender is the download's tee.
pub fn start(
    raw: Arc<dyn ObjectStore>,
    path: &str,
) -> (tokio::sync::mpsc::Sender<bytes::Bytes>, Upload) {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let (commit, committed) = tokio::sync::oneshot::channel();
    let p = path.to_owned();
    let task = tokio::spawn(async move {
        raw.put_stream(&p, "application/x-bzip2", Some(TIER), rx, committed)
            .await
    });
    (
        tx,
        Upload {
            path: path.to_owned(),
            commit,
            task,
        },
    )
}

impl Upload {
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Commit the archive (once the download has ended and checked out) and
    /// wait for it; its size.
    pub async fn commit(self) -> anyhow::Result<u64> {
        let _ = self.commit.send(true);
        let n = self
            .task
            .await
            .context("the raw upload's task failed")?
            .with_context(|| format!("retaining `{}`", self.path))?;
        Ok(n)
    }

    /// Give the upload up: nothing becomes visible.
    pub fn abandon(self) {
        let _ = self.commit.send(false);
    }
}

/// Record a retained archive. Written after the archive, so a manifest
/// always names a complete one.
pub async fn record(raw: &dyn ObjectStore, m: &Manifest) -> anyhow::Result<()> {
    raw.put(
        &manifest_path(&m.batch),
        serde_json::to_vec_pretty(m)?,
        "application/json",
    )
    .await?;
    Ok(())
}

/// What [`copy`] did.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Copied {
    pub archives: u64,
    pub bytes: u64,
    /// Already there with the same sha256.
    pub skipped: u64,
    pub listings: u64,
}

/// Copy retained archives (`batches`, `{batch}_verNN`) and the kept batch
/// lists from one raw store to another, e.g. from an environment's own
/// `raw` container into the archival account. Each archive is streamed,
/// checked against its manifest's sha256 as it goes, and committed (Cold)
/// before its manifest is written; one already there with the same sha256
/// is left alone.
pub async fn copy(
    from: &dyn ObjectStore,
    to: &dyn ObjectStore,
    batches: &[String],
) -> anyhow::Result<Copied> {
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    let mut out = Copied::default();
    for b in batches {
        let m = find(from, b, None)
            .await?
            .with_context(|| format!("`{b}` has no retained archive to copy"))?;
        if find(to, b, Some(&m.sha256)).await?.is_some() {
            out.skipped += 1;
            continue;
        }
        let mut stream = from
            .get_stream(&m.path)
            .await?
            .with_context(|| format!("`{}` is missing", m.path))?;
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let (commit, committed) = tokio::sync::oneshot::channel();
        let upload = to.put_stream(&m.path, "application/x-bzip2", Some(TIER), rx, committed);
        let feed = async {
            let (mut hash, mut n) = (Sha256::new(), 0u64);
            while let Some(c) = stream.next().await {
                let c = c?;
                hash.update(&c);
                n += c.len() as u64;
                if tx.send(c).await.is_err() {
                    break;
                }
            }
            drop(tx);
            let sha = source::hex(&hash.finalize());
            let ok = n == m.bytes && sha.eq_ignore_ascii_case(&m.sha256);
            let _ = commit.send(ok);
            anyhow::ensure!(
                ok,
                "`{}` read as {n} bytes, sha256 {sha}; its manifest says {} bytes, {}",
                m.path,
                m.bytes,
                m.sha256
            );
            Ok::<_, anyhow::Error>(n)
        };
        let (uploaded, fed) = tokio::join!(upload, feed);
        let n = fed?;
        uploaded.with_context(|| format!("copying `{}`", m.path))?;
        record_once(to, &m).await?;
        tracing::info!(batch = %b, mb = n / (1024 * 1024), "archive copied");
        out.archives += 1;
        out.bytes += n;
    }
    for path in from.list("listings").await? {
        if to.exists(&path).await? {
            continue;
        }
        let bytes = from
            .get(&path)
            .await?
            .with_context(|| format!("`{path}` vanished"))?;
        to.put(&path, bytes, "application/json").await?;
        out.listings += 1;
    }
    Ok(out)
}

/// [`record`], unless another curation (another environment's, sharing
/// the archival account) recorded the batch first: then its manifest must
/// name the same archive, which the two uploads then both wrote.
pub async fn record_once(raw: &dyn ObjectStore, m: &Manifest) -> anyhow::Result<()> {
    let path = manifest_path(&m.batch);
    if raw
        .put_new(&path, serde_json::to_vec_pretty(m)?, "application/json")
        .await?
    {
        return Ok(());
    }
    let theirs = find(raw, &m.batch, None)
        .await?
        .with_context(|| format!("`{path}` vanished"))?;
    if theirs.sha256.eq_ignore_ascii_case(&m.sha256) && theirs.bytes == m.bytes {
        tracing::info!(batch = %m.batch, "the archive was already retained, the same one");
        return Ok(());
    }
    anyhow::bail!(
        "`{path}` names another archive (sha256 {}) than this curation's ({}); the retained copy is ambiguous",
        theirs.sha256,
        m.sha256
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_archive_from_its_url() {
        assert_eq!(
            archive_name(
                "az_fireant_ver01",
                "https://chroniclingamerica.loc.gov/data/ocr/az_fireant_ver01.tar.bz2"
            ),
            "az_fireant_ver01.tar.bz2"
        );
        assert_eq!(
            archive_name("b_ver01", "/tmp/x/b_ver01.tar.gz"),
            "b_ver01.tar.gz"
        );
        assert_eq!(archive_name("b_ver01", "https://h/"), "b_ver01.tar.bz2");
        assert_eq!(archive_name("b_ver01", "https://h/a b"), "b_ver01.tar.bz2");
    }

    /// The tee holds every byte the parser reads, the checksum still
    /// verifies, and the copy is found and read back instead of the source.
    #[tokio::test]
    async fn a_teed_download_is_retained_and_read_back() {
        use sha2::Digest;
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b_ver01.tar.bz2");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(&file, &data).unwrap();
        let want = source::hex(&sha2::Sha256::digest(&data));
        let raw: Arc<dyn ObjectStore> =
            Arc::new(usnm_store::LocalStore::new(dir.path().join("raw")));

        let path = format!(
            "b_ver01/{}",
            archive_name("b_ver01", file.to_str().unwrap())
        );
        let (tee, upload) = start(raw.clone(), &path);
        let mut d = source::open_with(file.to_str().unwrap(), 1, Some(tee))
            .await
            .unwrap();
        let read = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            d.reader.read_to_end(&mut out).unwrap();
            (out, d.digest)
        });
        let (parsed, digest) = read.await.unwrap();
        let sha = digest.await.unwrap().unwrap();
        assert_eq!(parsed, data);
        assert_eq!(sha, want);
        assert_eq!(upload.commit().await.unwrap(), data.len() as u64);
        assert_eq!(raw.get(&path).await.unwrap().unwrap(), data);
        let m = Manifest {
            batch: "b_ver01".into(),
            path: path.clone(),
            source_url: file.display().to_string(),
            bytes: data.len() as u64,
            sha256: sha.clone(),
            fetched_at: Utc::now(),
            headers: BTreeMap::new(),
        };
        record(raw.as_ref(), &m).await.unwrap();

        // Found for the listed checksum (or none), not for another.
        assert_eq!(
            find(raw.as_ref(), "b_ver01", Some(&want)).await.unwrap(),
            Some(m.clone())
        );
        assert!(find(raw.as_ref(), "b_ver01", None).await.unwrap().is_some());
        assert!(find(raw.as_ref(), "b_ver01", Some(&"0".repeat(64)))
            .await
            .unwrap()
            .is_none());
        assert!(find(raw.as_ref(), "other_ver01", None)
            .await
            .unwrap()
            .is_none());

        // Read back: the same bytes and checksum, with the source gone.
        std::fs::remove_file(&file).unwrap();
        let mut d = open(raw.as_ref(), &m).await.unwrap();
        let (back, digest) = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            d.reader.read_to_end(&mut out).unwrap();
            (out, d.digest)
        })
        .await
        .unwrap();
        assert_eq!(back, data);
        assert_eq!(digest.await.unwrap().unwrap(), want);
    }

    #[tokio::test]
    async fn copies_archives_and_listings_once() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let from = usnm_store::LocalStore::new(dir.path().join("a"));
        let to = usnm_store::LocalStore::new(dir.path().join("b"));
        let data = b"an archive".to_vec();
        let sha = source::hex(&sha2::Sha256::digest(&data));
        from.put("b_ver01/b_ver01.tar.bz2", data.clone(), "x")
            .await
            .unwrap();
        let m = Manifest {
            batch: "b_ver01".into(),
            path: "b_ver01/b_ver01.tar.bz2".into(),
            source_url: "https://x/b_ver01.tar.bz2".into(),
            bytes: data.len() as u64,
            sha256: sha,
            fetched_at: Utc::now(),
            headers: BTreeMap::new(),
        };
        record(&from, &m).await.unwrap();
        from.put("listings/t-abc.json", b"[]".to_vec(), "application/json")
            .await
            .unwrap();
        let batches = vec!["b_ver01".to_owned()];
        let c = copy(&from, &to, &batches).await.unwrap();
        assert_eq!((c.archives, c.bytes, c.skipped, c.listings), (1, 10, 0, 1));
        assert_eq!(to.get(&m.path).await.unwrap().unwrap(), data);
        assert_eq!(
            find(&to, "b_ver01", Some(&m.sha256)).await.unwrap(),
            Some(m.clone())
        );
        let again = copy(&from, &to, &batches).await.unwrap();
        assert_eq!((again.archives, again.skipped, again.listings), (0, 1, 0));
        // A copy that doesn't match its manifest isn't committed.
        from.put(&m.path, b"changed!!!".to_vec(), "x")
            .await
            .unwrap();
        let to2 = usnm_store::LocalStore::new(dir.path().join("c"));
        assert!(copy(&from, &to2, &batches).await.is_err());
        assert!(to2.get(&m.path).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_abandoned_upload_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let raw: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        let (tee, upload) = start(raw.clone(), "b_ver01/b.tar.bz2");
        tee.send(bytes::Bytes::from_static(b"partial"))
            .await
            .unwrap();
        drop(tee);
        upload.abandon();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(raw.get("b_ver01/b.tar.bz2").await.unwrap().is_none());
    }
}
