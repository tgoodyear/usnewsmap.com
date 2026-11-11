//! Copy pipeline documents from Cosmos DB into PostgreSQL (`usnm_state.documents`, #1367), and
//! check that the two sides hold the same documents.
//!
//! Each document keeps its container, partition key value, id, JSON (without the `_` system
//! properties, as [`crate::cosmos::CosmosDocs`] reads it) and the ETag Cosmos gave it, as
//! `scripts/onprem/queue/documents.sql` expects. A copy is safe to run again: rows are keyed by
//! (container, partition key, id), and a row already there is rewritten only when its ETag is
//! not the source's. Documents deleted from the source after an earlier copy are not deleted
//! from the target; [`verify`] reports them.
//!
//! Source reads are paged (`x-ms-max-item-count`) and paced to a request-unit budget. Nothing
//! here changes the account's throughput.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::docs::Versioned;
use crate::state::{BATCHES, INDEX_RUNS, ISSUES, OPS};

/// The title records' container (titles-sync).
pub const TITLES: &str = "titles";

/// Every container of the `usnm` database and the field its partition key is, as
/// `infra/modules/cosmos.bicep` creates them.
pub const CONTAINERS: [(&str, &str); 5] = [
    (BATCHES, "batch"),
    (INDEX_RUNS, "index_version"),
    (ISSUES, "lccn"),
    (OPS, "kind"),
    (TITLES, "lccn"),
];

/// How many differences per container a verification lists.
pub const LISTED_DIFFERENCES: usize = 10;

/// One page of source documents.
pub struct Page {
    pub docs: Vec<Versioned>,
    pub continuation: Option<String>,
}

/// Where documents come from: Cosmos DB.
#[async_trait]
pub trait Source: Send + Sync {
    /// Every container, with its partition key path (`/batch`).
    async fn containers(&self) -> anyhow::Result<BTreeMap<String, String>>;
    /// The page after `continuation` (the first page for `None`), at most `max_items` long.
    async fn page(
        &self,
        container: &str,
        continuation: Option<&str>,
        max_items: u32,
    ) -> anyhow::Result<Page>;
    /// Request units used so far.
    fn request_charge(&self) -> f64;
}

/// A document as the target stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub pk: String,
    pub id: String,
    pub doc: Value,
    pub etag: String,
}

/// What one import did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Imported {
    pub inserted: u64,
    pub updated: u64,
    pub unchanged: u64,
}

impl Imported {
    fn add(&mut self, other: Imported) {
        self.inserted += other.inserted;
        self.updated += other.updated;
        self.unchanged += other.unchanged;
    }
}

/// Where documents go: PostgreSQL.
#[async_trait]
pub trait Target: Send + Sync {
    /// Insert rows not there yet and rewrite rows whose ETag differs, in one statement.
    async fn import(&self, container: &str, rows: &[Row]) -> anyhow::Result<Imported>;
    /// Every row of `container`, as digest entries, in any order.
    async fn entries(&self, container: &str) -> anyhow::Result<Vec<Entry>>;
}

/// A document's place in a container digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub pk: String,
    pub id: String,
    pub etag: String,
    /// SHA-256 of the document's [`canonical`] JSON.
    pub doc_sha256: [u8; 32],
}

impl Entry {
    pub fn new(pk: &str, id: &str, etag: &str, doc: &Value) -> Self {
        Self {
            pk: pk.to_owned(),
            id: id.to_owned(),
            etag: etag.to_owned(),
            doc_sha256: Sha256::digest(canonical(doc).as_bytes()).into(),
        }
    }

    fn key(&self) -> (&str, &str) {
        (&self.pk, &self.id)
    }
}

/// JSON with every object's keys in byte order and no whitespace, so the same document hashes
/// the same whichever store it was read from (JSONB keeps its own key order).
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// A container's count and digest: SHA-256 over its entries sorted by (partition key, id), each
/// written as the length-prefixed partition key, id and ETag and then the document's hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Side {
    pub count: u64,
    pub sha256: String,
}

pub fn digest(entries: &mut [Entry]) -> Side {
    entries.sort_by(|a, b| a.key().cmp(&b.key()));
    let mut h = Sha256::new();
    for e in entries.iter() {
        for part in [&e.pk, &e.id, &e.etag] {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
        h.update(e.doc_sha256);
    }
    Side {
        count: entries.len() as u64,
        sha256: hex(&h.finalize()),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One document that differs between the sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Difference {
    pub pk: String,
    pub id: String,
    /// `missing_in_target`, `missing_in_source`, `etag` or `document`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_etag: Option<String>,
}

/// How many entries differ between two sorted lists, and the first `limit` of them.
pub fn differences(source: &[Entry], target: &[Entry], limit: usize) -> (u64, Vec<Difference>) {
    let (mut i, mut j, mut n) = (0, 0, 0u64);
    let mut listed = Vec::new();
    let mut note = |d: Difference| {
        n += 1;
        if listed.len() < limit {
            listed.push(d);
        }
    };
    while i < source.len() || j < target.len() {
        let order = match (source.get(i), target.get(j)) {
            (Some(s), Some(t)) => s.key().cmp(&t.key()),
            (Some(_), None) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Greater,
        };
        match order {
            std::cmp::Ordering::Less => {
                let s = &source[i];
                note(Difference {
                    pk: s.pk.clone(),
                    id: s.id.clone(),
                    kind: "missing_in_target",
                    source_etag: Some(s.etag.clone()),
                    target_etag: None,
                });
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                let t = &target[j];
                note(Difference {
                    pk: t.pk.clone(),
                    id: t.id.clone(),
                    kind: "missing_in_source",
                    source_etag: None,
                    target_etag: Some(t.etag.clone()),
                });
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let (s, t) = (&source[i], &target[j]);
                let kind = if s.etag != t.etag {
                    Some("etag")
                } else if s.doc_sha256 != t.doc_sha256 {
                    Some("document")
                } else {
                    None
                };
                if let Some(kind) = kind {
                    note(Difference {
                        pk: s.pk.clone(),
                        id: s.id.clone(),
                        kind,
                        source_etag: Some(s.etag.clone()),
                        target_etag: Some(t.etag.clone()),
                    });
                }
                i += 1;
                j += 1;
            }
        }
    }
    (n, listed)
}

/// How to read the source.
#[derive(Debug, Clone)]
pub struct Options {
    /// The containers to take; empty for all of [`CONTAINERS`].
    pub containers: Vec<String>,
    /// Documents per source page.
    pub page_size: u32,
    /// Request units per second to stay under, on average; `None` for no pacing.
    pub max_ru_per_sec: Option<f64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            containers: Vec::new(),
            page_size: 100,
            max_ru_per_sec: Some(200.0),
        }
    }
}

/// One container's part of a [`Receipt`].
#[derive(Debug, Clone, Serialize)]
pub struct ContainerReceipt {
    pub name: String,
    pub partition_key: String,
    pub source: Side,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported: Option<Imported>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Side>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub differences: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub first_differences: Vec<Difference>,
    pub request_charge: f64,
}

/// What a copy or a verification did.
#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    /// `copy` or `verify`.
    pub mode: &'static str,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub containers: Vec<ContainerReceipt>,
    pub request_charge: f64,
    /// Verification: every container matched. A copy: always true.
    pub ok: bool,
}

/// The containers to take and each one's partition key field, checked against the source:
/// each must exist there with the partition key [`CONTAINERS`] names, and when all are taken,
/// the source may hold no container this list doesn't name (it would be left behind).
pub async fn plan(source: &dyn Source, wanted: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    let found = source.containers().await?;
    let known: BTreeMap<&str, &str> = CONTAINERS.into_iter().collect();
    if wanted.is_empty() {
        let unknown: Vec<&String> = found
            .keys()
            .filter(|c| !known.contains_key(c.as_str()))
            .collect();
        ensure!(
            unknown.is_empty(),
            "the source has containers this tool doesn't know: {unknown:?}"
        );
    }
    let names: Vec<String> = if wanted.is_empty() {
        known.keys().map(|c| (*c).to_owned()).collect()
    } else {
        wanted.to_vec()
    };
    let mut out = Vec::new();
    for name in names {
        let Some(field) = known.get(name.as_str()) else {
            bail!("unknown container `{name}`; known: {:?}", known.keys());
        };
        let Some(path) = found.get(&name) else {
            bail!("container `{name}` is not in the source");
        };
        ensure!(
            path.as_str() == format!("/{field}"),
            "container `{name}` is partitioned on `{path}`, not `/{field}`"
        );
        if !out.iter().any(|(n, _): &(String, String)| n == &name) {
            out.push((name, (*field).to_owned()));
        }
    }
    Ok(out)
}

/// A source document as a row: its partition key value is the string at `field`.
pub fn row(container: &str, field: &str, v: Versioned) -> anyhow::Result<Row> {
    let id = v.doc["id"]
        .as_str()
        .with_context(|| format!("a document in `{container}` has no string id"))?
        .to_owned();
    let pk = v.doc[field]
        .as_str()
        .with_context(|| format!("{container}/{id}: partition key `{field}` is not a string"))?
        .to_owned();
    ensure!(!v.etag.is_empty(), "{container}/{id}: no ETag");
    Ok(Row {
        pk,
        id,
        doc: v.doc,
        etag: v.etag,
    })
}

/// Wait long enough that `charge` request units, spent since `since`, keep to `budget` per second.
async fn pace(budget: Option<f64>, charge: f64, since: tokio::time::Instant) {
    let Some(budget) = budget.filter(|b| *b > 0.0) else {
        return;
    };
    let due = Duration::from_secs_f64((charge / budget).clamp(0.0, 60.0));
    let spent = since.elapsed();
    if due > spent {
        tokio::time::sleep(due - spent).await;
    }
}

/// Copy or verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Import every source document into the target.
    Copy,
    /// Write nothing; compare the two sides.
    Verify,
}

/// Copy every document of the planned containers from `source` into `target`, or (with
/// [`Mode::Verify`]) compare the two. `progress` gets a line after each page.
pub async fn run(
    source: &dyn Source,
    target: &dyn Target,
    mode: Mode,
    opts: &Options,
    progress: &(dyn Fn(&str) + Sync),
) -> anyhow::Result<Receipt> {
    ensure!(
        (1..=1000).contains(&opts.page_size),
        "page size must be 1 to 1000"
    );
    let planned = plan(source, &opts.containers).await?;
    run_planned(source, target, mode, &planned, opts, progress).await
}

/// [`run`] over containers already planned: (name, partition key field) pairs.
pub async fn run_planned(
    source: &dyn Source,
    target: &dyn Target,
    mode: Mode,
    planned: &[(String, String)],
    opts: &Options,
    progress: &(dyn Fn(&str) + Sync),
) -> anyhow::Result<Receipt> {
    let started_at = Utc::now();
    let since = tokio::time::Instant::now();
    let charge_at_start = source.request_charge();
    let mut containers = Vec::new();
    for (name, field) in planned {
        let (name, field) = (name.clone(), field.clone());
        let charge_before = source.request_charge();
        let mut entries = Vec::new();
        let mut imported = Imported::default();
        let mut continuation: Option<String> = None;
        loop {
            let page = source
                .page(&name, continuation.as_deref(), opts.page_size)
                .await
                .with_context(|| format!("reading `{name}`"))?;
            let rows = page
                .docs
                .into_iter()
                .map(|v| row(&name, &field, v))
                .collect::<anyhow::Result<Vec<_>>>()?;
            entries.extend(
                rows.iter()
                    .map(|r| Entry::new(&r.pk, &r.id, &r.etag, &r.doc)),
            );
            if mode == Mode::Copy && !rows.is_empty() {
                imported.add(
                    target
                        .import(&name, &rows)
                        .await
                        .with_context(|| format!("importing into `{name}`"))?,
                );
            }
            let charge = source.request_charge() - charge_before;
            progress(&match mode {
                Mode::Copy => format!(
                    "{name}: {} read, {} inserted, {} updated, {} unchanged, {charge:.1} RU",
                    entries.len(),
                    imported.inserted,
                    imported.updated,
                    imported.unchanged
                ),
                Mode::Verify => format!("{name}: {} read, {charge:.1} RU", entries.len()),
            });
            pace(
                opts.max_ru_per_sec,
                source.request_charge() - charge_at_start,
                since,
            )
            .await;
            continuation = page.continuation;
            if continuation.is_none() {
                break;
            }
        }
        let source_side = digest(&mut entries);
        let mut receipt = ContainerReceipt {
            name: name.clone(),
            partition_key: format!("/{field}"),
            source: source_side,
            imported: (mode == Mode::Copy).then_some(imported),
            target: None,
            differences: None,
            first_differences: Vec::new(),
            request_charge: source.request_charge() - charge_before,
        };
        if mode == Mode::Verify {
            let mut theirs = target
                .entries(&name)
                .await
                .with_context(|| format!("reading `{name}` from the target"))?;
            let target_side = digest(&mut theirs);
            let (n, listed) = differences(&entries, &theirs, LISTED_DIFFERENCES);
            for d in &listed {
                progress(&format!(
                    "{name}: {} {}/{} (source {:?}, target {:?})",
                    d.kind, d.pk, d.id, d.source_etag, d.target_etag
                ));
            }
            progress(&format!(
                "{name}: source {} {}, target {} {}, {n} differences",
                receipt.source.count, receipt.source.sha256, target_side.count, target_side.sha256
            ));
            receipt.target = Some(target_side);
            receipt.differences = Some(n);
            receipt.first_differences = listed;
        }
        containers.push(receipt);
    }
    let ok = containers.iter().all(|c| {
        c.target
            .as_ref()
            .is_none_or(|t| *t == c.source && c.differences == Some(0))
    });
    Ok(Receipt {
        mode: match mode {
            Mode::Copy => "copy",
            Mode::Verify => "verify",
        },
        started_at,
        finished_at: Utc::now(),
        containers,
        request_charge: source.request_charge() - charge_at_start,
        ok,
    })
}

/// The query each source page comes from.
pub fn page_query() -> Value {
    json!({"query": "SELECT * FROM c", "parameters": []})
}

#[async_trait]
impl Source for crate::cosmos::CosmosDocs {
    async fn containers(&self) -> anyhow::Result<BTreeMap<String, String>> {
        crate::cosmos::CosmosDocs::containers(self).await
    }

    async fn page(
        &self,
        container: &str,
        continuation: Option<&str>,
        max_items: u32,
    ) -> anyhow::Result<Page> {
        let (docs, continuation) = self
            .query_page(container, &page_query(), continuation, Some(max_items))
            .await?;
        Ok(Page {
            docs: docs
                .into_iter()
                .map(|d| {
                    let (doc, etag) = crate::cosmos::clean(d);
                    Versioned { doc, etag }
                })
                .collect(),
            continuation,
        })
    }

    fn request_charge(&self) -> f64 {
        crate::docs::DocStore::request_charge(self).unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A source over fixed documents, `page_size` at a time.
    #[derive(Default)]
    struct MemSource {
        containers: BTreeMap<String, String>,
        docs: BTreeMap<String, Vec<Versioned>>,
        pages: Mutex<u32>,
    }

    impl MemSource {
        fn standard() -> Self {
            let mut s = Self::default();
            for (c, f) in CONTAINERS {
                s.containers.insert(c.into(), format!("/{f}"));
            }
            s
        }

        fn put(&mut self, container: &str, doc: Value, etag: &str) {
            self.docs
                .entry(container.into())
                .or_default()
                .push(Versioned {
                    doc,
                    etag: etag.into(),
                });
        }
    }

    #[async_trait]
    impl Source for MemSource {
        async fn containers(&self) -> anyhow::Result<BTreeMap<String, String>> {
            Ok(self.containers.clone())
        }

        async fn page(
            &self,
            container: &str,
            continuation: Option<&str>,
            max_items: u32,
        ) -> anyhow::Result<Page> {
            *self.pages.lock().unwrap() += 1;
            let all = self.docs.get(container).cloned().unwrap_or_default();
            let from: usize = continuation.map_or(0, |c| c.parse().unwrap());
            let to = (from + max_items as usize).min(all.len());
            Ok(Page {
                docs: all[from..to].to_vec(),
                continuation: (to < all.len()).then(|| to.to_string()),
            })
        }

        fn request_charge(&self) -> f64 {
            f64::from(*self.pages.lock().unwrap()) * 2.5
        }
    }

    /// (container, partition key, id) to (document, ETag).
    type Rows = BTreeMap<(String, String, String), (Value, String)>;

    /// A target keyed like `usnm_state.documents`.
    #[derive(Default)]
    struct MemTarget {
        rows: Mutex<Rows>,
    }

    #[async_trait]
    impl Target for MemTarget {
        async fn import(&self, container: &str, rows: &[Row]) -> anyhow::Result<Imported> {
            let mut held = self.rows.lock().unwrap();
            let mut out = Imported::default();
            for r in rows {
                let key = (container.to_owned(), r.pk.clone(), r.id.clone());
                match held.get(&key) {
                    None => out.inserted += 1,
                    Some((_, etag)) if *etag == r.etag => {
                        out.unchanged += 1;
                        continue;
                    }
                    Some(_) => out.updated += 1,
                }
                held.insert(key, (r.doc.clone(), r.etag.clone()));
            }
            Ok(out)
        }

        async fn entries(&self, container: &str) -> anyhow::Result<Vec<Entry>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|((c, _, _), _)| c == container)
                .map(|((_, pk, id), (doc, etag))| Entry::new(pk, id, etag, doc))
                .collect())
        }
    }

    fn quiet(_: &str) {}

    fn opts(page_size: u32) -> Options {
        Options {
            containers: Vec::new(),
            page_size,
            max_ru_per_sec: None,
        }
    }

    fn source() -> MemSource {
        let mut s = MemSource::standard();
        for i in 0..7 {
            s.put(
                BATCHES,
                json!({"id": format!("b{i}"), "batch": format!("b{i}"), "status": "curated"}),
                &format!("\"e{i}\""),
            );
        }
        s.put(
            OPS,
            json!({"id": "current", "kind": "current", "index_version": "v1"}),
            "\"o1\"",
        );
        s.put(
            TITLES,
            json!({"id": "sn1", "lccn": "sn1", "title": "Café Gazette"}),
            "\"t1\"",
        );
        s
    }

    #[tokio::test]
    async fn copies_every_container_and_reruns_change_nothing() {
        let src = source();
        let dst = MemTarget::default();
        let first = run(&src, &dst, Mode::Copy, &opts(3), &quiet).await.unwrap();
        let batches = &first.containers[0];
        assert_eq!(batches.name, BATCHES);
        assert_eq!(batches.source.count, 7);
        assert_eq!(
            batches.imported,
            Some(Imported {
                inserted: 7,
                updated: 0,
                unchanged: 0
            })
        );
        assert_eq!(first.containers.len(), CONTAINERS.len());
        // Three pages of three for seven batches, one (empty or not) for each other container.
        assert_eq!(*src.pages.lock().unwrap(), 3 + 4);
        assert!(first.ok);
        // The original ETag and the partition key come across.
        let held = dst.rows.lock().unwrap().clone();
        let (doc, etag) = &held[&(OPS.into(), "current".into(), "current".into())];
        assert_eq!(
            (doc["index_version"].as_str(), etag.as_str()),
            (Some("v1"), "\"o1\"")
        );

        let again = run(&src, &dst, Mode::Copy, &opts(3), &quiet).await.unwrap();
        assert_eq!(
            again.containers[0].imported,
            Some(Imported {
                inserted: 0,
                updated: 0,
                unchanged: 7
            })
        );
        assert_eq!(again.containers[0].source, first.containers[0].source);
        let verified = run(&src, &dst, Mode::Verify, &opts(3), &quiet)
            .await
            .unwrap();
        assert!(verified.ok, "{verified:?}");
        assert!(verified.containers.iter().all(|c| c.differences == Some(0)));
    }

    #[tokio::test]
    async fn a_resumed_copy_updates_only_changed_etags() {
        let mut src = source();
        let dst = MemTarget::default();
        // A partial first run: only the batches.
        let partial = Options {
            containers: vec![BATCHES.into()],
            ..opts(4)
        };
        run(&src, &dst, Mode::Copy, &partial, &quiet).await.unwrap();
        // One batch changed at the source since.
        src.docs.get_mut(BATCHES).unwrap()[2] = Versioned {
            doc: json!({"id": "b2", "batch": "b2", "status": "released"}),
            etag: "\"e2b\"".into(),
        };
        let second = run(&src, &dst, Mode::Copy, &opts(4), &quiet).await.unwrap();
        assert_eq!(
            second.containers[0].imported,
            Some(Imported {
                inserted: 0,
                updated: 1,
                unchanged: 6
            })
        );
        assert_eq!(
            second.containers[3].imported.map(|i| i.inserted),
            Some(1),
            "{:?}",
            second.containers[3]
        );
        assert!(
            run(&src, &dst, Mode::Verify, &opts(4), &quiet)
                .await
                .unwrap()
                .ok
        );
    }

    #[tokio::test]
    async fn verify_names_the_first_differences() {
        let src = source();
        let dst = MemTarget::default();
        run(&src, &dst, Mode::Copy, &opts(100), &quiet)
            .await
            .unwrap();
        {
            let mut rows = dst.rows.lock().unwrap();
            // Same ETag, different body; a different ETag; one gone; one extra.
            rows.get_mut(&(BATCHES.into(), "b1".into(), "b1".into()))
                .unwrap()
                .0["status"] = json!("queued");
            rows.get_mut(&(BATCHES.into(), "b3".into(), "b3".into()))
                .unwrap()
                .1 = "pg-9".into();
            rows.remove(&(BATCHES.into(), "b5".into(), "b5".into()));
            rows.insert(
                (BATCHES.into(), "zz".into(), "zz".into()),
                (json!({"id": "zz", "batch": "zz"}), "pg-1".into()),
            );
        }
        let lines = Mutex::new(Vec::new());
        let log = |l: &str| lines.lock().unwrap().push(l.to_owned());
        let r = run(&src, &dst, Mode::Verify, &opts(100), &log)
            .await
            .unwrap();
        assert!(!r.ok);
        let b = &r.containers[0];
        assert_eq!(b.differences, Some(4));
        let kinds: Vec<(&str, &str)> = b
            .first_differences
            .iter()
            .map(|d| (d.id.as_str(), d.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("b1", "document"),
                ("b3", "etag"),
                ("b5", "missing_in_target"),
                ("zz", "missing_in_source")
            ]
        );
        assert_ne!(b.source.sha256, b.target.as_ref().unwrap().sha256);
        assert!(r.containers[1..].iter().all(|c| c.differences == Some(0)));
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("etag b3/b3")));
        // Nothing was written.
        assert!(dst
            .rows
            .lock()
            .unwrap()
            .contains_key(&(BATCHES.into(), "zz".into(), "zz".into())));
    }

    #[tokio::test]
    async fn the_plan_follows_the_source_containers() {
        let dst = MemTarget::default();
        let mut extra = source();
        extra.containers.insert("leases".into(), "/id".into());
        let err = run(&extra, &dst, Mode::Copy, &opts(10), &quiet)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("leases"), "{err}");
        // Naming containers leaves an unknown one alone.
        let only = Options {
            containers: vec![OPS.into(), OPS.into()],
            ..opts(10)
        };
        let r = run(&extra, &dst, Mode::Copy, &only, &quiet).await.unwrap();
        assert_eq!(r.containers.len(), 1);
        let unknown = Options {
            containers: vec!["leases".into()],
            ..opts(10)
        };
        assert!(run(&extra, &dst, Mode::Copy, &unknown, &quiet)
            .await
            .is_err());
        let mut moved = source();
        moved.containers.insert(OPS.into(), "/id".into());
        let err = run(&moved, &dst, Mode::Copy, &opts(10), &quiet)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("partitioned"), "{err}");
        let mut missing = source();
        missing.containers.remove(TITLES);
        assert!(run(&missing, &dst, Mode::Copy, &opts(10), &quiet)
            .await
            .is_err());
        assert!(run(&source(), &dst, Mode::Copy, &opts(0), &quiet)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_document_without_its_partition_key_stops_the_copy() {
        let mut src = source();
        src.put(ISSUES, json!({"id": "x"}), "\"i\"");
        let err = run(&src, &MemTarget::default(), Mode::Copy, &opts(10), &quiet)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("issues/x"), "{err:#}");
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let a = json!({"b": [{"z": 1, "a": "é"}], "a": null, "n": 1.5});
        assert_eq!(canonical(&a), r#"{"a":null,"b":[{"a":"é","z":1}],"n":1.5}"#);
        let b: Value = serde_json::from_str(r#"{"n":1.5,"a":null,"b":[{"a":"é","z":1}]}"#).unwrap();
        assert_eq!(canonical(&a), canonical(&b));
    }

    #[test]
    fn the_digest_ignores_order_but_not_content() {
        let e = |id: &str, etag: &str| Entry::new("p", id, etag, &json!({"id": id}));
        let mut one = vec![e("a", "1"), e("b", "2")];
        let mut two = vec![e("b", "2"), e("a", "1")];
        assert_eq!(digest(&mut one), digest(&mut two));
        let mut three = vec![e("b", "3"), e("a", "1")];
        assert_ne!(digest(&mut one).sha256, digest(&mut three).sha256);
        // Length prefixes: moving a character between fields changes the digest.
        let mut x = vec![Entry::new("ab", "c", "1", &json!({}))];
        let mut y = vec![Entry::new("a", "bc", "1", &json!({}))];
        assert_ne!(digest(&mut x).sha256, digest(&mut y).sha256);
    }

    /// The container list matches the one Bicep creates.
    #[test]
    fn the_containers_are_the_ones_the_infrastructure_creates() {
        let bicep = include_str!("../../../infra/modules/cosmos.bicep");
        let mut declared: Vec<(String, String)> = bicep
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                let name = l.strip_prefix("{ name: '")?.split('\'').next()?;
                let key = l.split("key: '/").nth(1)?.split('\'').next()?;
                Some((name.to_owned(), key.to_owned()))
            })
            .collect();
        declared.sort();
        let ours: Vec<(String, String)> = CONTAINERS
            .iter()
            .map(|(c, f)| ((*c).to_owned(), (*f).to_owned()))
            .collect();
        assert_eq!(declared, ours);
    }

    #[tokio::test]
    async fn pacing_waits_for_the_budget() {
        let start = tokio::time::Instant::now();
        pace(Some(1000.0), 50.0, start).await;
        assert!(start.elapsed() >= Duration::from_millis(50));
        let start = tokio::time::Instant::now();
        pace(None, 1e9, start).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
