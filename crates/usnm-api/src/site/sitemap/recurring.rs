//! Only finalized anonymous days contribute; never staging batches or raw text
//! in telemetry. Filter combinations count separately and are never published.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::NaiveDate;
use usnm_core::params::RawParams;
use usnm_store::ObjectStore;

use super::{backend, Result};
use crate::searchlog::{Record, FORMAT};
use crate::Snapshot;

const MAX_DAY_BYTES: usize = 32 * 1024 * 1024;
const MAX_DISTINCT: usize = 50_000;

fn identity(record: &Record, snap: &Snapshot) -> Option<(String, String)> {
    if record.v != FORMAT || record.source != "api" {
        return None;
    }
    let canonical =
        crate::searchlog::canonical(record, snap.refdata.bounds(), snap.refdata.analyzers())?;
    let mut recorded = snap
        .refdata
        .search_request(&RawParams::parse(&canonical).ok()?)
        .ok()?;
    let path = super::super::search_path(&record.query);
    if !super::valid_url(&path) {
        return None;
    }
    let query = super::super::search_query(&path)?;
    let base = snap
        .refdata
        .search_request(&RawParams::parse(&super::super::preview_query(&query)).ok()?)
        .ok()?;
    // Filters count separately, but only the unfiltered base URL is published.
    // Bucket choice changes presentation, not which pages match.
    recorded.bucket = base.bucket;
    (recorded.query == base.query && recorded.source == base.source)
        .then(|| (path, recorded.canonical()))
}

pub(super) async fn candidates(
    store: &dyn ObjectStore,
    snap: &Arc<Snapshot>,
    today: NaiveDate,
) -> Result<BTreeSet<String>> {
    let mut days: BTreeMap<(String, String), BTreeSet<NaiveDate>> = BTreeMap::new();
    let mut objects = store.list_info("days").await.map_err(backend)?;
    objects.sort_by(|a, b| a.path.cmp(&b.path));
    for object in objects {
        let file = object.path;
        let Some(day) = file
            .strip_prefix("days/")
            .and_then(|p| p.strip_suffix(".jsonl"))
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .filter(|day| *day < today)
        else {
            continue;
        };
        if object.size > MAX_DAY_BYTES as u64 {
            return Err(backend("finalized search day exceeds sitemap scan limit"));
        }
        let body = store
            .get_if_match(&file, &object.etag, MAX_DAY_BYTES as u64)
            .await
            .map_err(backend)?
            .ok_or_else(|| backend("finalized search day disappeared or changed"))?;
        let snap = snap.clone();
        let terms = tokio::task::spawn_blocking(move || {
            let mut terms = BTreeSet::new();
            for line in body
                .split(|b| *b == b'\n')
                .filter(|line| !line.trim_ascii().is_empty())
            {
                let record: Record = serde_json::from_slice(line)
                    .map_err(|_| backend(format!("invalid search log record in {file}")))?;
                if record.day != day {
                    return Err(backend(format!("search log day mismatch in {file}")));
                }
                if let Some(key) = identity(&record, &snap) {
                    terms.insert(key);
                    if terms.len() > MAX_DISTINCT {
                        return Err(backend("distinct searches exceed sitemap scan limit"));
                    }
                }
            }
            Ok(terms)
        })
        .await
        .map_err(backend)??;
        for path in terms {
            if !days.contains_key(&path) && days.len() >= MAX_DISTINCT {
                return Err(backend("distinct searches exceed sitemap scan limit"));
            }
            let seen = days.entry(path).or_default();
            if seen.len() < 2 {
                seen.insert(day);
            }
        }
    }
    Ok(days
        .into_iter()
        .filter_map(|((path, _), days)| (days.len() >= 2).then_some(path))
        .collect())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use usnm_store::LocalStore;

    pub(crate) fn record(snap: &Snapshot, day: NaiveDate, query: &str) -> Record {
        let (from, to) = snap.refdata.bounds();
        Record {
            v: FORMAT,
            day,
            source: "api".into(),
            q: query.into(),
            query: query.into(),
            mode: None,
            near: None,
            fuzzy: 0,
            from,
            to,
            state: vec![],
            lccn: vec![],
            lang: vec![],
            front: false,
            corpus: None,
            series: vec![],
            bucket: "year".into(),
            pages: Some(51),
            index_version: Some(snap.refdata.version().into()),
        }
    }

    pub(crate) async fn write(store: &dyn ObjectStore, day: NaiveDate, records: &[Record]) {
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend(serde_json::to_vec(record).unwrap());
            bytes.push(b'\n');
        }
        store
            .put(&format!("days/{day}.jsonl"), bytes, "application/x-ndjson")
            .await
            .unwrap();
    }

    async fn snapshot() -> Arc<Snapshot> {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data");
        Arc::new(Snapshot {
            refdata: crate::refdata::RefData::load(&LocalStore::new(fixtures))
                .await
                .unwrap(),
            backend: Arc::new(usnm_search::memory::MemoryBackend::new()),
            congress_dropped: false,
        })
    }

    #[tokio::test]
    async fn distinct_finalized_days_and_canonical_query_semantics_are_required() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let snap = snapshot().await;
        let one = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let two = one.succ_opt().unwrap();
        let today = two.succ_opt().unwrap();
        let first = record(&snap, one, "\"Cobalt Jubilee\"");
        let same_day = record(&snap, one, "\"cobalt jubilee\"");
        write(&store, one, &[first, same_day]).await;
        assert!(candidates(&store, &snap, today).await.unwrap().is_empty());
        let second = record(&snap, two, "\"cobalt jubilee\"");
        write(&store, two, std::slice::from_ref(&second)).await;
        assert_eq!(
            candidates(&store, &snap, today).await.unwrap(),
            BTreeSet::from(["/search/cobalt-jubilee".into()])
        );
        assert!(
            candidates(&store, &snap, two).await.unwrap().is_empty(),
            "today is never consumed"
        );

        for variant in ["filter", "import", "version", "source", "phrase-vs-and"] {
            write(&store, one, &[record(&snap, one, "\"cobalt jubilee\"")]).await;
            let mut other = second.clone();
            match variant {
                "filter" => other.state.push("NY".into()),
                "import" => other.source = "import".into(),
                "version" => other.v += 1,
                "source" => other.corpus = Some("congress".into()),
                _ => other.query = "cobalt AND jubilee".into(),
            }
            write(&store, two, &[other]).await;
            assert!(
                candidates(&store, &snap, today).await.unwrap().is_empty(),
                "{variant}"
            );
        }
        let mut filtered_one = record(&snap, one, "\"cobalt jubilee\"");
        filtered_one.state.push("NY".into());
        filtered_one.to = filtered_one.from;
        let mut filtered_two = filtered_one.clone();
        filtered_two.day = two;
        write(&store, one, &[filtered_one]).await;
        write(&store, two, std::slice::from_ref(&filtered_two)).await;
        assert_eq!(
            candidates(&store, &snap, today).await.unwrap(),
            BTreeSet::from(["/search/cobalt-jubilee".into()]),
            "a repeated filtered search promotes only its base term"
        );
        filtered_two.state = vec!["CA".into()];
        write(&store, two, &[filtered_two]).await;
        assert!(
            candidates(&store, &snap, today).await.unwrap().is_empty(),
            "different filter combinations cannot pool their days"
        );
        // Staging is not a second day of evidence.
        store
            .put(
                "staging/2026-10-02/batch.jsonl",
                serde_json::to_vec(&second).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        store
            .put("days/2026-10-02.jsonl", Vec::new(), "application/json")
            .await
            .unwrap();
        assert!(candidates(&store, &snap, today).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn encoded_unicode_urls_must_be_shorter_than_2048_characters() {
        let snap = snapshot().await;
        let day = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let words = (0..7)
            .map(|i| "\u{0e01}".repeat(if i == 6 { 31 } else { 32 }))
            .collect::<Vec<_>>()
            .join(" ");
        for (tail, length, admitted) in [
            ("aaaa", 2047, true),
            ("aaaaa", 2048, false),
            ("aaaaaa", 2049, false),
        ] {
            let query = format!("\"{words} {tail}\"");
            let path = super::super::super::search_path(&query);
            assert_eq!(super::super::ORIGIN.len() + path.len(), length);
            assert_eq!(
                identity(&record(&snap, day, &query), &snap).is_some(),
                admitted
            );
        }
    }

    #[tokio::test]
    async fn malformed_logs_fail_explicitly_without_echoing_search_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let snap = snapshot().await;
        let day = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        store
            .put(
                "days/2026-10-01.jsonl",
                b"private malformed text".to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        let error = format!(
            "{:?}",
            candidates(&store, &snap, day.succ_opt().unwrap())
                .await
                .unwrap_err()
        );
        assert!(error.contains("invalid search log record"));
        assert!(!error.contains("private malformed text"));
        write(
            &store,
            day,
            &[record(&snap, day.succ_opt().unwrap(), "cobalt")],
        )
        .await;
        assert!(format!(
            "{:?}",
            candidates(&store, &snap, day.succ_opt().unwrap())
                .await
                .unwrap_err()
        )
        .contains("day mismatch"));
    }

    #[tokio::test]
    async fn size_metadata_is_checked_before_read_and_reads_are_conditionally_bounded() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use usnm_store::{ObjectInfo, StoreError};

        #[derive(Debug)]
        struct Store {
            size: u64,
            reads: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl ObjectStore for Store {
            async fn get(&self, _: &str) -> std::result::Result<Option<Vec<u8>>, StoreError> {
                panic!("unbounded reads are forbidden");
            }
            async fn put_new(
                &self,
                _: &str,
                _: Vec<u8>,
                _: &str,
            ) -> std::result::Result<bool, StoreError> {
                unreachable!()
            }
            async fn put(
                &self,
                _: &str,
                _: Vec<u8>,
                _: &str,
            ) -> std::result::Result<(), StoreError> {
                unreachable!()
            }
            async fn list(&self, _: &str) -> std::result::Result<Vec<String>, StoreError> {
                unreachable!()
            }
            async fn list_info(
                &self,
                prefix: &str,
            ) -> std::result::Result<Vec<ObjectInfo>, StoreError> {
                assert_eq!(prefix, "days");
                Ok(vec![ObjectInfo {
                    path: "days/2026-10-01.jsonl".into(),
                    size: self.size,
                    etag: "listed".into(),
                    created: None,
                    content_md5: None,
                }])
            }
            async fn get_if_match(
                &self,
                path: &str,
                etag: &str,
                max_bytes: u64,
            ) -> std::result::Result<Option<Vec<u8>>, StoreError> {
                assert_eq!(etag, "listed");
                assert_eq!(max_bytes, MAX_DAY_BYTES as u64);
                self.reads.fetch_add(1, Ordering::SeqCst);
                // Simulate an oversized read despite a smaller listing.
                Err(StoreError::TooLarge(path.into()))
            }
        }
        let snap = snapshot().await;
        let today = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        for (size, expected_reads) in [(MAX_DAY_BYTES as u64 + 1, 0), (1, 1)] {
            let store = Store {
                size,
                reads: AtomicUsize::new(0),
            };
            assert!(candidates(&store, &snap, today).await.is_err());
            assert_eq!(store.reads.load(Ordering::SeqCst), expected_reads);
        }
    }
}
