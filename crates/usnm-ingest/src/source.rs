//! Where batches come from (04 §4.1, §4.4 `discover`).
//!
//! `enqueue` takes a batch list: JSON `[{name, url, sha256?, ocr_source?}]`,
//! where `name` is the LoC batch name with its version suffix
//! (`batch_az_acacia_ver02`). Reading that list off the Chronicling America
//! Datasets portal is the next step, once spike S-1 has confirmed the
//! portal's listing format.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::Utc;
use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::state::{Batch, BatchStatus, State};

/// Sent with every request to LoC (04 §4.1 good-citizen policy).
pub const USER_AGENT: &str = concat!(
    "usnewsmap-ingest/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/tgoodyear/usnewsmap.com)"
);

#[derive(Debug, Clone, Deserialize)]
pub struct ListedBatch {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub ocr_source: Option<String>,
}

/// Split `batch_az_acacia_ver02` into (`batch_az_acacia`, 2).
pub fn split_version(name: &str) -> anyhow::Result<(String, u16)> {
    let (base, ver) = name
        .rsplit_once("_ver")
        .with_context(|| format!("batch name `{name}` has no `_verNN` suffix"))?;
    let version: u16 = ver
        .parse()
        .ok()
        .filter(|v| *v > 0)
        .with_context(|| format!("batch name `{name}` has an invalid version"))?;
    if !usnm_store::is_safe_segment(base) {
        bail!("batch name `{name}` is not a safe identifier");
    }
    Ok((base.to_owned(), version))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnqueueReport {
    pub new: usize,
    pub new_version: usize,
    pub requeued: usize,
    pub unchanged: usize,
}

/// Record every listed batch; queue new batches, new versions, and batches
/// that failed or were never curated.
pub async fn enqueue(state: &State, list: &[ListedBatch]) -> anyhow::Result<EnqueueReport> {
    let mut report = EnqueueReport::default();
    for l in list {
        let (batch, version) = split_version(&l.name)?;
        if let Some(sha) = &l.sha256 {
            anyhow::ensure!(
                sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}: sha256 must be 64 hex digits",
                l.name
            );
        }
        let fresh = Batch {
            id: batch.clone(),
            batch: batch.clone(),
            version,
            source_url: l.url.clone(),
            source_sha256: l.sha256.clone(),
            ocr_source: l
                .ocr_source
                .clone()
                .unwrap_or_else(|| "ndnp-original".into()),
            status: BatchStatus::Queued,
            attempts: 0,
            lease: None,
            curated: None,
            last_error: None,
            updated_at: Utc::now(),
        };
        let Some((mut b, etag)) = state.batch(&batch).await? else {
            if state.create_batch(&fresh).await? {
                report.new += 1;
                continue;
            }
            bail!("{batch}: created concurrently; run enqueue again");
        };
        let curated_version = b.curated.as_ref().map(|c| c.version);
        if version > b.version || (version == b.version && curated_version != Some(version)) {
            if version > b.version {
                report.new_version += 1;
            } else if matches!(b.status, BatchStatus::Failed) {
                report.requeued += 1;
            } else {
                report.unchanged += 1;
                continue; // already queued or being curated
            }
            b.version = version;
            b.source_url = fresh.source_url;
            b.source_sha256 = fresh.source_sha256;
            b.ocr_source = fresh.ocr_source;
            b.status = BatchStatus::Queued;
            b.attempts = 0;
            b.last_error = None;
            b.updated_at = Utc::now();
            if state.replace_batch(&b, &etag).await?.is_none() {
                bail!("{batch}: changed concurrently; run enqueue again");
            }
        } else {
            report.unchanged += 1;
        }
    }
    Ok(report)
}

/// Fetch `url` (https, `file://` or a local path) into `dest`, returning its
/// sha256. Retries 429 and 5xx with backoff, honoring `Retry-After`.
pub async fn fetch(url: &str, dest: &Path) -> anyhow::Result<String> {
    if let Some(path) = url
        .strip_prefix("file://")
        .or_else(|| (!url.contains("://")).then_some(url))
    {
        let src = path.to_owned();
        let dest = dest.to_owned();
        return tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let mut input = std::fs::File::open(&src).with_context(|| src.clone())?;
            let mut out = std::fs::File::create(&dest)?;
            let mut hasher = HashingWriter {
                inner: &mut out,
                hash: Sha256::new(),
            };
            std::io::copy(&mut input, &mut hasher)?;
            Ok(hex(&hasher.hash.finalize()))
        })
        .await?;
    }
    if !url.starts_with("https://") {
        bail!("batch source `{url}` must be https, file:// or a local path");
    }
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()?;
    let mut delay = Duration::from_secs(5);
    for attempt in 1..=6 {
        let resp = client.get(url).send().await;
        let retry_after = match &resp {
            Ok(r) if r.status().is_success() => None,
            Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => Some(
                r.headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map_or(delay, Duration::from_secs),
            ),
            Ok(r) => bail!("{url} returned {}", r.status()),
            Err(_) => Some(delay),
        };
        if let Some(wait) = retry_after {
            if attempt == 6 {
                bail!("{url}: giving up after {attempt} attempts");
            }
            tracing::warn!(
                url,
                attempt,
                wait_secs = wait.as_secs(),
                "fetch failed; backing off"
            );
            tokio::time::sleep(wait.min(Duration::from_secs(600))).await;
            delay *= 2;
            continue;
        }
        let mut stream = resp.expect("checked").bytes_stream();
        let mut out = std::fs::File::create(dest)?;
        let mut hash = Sha256::new();
        let mut ok = true;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    hash.update(&c);
                    out.write_all(&c)?;
                }
                Err(e) => {
                    tracing::warn!(url, error = %e, "download interrupted; retrying");
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            out.sync_all()?;
            return Ok(hex(&hash.finalize()));
        }
        tokio::time::sleep(delay).await;
        delay *= 2;
    }
    bail!("{url}: giving up")
}

struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hash: Sha256,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::MemoryDocs;
    use std::sync::Arc;

    fn listed(name: &str) -> ListedBatch {
        ListedBatch {
            name: name.into(),
            url: format!("https://example.org/{name}.tar.bz2"),
            sha256: None,
            ocr_source: None,
        }
    }

    #[test]
    fn batch_names() {
        assert_eq!(
            split_version("batch_az_acacia_ver02").unwrap(),
            ("batch_az_acacia".into(), 2)
        );
        assert!(split_version("batch_az_acacia").is_err());
        assert!(split_version("batch_az_acacia_ver00").is_err());
        assert!(split_version("../x_ver01").is_err());
    }

    #[tokio::test]
    async fn enqueue_queues_new_batches_and_versions_once() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        let r = enqueue(&s, &[listed("batch_a_ver01"), listed("batch_b_ver01")])
            .await
            .unwrap();
        assert_eq!(r.new, 2);
        let r = enqueue(&s, &[listed("batch_a_ver01")]).await.unwrap();
        assert_eq!(r.unchanged, 1);
        let r = enqueue(&s, &[listed("batch_a_ver02")]).await.unwrap();
        assert_eq!(r.new_version, 1);
        let (b, _) = s.batch("batch_a").await.unwrap().unwrap();
        assert_eq!((b.version, b.status), (2, BatchStatus::Queued));
        // An older listing never downgrades.
        let r = enqueue(&s, &[listed("batch_a_ver01")]).await.unwrap();
        assert_eq!(r.unchanged, 1);
        assert_eq!(s.batch("batch_a").await.unwrap().unwrap().0.version, 2);
    }

    #[tokio::test]
    async fn fetches_local_files_with_their_hash() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.bin");
        std::fs::write(&src, b"abc").unwrap();
        let dest = dir.path().join("b.bin");
        let sha = fetch(src.to_str().unwrap(), &dest).await.unwrap();
        assert_eq!(
            sha,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(std::fs::read(dest).unwrap(), b"abc");
        assert!(fetch("http://example.org/x", &dir.path().join("c"))
            .await
            .is_err());
    }
}
