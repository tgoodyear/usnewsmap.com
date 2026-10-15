//! Rejected node output, in the exchange account's `ocr-quarantine`
//! container (no node role, a 30-day lifecycle rule; 12 §12.5 Step 8). It
//! never reaches the lake.
//!
//! Each reject is `rejects/<run>/<source path>` (a dead letter's text as
//! `rejects/<run>/deadletter/<message id>.txt`), with a reason record beside it,
//! `<…>.reason.json`, that holds the reason code and where the object came
//! from, never its content. A source over its size limit isn't read, so
//! only its record is written. `ocr-sync --from-quarantine --reason <r>`
//! puts the rejects with that reason back where they came from after a fix,
//! for the next pass to check again. A part's record names the ALTO and runs
//! record kept with it; a replay puts those back before the part, and keeps
//! all of them in quarantine when one can't be put back, so a part never
//! returns without its runs record and passes as one from an older worker.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use usnm_store::queue::{Queue, DEADLETTER};
use usnm_store::ObjectStore;

/// What a reject was.
pub const PART: &str = "part";
pub const ALTO: &str = "alto";
pub const RUNS: &str = "runs";
pub const DEAD_LETTER: &str = "deadletter";

/// The container's one top-level folder.
pub const TOP: &str = "rejects";

/// A reject's reason record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub reason: String,
    /// [`PART`], [`ALTO`], [`RUNS`] or [`DEAD_LETTER`].
    pub kind: String,
    /// The path in `ocr-out`, or the dead letter's message id.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub size: u64,
    /// The source blob's creation time on the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,
    pub run: String,
    pub at: DateTime<Utc>,
    /// Where the content is in `ocr-quarantine`; none when it wasn't read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// For a part: the sources of the ALTO and runs record kept with it,
    /// which a replay needs. Empty in records written before them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companions: Vec<String>,
}

/// One reject to keep.
#[derive(Debug, Clone)]
pub struct Item {
    pub reason: String,
    pub kind: &'static str,
    pub source: String,
    pub etag: Option<String>,
    pub size: u64,
    pub created: Option<DateTime<Utc>>,
    pub content: Option<Vec<u8>>,
    /// For a part, see [`Record::companions`].
    pub companions: Vec<String>,
}

/// Where a reject's content goes: under its source's own path when that
/// fits the store's path rules with the prefix and the record's suffix, or
/// else under the sha256 of the source, so no source name can make the
/// write fail and stop every run.
fn content_path(run: &str, kind: &str, source: &str) -> String {
    let p = if kind == DEAD_LETTER {
        format!("{TOP}/{run}/deadletter/{source}.txt")
    } else {
        format!("{TOP}/{run}/{source}")
    };
    if usnm_store::validate_path(&format!("{p}.reason.json")).is_ok() {
        return p;
    }
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(source.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{TOP}/{run}/hashed/{kind}-{h}")
}

/// Keep `item` in quarantine: its content (if read), then its record.
pub async fn put(quarantine: &dyn ObjectStore, run: &str, item: Item) -> anyhow::Result<Record> {
    let path = content_path(run, item.kind, &item.source);
    let content = match item.content {
        Some(bytes) => {
            // A second reject of the same source in one run keeps the first.
            quarantine
                .put_new(&path, bytes, "application/octet-stream")
                .await
                .with_context(|| format!("quarantine {path}"))?;
            Some(path.clone())
        }
        None => None,
    };
    let record = Record {
        reason: item.reason,
        kind: item.kind.to_owned(),
        source: item.source,
        etag: item.etag,
        size: item.size,
        created: item.created,
        run: run.to_owned(),
        at: Utc::now(),
        content,
        companions: item.companions,
    };
    quarantine
        .put_new(
            &format!("{path}.reason.json"),
            serde_json::to_vec_pretty(&record)?,
            "application/json",
        )
        .await
        .with_context(|| format!("quarantine record {path}"))?;
    Ok(record)
}

/// What a replay did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Replayed {
    pub parts: usize,
    pub alto: usize,
    pub runs: usize,
    pub dead_letters: usize,
    /// Records with no content to put back (over a size limit).
    pub without_content: usize,
    /// Parts kept in quarantine with their companions because one of those
    /// can't be put back: its record or content is gone, or it had none.
    pub incomplete: usize,
    /// Sources already back in `ocr-out` with the same bytes.
    pub already_there: usize,
    /// Sources whose path in `ocr-out` holds other bytes: kept in quarantine.
    pub conflicts: usize,
}

/// A record to replay, with where it is.
struct Entry {
    path: String,
    record: Record,
}

/// Put every reject with `reason` back where it came from, and remove it
/// from quarantine.
///
/// Dead letters, and ALTO and runs records no part names, go one by one.
/// Then each part with its companions: all their contents are read first,
/// the companions are put back before the part (as a node writes them, so
/// a run stopped midway never leaves a part without its runs record), and
/// the part's entries leave quarantine before theirs.
pub async fn replay(
    quarantine: &dyn ObjectStore,
    out: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    reason: &str,
) -> anyhow::Result<Replayed> {
    let mut report = Replayed::default();
    let mut record_paths = Vec::new();
    let mut etags: HashMap<String, String> = HashMap::new();
    for info in quarantine.list_info(TOP).await? {
        if info.path.ends_with(".reason.json") {
            record_paths.push(info.path.clone());
        }
        etags.insert(info.path, info.etag);
    }
    let mut entries = Vec::new();
    for path in record_paths {
        let Some(body) = quarantine.get(&path).await? else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<Record>(&body) else {
            tracing::warn!(record = %path, "unreadable quarantine record; left as it is");
            continue;
        };
        if record.reason == reason {
            entries.push(Entry { path, record });
        }
    }

    // A part's companions: the ALTO and runs records of its run.
    let by_source: HashMap<(&str, &str), usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e.record.kind.as_str(), ALTO | RUNS))
        .map(|(i, e)| ((e.record.run.as_str(), e.record.source.as_str()), i))
        .collect();
    let mut claimed = vec![false; entries.len()];
    let mut groups: Vec<(usize, Vec<Option<usize>>)> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if e.record.kind != PART {
            continue;
        }
        let companions: Vec<Option<usize>> = e
            .record
            .companions
            .iter()
            .map(|s| by_source.get(&(e.record.run.as_str(), s.as_str())).copied())
            .collect();
        for c in companions.iter().flatten() {
            claimed[*c] = true;
        }
        groups.push((i, companions));
    }

    for (i, e) in entries.iter().enumerate() {
        if e.record.kind == PART || claimed[i] {
            continue;
        }
        if e.record.kind == DEAD_LETTER {
            replay_dead_letter(quarantine, queue, &etags, e, reason, &mut report).await?;
        } else if matches!(e.record.kind.as_str(), ALTO | RUNS) {
            replay_group(quarantine, out, &etags, &[e], reason, &mut report).await?;
        }
    }
    for (part, companions) in groups {
        let Some(mut members) = companions
            .iter()
            .map(|c| c.map(|c| &entries[c]))
            .collect::<Option<Vec<&Entry>>>()
        else {
            let source = &entries[part].record.source;
            tracing::warn!(%source, "a companion's quarantine record is gone; part kept in quarantine");
            report.incomplete += 1;
            continue;
        };
        members.push(&entries[part]);
        replay_group(quarantine, out, &etags, &members, reason, &mut report).await?;
    }
    Ok(report)
}

async fn replay_dead_letter(
    quarantine: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    etags: &HashMap<String, String>,
    e: &Entry,
    reason: &str,
    report: &mut Replayed,
) -> anyhow::Result<()> {
    let Some(content) = &e.record.content else {
        report.without_content += 1;
        return Ok(());
    };
    let Some(bytes) = quarantine.get(content).await? else {
        return Ok(());
    };
    let text = String::from_utf8(bytes).context("dead letter isn't UTF-8")?;
    queue.send(DEADLETTER, &text).await?;
    report.dead_letters += 1;
    remove(quarantine, etags, e).await?;
    tracing::info!(source = %e.record.source, kind = %e.record.kind, reason, "replayed from quarantine");
    Ok(())
}

/// Put `members` back in `ocr-out` in order, all or none of them: the last
/// is the part when there is one. Their quarantine entries go only once
/// every one is back, last member first.
async fn replay_group(
    quarantine: &dyn ObjectStore,
    out: &dyn ObjectStore,
    etags: &HashMap<String, String>,
    members: &[&Entry],
    reason: &str,
    report: &mut Replayed,
) -> anyhow::Result<()> {
    let lead = members.last().expect("a group has a member");
    let mut contents = Vec::with_capacity(members.len());
    for e in members {
        let bytes = match &e.record.content {
            Some(c) => quarantine.get(c).await?,
            None => None,
        };
        let Some(bytes) = bytes else {
            if std::ptr::eq(*e, *lead) {
                if e.record.content.is_none() {
                    report.without_content += 1;
                }
            } else {
                tracing::warn!(source = %lead.record.source, companion = %e.record.source, "a companion has no content to put back; part kept in quarantine");
                report.incomplete += 1;
            }
            return Ok(());
        };
        contents.push(bytes);
    }
    for (e, bytes) in members.iter().zip(contents) {
        let ct = if e.record.kind == ALTO {
            "application/xml"
        } else {
            "application/vnd.apache.parquet"
        };
        if out.put_new(&e.record.source, bytes.clone(), ct).await? {
            match e.record.kind.as_str() {
                PART => report.parts += 1,
                ALTO => report.alto += 1,
                _ => report.runs += 1,
            }
        } else if out.get(&e.record.source).await?.as_deref() == Some(bytes.as_slice()) {
            report.already_there += 1;
        } else {
            // Something else is at the source path now: keep the rejects
            // for inspection, and the part out of `ocr-out`.
            tracing::warn!(source = %e.record.source, record = %e.path, "source path holds other bytes; reject kept in quarantine");
            report.conflicts += 1;
            return Ok(());
        }
    }
    for e in members.iter().rev() {
        remove(quarantine, etags, e).await?;
        tracing::info!(source = %e.record.source, kind = %e.record.kind, reason, "replayed from quarantine");
    }
    Ok(())
}

/// Remove a replayed reject's content, then its record.
async fn remove(
    quarantine: &dyn ObjectStore,
    etags: &HashMap<String, String>,
    e: &Entry,
) -> anyhow::Result<()> {
    for p in e.record.content.iter().chain(std::iter::once(&e.path)) {
        if let Some(etag) = etags.get(p.as_str()) {
            quarantine.delete_if_match(p, etag).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_sources_get_a_hashed_path() {
        let short = content_path("r1", PART, "pages/a.parquet");
        assert_eq!(short, "rejects/r1/pages/a.parquet");
        let long = format!("pages/{}", "a/".repeat(500));
        let p = content_path("r1", PART, &long);
        assert!(p.starts_with("rejects/r1/hashed/part-"));
        assert!(usnm_store::validate_path(&format!("{p}.reason.json")).is_ok());
    }
}
