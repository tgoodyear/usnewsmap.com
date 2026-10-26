//! Resumable, create-only Azure block uploads.

use super::{error_code, transport, xml_elements, xml_values, BlobStore};
use crate::retry::{retryable_write, retrying};
use crate::{validate_path, StoreError};
use bytes::Bytes;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, IF_NONE_MATCH};
use reqwest::{Method, StatusCode};
use std::collections::HashMap;

/// The `n`th block's id: the first 16 bytes of the block's SHA-256 in hex,
/// then `n`, base64. Azure stages blocks by blob and id, so an id names its
/// bytes: a block an interrupted upload left staged is used again only if
/// it holds what would be sent, and two uploads to one blob at once share
/// an id only for the same bytes at the same place, so a commit can't mix
/// their blocks. 40 bytes before base64, the length of the random-tag ids
/// earlier uploads used: one blob's ids must all have one length, and such
/// an upload may have left blocks behind.
pub(super) fn block_id(block: &[u8], n: usize) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let digest = Sha256::digest(block);
    let mut id = String::with_capacity(40);
    for b in &digest[..16] {
        let _ = write!(id, "{b:02x}");
    }
    let _ = write!(id, "{n:08}");
    base64::engine::general_purpose::STANDARD.encode(id)
}

/// The Put Block List body committing `ids` in order.
fn block_list(ids: &[String]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
    for id in ids {
        xml.push_str("<Latest>");
        xml.push_str(id);
        xml.push_str("</Latest>");
    }
    xml.push_str("</BlockList>");
    xml
}

/// A blob's blocks as Get Block List gives them: ids and sizes, in order.
#[derive(Debug, Default)]
struct BlockLists {
    committed: Vec<(String, u64)>,
    uncommitted: Vec<(String, u64)>,
}

/// A streamed upload's blocks so far, and how many of them Azure held
/// already.
#[derive(Default)]
struct Staging {
    ids: Vec<String>,
    kept: usize,
    kept_bytes: u64,
}

impl BlobStore {
    /// Put Block, sent again after a failure that may pass: staging a block
    /// again under its id replaces it with the same bytes. Each try has a
    /// timeout sized to the block ([`super::Tuning::block_timeout`]).
    async fn put_block(&self, path: &str, id: &str, body: Bytes) -> Result<(), StoreError> {
        let timeout = self.tuning.block_timeout(body.len());
        retrying(
            "put block",
            path,
            self.tuning.write,
            retryable_write,
            |_| {
                let body = body.clone();
                async move {
                    let resp = self
                        .request(Method::PUT, path)
                        .await?
                        .query(&[("comp", "block"), ("blockid", id)])
                        .header(CONTENT_LENGTH, body.len())
                        .timeout(timeout)
                        .body(body)
                        .send()
                        .await
                        .map_err(|e| transport("put block", path, e))?;
                    match resp.status() {
                        StatusCode::CREATED => Ok(()),
                        s => Err(StoreError::Http {
                            op: "put block",
                            path: path.into(),
                            status: s.as_u16(),
                        }),
                    }
                }
            },
        )
        .await
    }

    /// Get Block List (`blocklisttype=all`): `path`'s committed and
    /// uncommitted blocks. Empty when there is no such blob.
    async fn block_lists(&self, path: &str) -> Result<BlockLists, StoreError> {
        retrying(
            "get block list",
            path,
            self.tuning.write,
            retryable_write,
            |_| async move {
                let resp = self
                    .request(Method::GET, path)
                    .await?
                    .query(&[("comp", "blocklist"), ("blocklisttype", "all")])
                    .send()
                    .await
                    .map_err(|e| transport("get block list", path, e))?;
                match resp.status() {
                    StatusCode::NOT_FOUND => return Ok(BlockLists::default()),
                    s if !s.is_success() => {
                        return Err(StoreError::Http {
                            op: "get block list",
                            path: path.into(),
                            status: s.as_u16(),
                        })
                    }
                    _ => {}
                }
                let xml = resp
                    .text()
                    .await
                    .map_err(|e| transport("get block list", path, e))?;
                let section = |name: &str| -> Vec<(String, u64)> {
                    xml_elements(&xml, name)
                        .into_iter()
                        .flat_map(|s| xml_elements(s, "Block"))
                        .filter_map(|b| {
                            let id = xml_values(b, "Name").next()?;
                            let size = xml_values(b, "Size").next()?.trim().parse().ok()?;
                            Some((id, size))
                        })
                        .collect()
                };
                Ok(BlockLists {
                    committed: section("CommittedBlocks"),
                    uncommitted: section("UncommittedBlocks"),
                })
            },
        )
        .await
    }

    /// Put Block List committing `ids`, create-only (`If-None-Match: *`),
    /// sent again after a failure that may pass. When a later try, or an
    /// upload of the same ids after one whose commit is [`BlobStore::unsure`],
    /// finds the blob already there, that commit may have gone through with
    /// only its answer lost: the blob is this upload's if its committed
    /// blocks are `ids`, which name their bytes ([`block_id`]). Never for an
    /// empty upload: a blob written whole (Put Blob) lists no blocks either.
    async fn put_block_list(
        &self,
        path: &str,
        ids: &[String],
        content_type: &str,
        tier: Option<&str>,
    ) -> Result<(), StoreError> {
        let body = Bytes::from(block_list(ids));
        let earlier = self
            .unsure
            .lock()
            .unwrap()
            .get(path)
            .is_some_and(|sent| sent.as_slice() == ids);
        let result = retrying(
            "put block list",
            path,
            self.tuning.write,
            retryable_write,
            |attempt| {
                let body = body.clone();
                async move {
                    let mut req = self
                        .request(Method::PUT, path)
                        .await?
                        .query(&[("comp", "blocklist")])
                        .header(IF_NONE_MATCH, "*")
                        .header("x-ms-blob-content-type", content_type)
                        .header(CONTENT_TYPE, "application/xml");
                    if let Some(t) = tier {
                        req = req.header("x-ms-access-tier", t);
                    }
                    let resp = req
                        .body(body)
                        .send()
                        .await
                        .map_err(|e| transport("put block list", path, e))?;
                    match (resp.status(), error_code(&resp)) {
                        (StatusCode::CREATED, _) => Ok(()),
                        (StatusCode::CONFLICT, "BlobAlreadyExists")
                        | (StatusCode::PRECONDITION_FAILED, "ConditionNotMet") => {
                            if !ids.is_empty() && (attempt > 1 || earlier) {
                                let committed = self.block_lists(path).await?.committed;
                                if committed.len() == ids.len()
                                    && committed.iter().zip(ids).all(|((c, _), id)| c == id)
                                {
                                    tracing::warn!(
                                        path,
                                        attempt,
                                        "an earlier commit went through; its answer was lost"
                                    );
                                    return Ok(());
                                }
                            }
                            Err(StoreError::AlreadyExists(path.into()))
                        }
                        (s, _) => Err(StoreError::Http {
                            op: "put block list",
                            path: path.into(),
                            status: s.as_u16(),
                        }),
                    }
                }
            },
        )
        .await;
        let mut unsure = self.unsure.lock().unwrap();
        match &result {
            Err(e) if retryable_write(e) => {
                unsure.insert(path.to_owned(), ids.to_vec());
            }
            _ => {
                unsure.remove(path);
            }
        }
        result
    }

    /// Stage the next block of the upload `up` to `path`, unless `held`
    /// (the blocks Azure holds for the path already) has it.
    async fn stage(
        &self,
        path: &str,
        block: Vec<u8>,
        held: &HashMap<String, u64>,
        up: &mut Staging,
    ) -> Result<(), StoreError> {
        let n = up.ids.len();
        // Hashing 8 MiB takes milliseconds: off the async threads.
        let (id, block) = tokio::task::spawn_blocking(move || (block_id(&block, n), block))
            .await
            .map_err(|e| StoreError::Io(format!("hashing a block of `{path}`: {e}")))?;
        let len = block.len() as u64;
        if held.get(&id) == Some(&len) {
            up.kept += 1;
            up.kept_bytes += len;
        } else {
            self.put_block(path, &id, Bytes::from(block)).await?;
        }
        up.ids.push(id);
        Ok(())
    }
    pub(super) async fn upload_stream(
        &self,
        path: &str,
        content_type: &str,
        tier: Option<&str>,
        mut chunks: tokio::sync::mpsc::Receiver<bytes::Bytes>,
        commit: tokio::sync::oneshot::Receiver<bool>,
    ) -> Result<u64, StoreError> {
        validate_path(path)?;
        let block_bytes = self.tuning.block_bytes;
        // Only an upload sent before can be resumed, so a failed listing
        // costs nothing but the blocks being sent again. Committed blocks
        // count too: if the blob is there, the create-only commit fails
        // anyway, unless it is this process's own commit whose answer was
        // lost ([`BlobStore::unsure`]), which then isn't sent twice.
        let held: HashMap<String, u64> = match self.block_lists(path).await {
            Ok(l) => l.committed.into_iter().chain(l.uncommitted).collect(),
            Err(e) => {
                tracing::warn!(path, error = %e, "blocks not listed; sending every block");
                HashMap::new()
            }
        };
        let mut up = Staging::default();
        let mut buf: Vec<u8> = Vec::with_capacity(block_bytes);
        let mut total = 0u64;
        while let Some(c) = chunks.recv().await {
            total += c.len() as u64;
            let mut rest = &c[..];
            while !rest.is_empty() {
                let take = (block_bytes - buf.len()).min(rest.len());
                buf.extend_from_slice(&rest[..take]);
                rest = &rest[take..];
                if buf.len() == block_bytes {
                    let block = std::mem::replace(&mut buf, Vec::with_capacity(block_bytes));
                    self.stage(path, block, &held, &mut up).await?;
                }
            }
        }
        if !buf.is_empty() {
            self.stage(path, buf, &held, &mut up).await?;
        }
        if up.kept > 0 {
            tracing::info!(
                path,
                blocks = up.kept,
                of = up.ids.len(),
                mb = up.kept_bytes / 1_000_000,
                "upload resumed: blocks Azure held already were not sent again"
            );
        }
        if commit.await != Ok(true) {
            return Err(StoreError::Io(format!(
                "`{path}`: upload abandoned; its blocks were not committed"
            )));
        }
        self.put_block_list(path, &up.ids, content_type, tier)
            .await?;
        Ok(total)
    }
}
