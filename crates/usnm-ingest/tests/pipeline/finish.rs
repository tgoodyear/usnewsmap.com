//! `release-finish` (#283): a release stopped after its count is finished into
//! the version an uninterrupted run of the same inputs publishes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chrono::NaiveDate;
use serde_json::Value;
use usnm_ingest::catalog::{Catalog, PlacePeriod};
use usnm_ingest::release::Release;
use usnm_ingest::sink::JsonlSink;
use usnm_store::{LocalStore, ObjectStore};

use crate::common::{
    curate, env, fixture_pages, garble, listed, local_row, over_decades, prose,
    put_american_stories, put_overlay_part, quick_merges, raw_run, write_archive, Env, JaRow, Page,
    JA_LATIN,
};

thread_local! {
    /// Where this thread's log lines go, while a test logs them (`log_to`).
    static LOG: std::cell::RefCell<Option<Arc<std::sync::Mutex<std::fs::File>>>> =
        const { std::cell::RefCell::new(None) };
}

/// The writer of one log line: this thread's log, or nowhere.
struct ThreadLog(Option<Arc<std::sync::Mutex<std::fs::File>>>);

impl std::io::Write for ThreadLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &self.0 {
            Some(f) => f.lock().unwrap().write_all(buf).map(|()| buf.len()),
            None => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match &self.0 {
            Some(f) => f.lock().unwrap().flush(),
            None => Ok(()),
        }
    }
}

/// Logs this thread's lines to `path` until dropped.
struct LogGuard;

impl Drop for LogGuard {
    fn drop(&mut self) {
        LOG.with(|l| *l.borrow_mut() = None);
    }
}

/// The JSON log lines this thread writes, as the release's own log has them
/// (`usnm_telemetry`'s), at `path` until the guard drops. One subscriber
/// for the whole process, writing to each thread's file: subscribers set
/// per thread can miss lines while other tests' come and go.
fn log_to(path: &Path) -> LogGuard {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .json()
                .with_writer(|| ThreadLog(LOG.with(|l| l.borrow().clone())))
                .with_max_level(tracing::Level::INFO)
                .finish(),
        )
        .expect("no other subscriber");
    });
    let file = std::fs::File::create(path).unwrap();
    LOG.with(|l| *l.borrow_mut() = Some(Arc::new(std::sync::Mutex::new(file))));
    LogGuard
}

/// A sink that stops like a release whose merge wait runs out: the main
/// index's count passes, the wait starts, then gives up. Its other indexes
/// are never reached.
struct StopsMerging {
    inner: JsonlSink,
    index: Option<String>,
}

#[async_trait::async_trait]
impl usnm_ingest::sink::IndexSink for StopsMerging {
    async fn create(
        &mut self,
        index_id: &str,
        layout: usnm_ingest::sink::MainLayout,
    ) -> anyhow::Result<()> {
        self.index = Some(index_id.to_owned());
        self.inner.create(index_id, layout).await
    }
    async fn add(&mut self, doc: &Value) -> anyhow::Result<()> {
        self.inner.add(doc).await
    }
    async fn add_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        self.inner.add_line(line).await
    }
    async fn finish(&mut self, expected: u64) -> anyhow::Result<()> {
        self.inner.finish(expected).await?;
        let index = self.index.clone().unwrap();
        tracing::info!(index = %index, step = "settle", "release merges");
        anyhow::bail!("merges into `{index}` did not finish within 0 s (settle); not publishing")
    }
    fn backend(&self) -> &'static str {
        self.inner.backend()
    }
    fn stats(&self) -> Arc<usnm_ingest::sink::SinkStats> {
        self.inner.stats()
    }
}

/// The inputs of a full release with every feature a finish has to carry
/// over (#283): two batches, American Stories' text, the Japanese overlay
/// with Latin text (a page curation never had, and one whose LoC text it
/// beats) and Japanese pages, and our OCR from `ocr-local` with a
/// revocation (a garbled page and one LoC has no text for take ours).
async fn finish_inputs(e: &Env) {
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (mut early, mut late): (Vec<Page>, Vec<Page>) =
        fixture_pages().into_iter().partition(|p| p.date < split);
    let ours = |v: &[Page]| -> Vec<usize> {
        v.iter()
            .enumerate()
            .filter(|(_, p)| p.lccn == "sn99000001" && !p.text.is_empty())
            .map(|(i, _)| i)
            .collect()
    };
    let sel = ours(&early);
    let clean: Vec<String> = (0..5).map(prose).collect();
    let (a, b, d, j) = (sel[0], sel[1], sel[2], sel[3]);
    early[a].text = garble(&clean[0]);
    early[b].text = clean[1].clone();
    early[d].text = String::new();
    early[j].text = garble(&clean[3]);
    let g = ours(&late)[0];
    late[g].text = garble(&clean[4]);
    for (name, pages, historical) in [
        ("batch_fx_early_ver01", &early, true),
        ("batch_fx_late_ver01", &late, false),
    ] {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, &pages.iter().collect::<Vec<_>>(), historical, true);
        curate(e, vec![listed(name, &path, None)]).await;
    }
    put_american_stories(e).await;
    let lccn = "sn99000001";
    let fx = "batch_fx_early";
    let rows = [
        local_row(&early[a], fx, &clean[0], &early[a].text, "node1", 10),
        local_row(
            &early[b],
            fx,
            &garble(&clean[1]),
            &early[b].text,
            "node1",
            10,
        ),
        local_row(&early[d], fx, &clean[2], "", "node1", 10),
        local_row(&early[j], fx, &clean[3], &early[j].text, "node-bad", 10),
        local_row(
            &late[g],
            "batch_fx_late",
            &clean[4],
            &late[g].text,
            "node1",
            10,
        ),
    ];
    e.curated
        .put(
            &usnm_ocr_local::pages_path(lccn, "20261011T000000Z-aaaa"),
            usnm_ocr_local::pages::encode(&rows).unwrap(),
            "application/octet-stream",
        )
        .await
        .unwrap();
    e.curated
        .put(
            "ocr-local/revoked/20261013T000000Z.json",
            serde_json::to_vec(&serde_json::json!({
                "node": "node-bad", "reason": "a bad model", "reference": "#296",
            }))
            .unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    // The overlay: a page whose LoC text our Latin reading beats, one
    // curation never had, and Japanese pages for the Japanese index.
    let p0 = &early[sel[4]];
    let latin = format!("{JA_LATIN}\n{JA_LATIN}");
    put_overlay_part(
        e,
        "ocr-ja/pages/a.parquet",
        &[
            JaRow {
                lccn,
                date: early[j].date,
                seq: early[j].seq,
                batch: fx,
                loc_text: "garbled",
                text: &latin,
                ocred_at: 1,
            },
            JaRow {
                lccn,
                date: p0.date,
                seq: 90,
                batch: fx,
                loc_text: "missing",
                text: JA_LATIN,
                ocred_at: 1,
            },
            JaRow {
                lccn,
                date: p0.date,
                seq: 91,
                batch: fx,
                loc_text: "missing",
                text: "米國と日本の戰爭",
                ocred_at: 1,
            },
        ],
    )
    .await;
}

/// `to` with `from`'s curated store and batch records, so two releases
/// read the same inputs (curation names its parts by when it ran).
async fn same_inputs(from: &Env, to: &Env) {
    for (path, bytes) in files(&from.root.join("curated")) {
        let dest = to.root.join("curated").join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, bytes).unwrap();
    }
    for (b, _) in from.state.batches(&[]).await.unwrap() {
        assert!(to.state.create_batch(&b).await.unwrap());
    }
}

/// Every file under `dir`, by path below it.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// `got` and `want` hold the same files, the same bytes; says which differ.
fn assert_same_files(got: &BTreeMap<String, Vec<u8>>, want: &BTreeMap<String, Vec<u8>>) {
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>()
    );
    for (path, bytes) in got {
        if *bytes != want[path] {
            let show = |b: &[u8]| {
                String::from_utf8_lossy(b)
                    .chars()
                    .take(3000)
                    .collect::<String>()
            };
            panic!(
                "{path} differs:\n got: {}\nwant: {}",
                show(bytes),
                show(&want[path])
            );
        }
    }
}

/// A pointer without its publish time, which differs between two runs.
fn timeless(mut pointer: Value) -> Value {
    pointer.as_object_mut().unwrap().remove("published_at");
    pointer
}

/// A release that stopped after its main index passed the exact count (the
/// merge wait ran out) is finished by `release-finish` into the version an
/// uninterrupted run of the same inputs publishes: the same documents in
/// every index, the same snapshot file for file, the same pointer, and a
/// run record that says it was published. Our OCR's part written after the
/// run started is left out (the store as of the run's start); without that,
/// the run's own log lines stop the finish. A log without the merge wait,
/// or an index short of a document, is refused.
#[tokio::test]
async fn a_release_stopped_after_its_count_is_finished_as_it_would_have_ended() {
    use usnm_ingest::activity::Reporter;
    use usnm_ingest::finish::{AsOf, Resume};
    use usnm_ingest::state::RunStatus;
    let settings = |e: &Env| Release {
        american_stories: true,
        ja_latin: true,
        ocr_local: true,
        partition_decade: true,
        single_text_field: true,
        page_language: true,
        split_docs: Some(1000),
        ..e.releaser(22, true)
    };
    let whole = env().await;
    finish_inputs(&whole).await;
    let want = settings(&whole)
        .run(&mut JsonlSink::new(whole.root.join("reference/indexes")))
        .await
        .unwrap()
        .unwrap();

    let e = env().await;
    same_inputs(&whole, &e).await;
    let log = e.root.join("release.log");
    let err = {
        let _log = log_to(&log);
        let mut sink = StopsMerging {
            inner: JsonlSink::new(e.root.join("reference/indexes")),
            index: None,
        };
        settings(&e).run(&mut sink).await.unwrap_err()
    };
    assert!(format!("{err:#}").contains("did not finish"), "{err:#}");
    let version = want.index_version.clone();
    let run = raw_run(&e, &version).await;
    assert_eq!(run["status"], "failed", "{run}");
    // A later part of our OCR, which the run never saw.
    e.curated
        .put(
            &usnm_ocr_local::pages_path("sn99000001", "20261023T000000Z-cccc"),
            std::fs::read(
                e.root
                    .join("curated/ocr-local/pages/sn99000001/20261011T000000Z-aaaa.parquet"),
            )
            .unwrap(),
            "application/octet-stream",
        )
        .await
        .unwrap();

    // A catalog that moved a title since the run: the index's documents
    // say where it was, and a catalog written after the run started isn't
    // taken.
    {
        use usnm_ingest::sink::IndexSink;
        let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
        let catalog = usnm_ingest::catalog::Catalog::load(e.reference.as_ref())
            .await
            .unwrap();
        let held = sink
            .title_fields(&want.indexes[0], &catalog.period_cuts())
            .await
            .unwrap()
            .unwrap();
        usnm_ingest::finish::check_titles(&catalog, &want.indexes[0], &held).unwrap();
        let mut titles = catalog.titles.clone();
        let other = titles[1].place_id.clone();
        titles[0].place_id = other;
        let moved = usnm_ingest::catalog::Catalog::new(titles, catalog.places.clone()).unwrap();
        let err = usnm_ingest::finish::check_titles(&moved, &want.indexes[0], &held)
            .unwrap_err()
            .to_string();
        assert!(err.contains("catalog changed"), "{err}");
        // A title given a place by date since the run: its documents, all in
        // the one place, hold dates the catalog now has in another. The
        // same periods before the first of its dates change nothing.
        let own = catalog.titles[0].place_id.clone();
        let first = std::fs::read_to_string(
            e.root
                .join("reference/indexes")
                .join(format!("{}.jsonl", want.indexes[0])),
        )
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|d| d["lccn"] == catalog.titles[0].lccn.as_str())
        .map(|d| u32::try_from(d["day"].as_u64().unwrap()).unwrap())
        .min()
        .unwrap();
        let by_date = |to: u32| {
            let to = usnm_core::time::date_from_day(to);
            let mut titles = catalog.titles.clone();
            titles[0].place_periods = vec![
                usnm_ingest::catalog::PlacePeriod {
                    place_id: catalog.titles[1].place_id.clone(),
                    from: None,
                    to: Some(to),
                },
                usnm_ingest::catalog::PlacePeriod {
                    place_id: own.clone(),
                    from: to.succ_opt(),
                    to: None,
                },
            ];
            usnm_ingest::catalog::Catalog::new(titles, catalog.places.clone()).unwrap()
        };
        for (to, finishes) in [(first, false), (first - 1, true)] {
            let dated = by_date(to);
            let held = sink
                .title_fields(&want.indexes[0], &dated.period_cuts())
                .await
                .unwrap()
                .unwrap();
            let checked = usnm_ingest::finish::check_titles(&dated, &want.indexes[0], &held);
            if finishes {
                checked.unwrap();
            } else {
                let err = checked.unwrap_err().to_string();
                assert!(
                    err.contains("catalog changed") && err.contains("dated"),
                    "{err}"
                );
            }
        }
        let later = e.root.join("later-reference");
        for (path, bytes) in files(&e.root.join("reference")) {
            let dest = later.join(path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, bytes).unwrap();
        }
        let err = Resume::load(&e.state, &LocalStore::new(&later), &version, &log)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("changed after"), "{err}");
    }

    // A log without the merge wait: the run never passed its count.
    let short = e.root.join("short.log");
    std::fs::write(
        &short,
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|l| !l.contains("release merges"))
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let err = Resume::load(&e.state, e.reference.as_ref(), &version, &short)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing to finish"), "{err}");

    let finish = |curated: Arc<dyn ObjectStore>| {
        let (e, log, version) = (&e, &log, &version);
        async move {
            let resume = Resume::load(&e.state, e.reference.as_ref(), version, log).await?;
            let r = Release {
                curated,
                ..settings(e)
            };
            let lease = r.lock().await?;
            let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
            let out = r.finish(&mut sink, &lease, &Reporter::off(), &resume).await;
            r.unlock(lease).await?;
            out
        }
    };
    let started: std::time::SystemTime = {
        let (run, _) = e.state.run(&version).await.unwrap().unwrap();
        run.started_at.into()
    };
    // Seen whole, the curated store has a part the run didn't: its log says so.
    let err = finish(e.curated.clone()).await.unwrap_err().to_string();
    assert!(
        err.contains("our OCR in ocr-local") && err.contains("parts = 1, the finish has 2"),
        "{err}"
    );
    // An index short of a document isn't finished.
    let main = e
        .root
        .join(format!("reference/indexes/{}.jsonl", want.indexes[0]));
    let full = std::fs::read(&main).unwrap();
    let cut = full[..full.len() - 1]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap();
    std::fs::write(&main, &full[..=cut]).unwrap();
    let err = finish(Arc::new(AsOf::new(e.curated.clone(), started)))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("isn't complete"), "{err}");
    std::fs::write(&main, &full).unwrap();

    // As of the run's start, it finishes into the version the whole run made.
    let got = finish(Arc::new(AsOf::new(e.curated.clone(), started)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, want);
    let run = raw_run(&e, &version).await;
    assert_eq!(run["status"], "published", "{run}");
    assert!(run.get("last_error").is_none_or(Value::is_null), "{run}");
    let whole_run = raw_run(&whole, &version).await;
    for field in [
        "indexes",
        "new_index",
        "docs",
        "pages",
        "duplicate_pages",
        "batch_count",
        "batch_list",
        "build",
    ] {
        assert_eq!(run[field], whole_run[field], "{field}");
    }
    let (got_run, _) = e.state.run(&version).await.unwrap().unwrap();
    assert_eq!(got_run.status, RunStatus::Published);
    assert_eq!(got_run.failed_at, None);
    // Every index and every snapshot file, byte for byte.
    let snapshot = |env: &Env| files(&env.root.join("reference").join(&version));
    assert_same_files(&snapshot(&e), &snapshot(&whole));
    assert!(snapshot(&e).contains_key("page_languages.json"));
    assert!(snapshot(&e).contains_key("american_stories.json"));
    assert!(snapshot(&e).contains_key("ja_latin.json"));
    assert!(snapshot(&e).contains_key("ocr_local_pages.bin"));
    let indexes = |env: &Env| files(&env.root.join("reference/indexes"));
    assert_eq!(
        indexes(&e).keys().count(),
        2,
        "the main and Japanese indexes"
    );
    assert_same_files(&indexes(&e), &indexes(&whole));
    assert_eq!(
        timeless(e.reference_json("current.json").await),
        timeless(whole.reference_json("current.json").await)
    );
    // The last release progress is the finished index's.
    let progress = e.state.release_progress().await.unwrap().unwrap();
    assert_eq!(
        (progress.index_version.as_str(), progress.docs_sent),
        (version.as_str(), want.docs)
    );
}

/// A manifest without its splits' sizes, which differ between two writers
/// that merged the same documents into the same splits.
fn sizeless(mut manifest: Value) -> Value {
    for l in manifest["indexes"].as_array_mut().unwrap() {
        let l = l.as_object_mut().unwrap();
        for k in ["bytes", "footer_bytes", "largest_footer_bytes"] {
            l.remove(k);
        }
    }
    manifest
}

/// The splits of `index`'s file-backed metastore, with their states.
fn metastore_splits(qw: &Path, index: &str) -> Vec<(String, String)> {
    let meta: Value = serde_json::from_slice(
        &std::fs::read(qw.join(format!("meta/{index}/metastore.json"))).unwrap(),
    )
    .unwrap();
    meta["splits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["split_id"].as_str().unwrap().to_owned(),
                s["split_state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// A title's places by date from `(place, from, to)` days, the ends left open
/// as the catalog has them.
fn place_periods(spec: &[(&str, Option<&str>, Option<&str>)]) -> Vec<PlacePeriod> {
    let day = |d: &str| d.parse::<NaiveDate>().unwrap();
    spec.iter()
        .map(|&(place, from, to)| PlacePeriod {
            place_id: place.to_owned(),
            from: from.map(day),
            to: to.map(day),
        })
        .collect()
}

/// The titles the real-writer finish moves between places (#1066), by the
/// dates of their pages as the test lays them out. sn99000002's pages are
/// dated 1839 to 1841: they are in Illinois's place (P00001) before the first
/// page of July 1839 and from the first page of July 1841, and in the title's
/// own, New York's, between, so P00001 has two periods and the search has a
/// range with both a start and an end. sn99000003's pages are dated 1895 to
/// 1897: in Georgia's place (P00004) up to its first page of July 1896 and
/// then in its own, one cut, as a title that moved once has.
///
/// The pages are weekly and the cuts are put against them. The second and
/// third periods of sn99000002 start on the day of a page, so a range that
/// began a day late, or that counted its last day in the next range too,
/// would put a page in the wrong period. sn99000003's starts the day after a
/// page, so a range that began a day early would. A cut in the middle of a
/// week, as a half-year's first day is, shows none of these.
fn moved() -> [(&'static str, Vec<PlacePeriod>); 2] {
    [
        (
            "sn99000002",
            place_periods(&[
                ("P00001", None, Some("1839-07-05")),
                ("P00002", Some("1839-07-06"), Some("1841-07-02")),
                ("P00001", Some("1841-07-03"), None),
            ]),
        ),
        (
            "sn99000003",
            place_periods(&[
                ("P00004", None, Some("1896-07-04")),
                ("P00003", Some("1896-07-05"), None),
            ]),
        ),
    ]
}

/// `e`'s catalog with `moved()`'s titles in the places they were in by date,
/// written before the release starts: a full release puts each page in the
/// place of its date, and a finish takes the catalog the run had (it refuses
/// one written since).
async fn move_titles(e: &Env) {
    let mut titles = e.reference_json("catalog/titles.json").await;
    for (lccn, periods) in moved() {
        let title = titles
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|t| t["lccn"] == lccn)
            .unwrap();
        title["place_periods"] = serde_json::to_value(periods).unwrap();
    }
    e.reference
        .put(
            "catalog/titles.json",
            serde_json::to_vec(&titles).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
}

/// How many documents `lccn` has in each place, by the periods `catalog` gives
/// the title, as its pages with text say: a page is in the period its date
/// falls in, and so in that period's place. Counted from the pages and the
/// periods' dates, not the days and cuts the search is sent.
fn docs_by_period(catalog: &Catalog, lccn: &str, pages: &[&Page]) -> BTreeMap<String, Vec<u64>> {
    let periods = &catalog.title(lccn).unwrap().place_periods;
    let mut by: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for p in pages
        .iter()
        .filter(|p| p.lccn == lccn && !p.text.is_empty())
    {
        let i = periods.iter().position(|q| q.contains(p.date)).unwrap();
        by.entry(periods[i].place_id.clone())
            .or_insert_with(|| vec![0; periods.len()])[i] += 1;
    }
    by
}

/// `release-finish` on a real Quickwit writer through ingest v1 (#283): a
/// full base laid out by decade whose merge wait runs out after its count
/// (a one-second deadline), the writer stopped, and a staged split left in
/// its metastore as a writer stopped mid-merge leaves one. A finish on a new
/// writer over the same data marks the staged split for deletion, merges the
/// index with the release's restart, puts its merge policy back, builds the
/// Japanese index and publishes the version an uninterrupted run of the same
/// inputs publishes: the same counts, the same splits by decade within the
/// split ceiling, the same snapshot and pointer, each page once. Two titles
/// have moved between places by the dates of their pages (`moved`), so the
/// finish counts their documents in each place by period with the range
/// aggregation Quickwit answers and checks them against the catalog (#1132):
/// the counts are those of the pages' dates, and a catalog that puts a period
/// elsewhere than the documents are is refused. Runs when `QUICKWIT_BIN` is
/// set.
#[tokio::test]
async fn a_release_whose_merge_wait_ran_out_is_finished_on_a_real_writer() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::activity::Reporter;
    use usnm_ingest::finish::{check_titles, AsOf, Resume};
    use usnm_ingest::merges::Node;
    use usnm_ingest::sink::{IndexSink, IngestApi, QuickwitNode, QuickwitSink};

    let whole = env().await;
    move_titles(&whole).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let (early, late) = (over_decades(&early), over_decades(&late));
    for (name, pages) in [
        ("batch_fx_early_ver01", &early),
        ("batch_fx_late_ver01", &late),
    ] {
        let path = whole.root.join(format!("{name}.tar.gz"));
        write_archive(&path, &pages.iter().collect::<Vec<_>>(), false, true);
        curate(&whole, vec![listed(name, &path, None)]).await;
    }
    let p0 = early.iter().find(|p| !p.text.is_empty()).unwrap();
    put_overlay_part(
        &whole,
        "ocr-ja/pages/a.parquet",
        &[
            JaRow {
                lccn: &p0.lccn,
                date: p0.date,
                seq: 90,
                batch: "batch_fx_early",
                loc_text: "missing",
                text: JA_LATIN,
                ocred_at: 1,
            },
            JaRow {
                lccn: &p0.lccn,
                date: p0.date,
                seq: 91,
                batch: "batch_fx_early",
                loc_text: "missing",
                text: "米國と日本の戰爭",
                ocred_at: 1,
            },
        ],
    )
    .await;
    let e = env().await;
    move_titles(&e).await;
    same_inputs(&whole, &e).await;
    let settings = |e: &Env| Release {
        ja_latin: true,
        partition_decade: true,
        single_text_field: true,
        page_language: true,
        ..e.releaser(22, true)
    };
    let writer = |e: &Env, port| {
        let qw = e.root.join("qw");
        std::fs::create_dir_all(&qw).unwrap();
        let meta = format!("file://{}/meta", qw.display());
        let root = format!("file://{}/indexes", qw.display());
        let bin = bin.clone();
        async move {
            let node = QuickwitNode::start(Path::new(&bin), &qw, port, &meta, &root)
                .await
                .unwrap();
            (node, root)
        }
    };

    // The whole run.
    let (node, root) = writer(&whole, 7461).await;
    let mut sink = QuickwitSink::new(&node.url, &root)
        .unwrap()
        .watching(&node)
        .merges(quick_merges())
        .ingest(IngestApi::V1);
    let want = settings(&whole).run(&mut sink).await.unwrap().unwrap();
    node.stop().await.unwrap();

    // The run whose merge wait runs out, logged as the release logs.
    let log = e.root.join("release.log");
    let (node, root) = writer(&e, 7465).await;
    let err = {
        let _log = log_to(&log);
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(usnm_ingest::merges::MergeWait {
                timeout: std::time::Duration::from_secs(1),
                ..quick_merges()
            })
            .ingest(IngestApi::V1);
        settings(&e).run(&mut sink).await.unwrap_err()
    };
    assert!(
        format!("{err:#}").contains("did not finish within 1 s"),
        "{err:#}"
    );
    node.stop().await.unwrap();
    let version = want.index_version.clone();
    let main = want.indexes[0].clone();
    assert_eq!(raw_run(&e, &version).await["status"], "failed");

    // A split the stopped writer left staged: a merge's output it never
    // published, a copy of documents the published splits hold.
    let qw = e.root.join("qw");
    let path = qw.join(format!("meta/{main}/metastore.json"));
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut staged = meta["splits"][0].clone();
    staged["split_id"] = "01M5STALESTAGEDSPLIT000000".into();
    staged["split_state"] = "Staged".into();
    staged["create_timestamp"] = 1.into();
    staged["update_timestamp"] = 1.into();
    staged["publish_timestamp"] = Value::Null;
    meta["splits"].as_array_mut().unwrap().push(staged);
    std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();

    // The finish, on a new writer over the same data.
    let (node, root) = writer(&e, 7465).await;
    let resume = Resume::load(&e.state, e.reference.as_ref(), &version, &log)
        .await
        .unwrap();
    let r = Release {
        curated: Arc::new(AsOf::new(e.curated.clone(), resume.run.started_at.into())),
        ..settings(&e)
    };
    let lease = r.lock().await.unwrap();
    let mut sink = QuickwitSink::new(&node.url, &root)
        .unwrap()
        .watching(&node)
        .merges(quick_merges())
        .ingest(IngestApi::V1);
    let got = r
        .finish(&mut sink, &lease, &Reporter::off(), &resume)
        .await
        .unwrap()
        .unwrap();
    r.unlock(lease).await.unwrap();
    assert_eq!(got, want);
    assert_eq!(raw_run(&e, &version).await["status"], "published");

    // The staged split is gone from the index; nothing else is staged.
    let states = metastore_splits(&qw, &main);
    assert!(
        states
            .iter()
            .any(|(id, s)| id == "01M5STALESTAGEDSPLIT000000" && s == "MarkedForDeletion"),
        "{states:?}"
    );
    assert!(!states.iter().any(|(_, s)| s == "Staged"), "{states:?}");
    // The index's own merge policy, and each page once.
    let http = reqwest::Client::new();
    let n = Node {
        http: &http,
        base: &node.url,
    };
    let config = n.index_config(&main).await.unwrap();
    assert_eq!(
        config["indexing_settings"]["merge_policy"]["merge_factor"],
        10
    );
    let v: Value = http
        .post(format!("{}/api/v1/{main}/search", node.url))
        .json(&serde_json::json!({
            "query": "*", "max_hits": 0,
            "aggs": {"twice": {"terms": {"field": "doc_id", "size": 100, "min_doc_count": 2}}},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["aggregations"]["twice"]["buckets"], serde_json::json!([]));

    // The finish counted the documents of the titles that moved by place and
    // period with the search the node answered. Sent again, it counts each
    // document in the place and period of its date, as the pages say, and the
    // check passes.
    let catalog = Catalog::load(e.reference.as_ref()).await.unwrap();
    let all: Vec<&Page> = early.iter().chain(&late).collect();
    let held = sink
        .title_fields(&main, &catalog.period_cuts())
        .await
        .unwrap()
        .unwrap();
    for (lccn, periods) in moved() {
        let want = docs_by_period(&catalog, lccn, &all);
        // Every period has documents, so no count is of nothing.
        let filled = want.values().flatten().filter(|&&n| n > 0).count();
        assert_eq!(filled, periods.len(), "{lccn}: {want:?}");
        assert_eq!(held[lccn].by_period, want, "{lccn}");
    }
    check_titles(&catalog, &main, &held).unwrap();
    // A catalog that gives some dates another place than the documents of
    // those dates are in is refused: for sn99000002, dates in the middle of
    // a place's run; for sn99000003, the end of it.
    let elsewhere = [
        (
            "sn99000002",
            place_periods(&[
                ("P00001", None, Some("1839-06-30")),
                ("P00002", Some("1839-07-01"), Some("1840-06-30")),
                ("P00001", Some("1840-07-01"), Some("1840-12-31")),
                ("P00002", Some("1841-01-01"), Some("1841-06-30")),
                ("P00001", Some("1841-07-01"), None),
            ]),
            "in P00002 dated 1840-07-01 to 1840-12-31",
        ),
        (
            "sn99000003",
            place_periods(&[
                ("P00004", None, Some("1897-06-30")),
                ("P00003", Some("1897-07-01"), None),
            ]),
            "in P00003 dated up to 1897-06-30",
        ),
    ];
    for (lccn, periods, says) in elsewhere {
        let mut titles = catalog.titles.clone();
        titles
            .iter_mut()
            .find(|t| t.lccn == lccn)
            .unwrap()
            .place_periods = periods;
        let other = Catalog::new(titles, catalog.places.clone()).unwrap();
        let held = sink
            .title_fields(&main, &other.period_cuts())
            .await
            .unwrap()
            .unwrap();
        let err = check_titles(&other, &main, &held).unwrap_err().to_string();
        assert!(
            err.contains("catalog changed") && err.contains(says),
            "{lccn}: {err}"
        );
    }
    node.stop().await.unwrap();

    // The same version as the whole run's: snapshot, layout and pointer.
    let snapshot = |env: &Env| {
        let mut f = files(&env.root.join("reference").join(&version));
        f.remove("manifest.json");
        f
    };
    assert_same_files(&snapshot(&e), &snapshot(&whole));
    let manifest = format!("{version}/manifest.json");
    let (m, w) = (
        e.reference_json(&manifest).await,
        whole.reference_json(&manifest).await,
    );
    assert_eq!(sizeless(m.clone()), sizeless(w));
    assert_eq!(m["indexes"].as_array().unwrap().len(), 2, "{m}");
    assert_eq!(
        m["indexes"][0]["splits_by_decade"],
        serde_json::json!({"1830": 1, "1840": 1, "1860": 1, "1890": 1}),
        "{}",
        m["indexes"][0]
    );
    assert_eq!(
        timeless(e.reference_json("current.json").await),
        timeless(whole.reference_json("current.json").await)
    );
}
