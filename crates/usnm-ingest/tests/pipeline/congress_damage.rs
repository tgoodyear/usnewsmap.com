//! `usnm-ingest congress-damage` (#922): the damage metric over every page of
//! the curated Serial Set, on a store built from the recorded GovInfo
//! responses in `fixtures/congress/govinfo` and a few synthetic volumes.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::{Array, Date32Array, Float64Array, Int32Array, StringArray};
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use usnm_congress::curated::{self, Counts, CuratedPage};
use usnm_congress::release::Listing;
use usnm_core::damage;
use usnm_ingest::congress_damage::{
    self, corpus_path, summary_path, table_path, Corpus, Options, PassReport, Summary,
};
use usnm_ingest::congress_damage::{BELOW_FLOOR, MIN_CHARS, NO_TEXT};
use usnm_store::ObjectStore;

use crate::common::{env, put_globe, Env};
use crate::congress::put_serialset;

/// English prose of well over the floor, and the same with its common words
/// one OCR edit off.
const CLEAN: &str = "The Committee on the Judiciary, to whom was referred the petition of \
    the inhabitants of the Territory, have had the same under consideration, and report \
    that the prayer of the petitioners is reasonable, and that it ought to be granted. \
    The Senate and the House of Representatives of the United States of America in \
    Congress assembled have power to make all needful rules and regulations respecting \
    the territory belonging to the United States.";
const DAMAGED: &str = "Tbe Comrnittee on tbe Judiciary, to wbom was referrcd tbe petition of \
    tbe inbabitants of tbe Territory, bave bad tbe sarne under consideration, aud report \
    tbat tbe prayer of tbe petitioners is reasonable, aud tbat it ougbt to be granted. \
    Tbe Senate aud tbe House of Representatives of tbe United States of Arnerica in \
    Congress assernbled bave power to rnake all needful rules aud regulations respecting \
    tbe territory belonging to tbe United States.";

const VOLUME: &str = "00033_00_00";
const SYNTHETIC: &str = "00034_00_00";

fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

/// A page of a synthetic volume, shaped like the fixture's rows.
fn page(base: &CuratedPage, unit: &str, seq: u32, year: i32, text: Option<&str>) -> CuratedPage {
    let mut p = base.clone();
    p.unit_id = unit.into();
    p.package_id = Some(format!("SERIALSET-{unit}-001-0001-0000"));
    p.doc_id = format!("SERIALSET-{unit}-001-0001-0000_p{seq}");
    p.seq = seq;
    p.date = NaiveDate::from_ymd_opt(year, 3, 4).unwrap();
    p.text_status = match text {
        Some(t) if t.chars().count() >= usnm_core::text::MIN_PAGE_CHARS => "ok",
        Some(_) => "short",
        None => "empty",
    }
    .into();
    p.text = (p.text_status == "ok").then(|| text.unwrap().to_owned());
    p.text_chars = text.map_or(0, |t| t.chars().count() as i32);
    p
}

/// A synthetic volume of five pages: clean prose of 1821, damaged prose of
/// 1830, a stub of text (short), a blank leaf and a few lines of prose, under
/// the floor.
async fn put_synthetic(e: &Env, unit: &str) -> Counts {
    let base = rows_of(e, VOLUME).await.remove(0);
    let rows = [
        page(&base, unit, 1, 1821, Some(CLEAN)),
        page(&base, unit, 2, 1830, Some(DAMAGED)),
        page(&base, unit, 3, 1830, Some("12")),
        page(&base, unit, 4, 1830, None),
        page(&base, unit, 5, 1830, Some(&CLEAN[..MIN_CHARS - 1])),
    ];
    curated::write_attempt(
        e.curated.as_ref(),
        "serialset",
        "govinfo",
        unit,
        &rows,
        at(),
    )
    .await
    .unwrap()
}

/// The rows of a unit's newest finished attempt.
async fn rows_of(e: &Env, unit: &str) -> Vec<CuratedPage> {
    let f = curated::finished_in(e.curated.as_ref(), "serialset")
        .await
        .unwrap();
    let f = f.iter().find(|f| f.unit == unit).unwrap();
    let (counts, _) = curated::read_counts(e.curated.as_ref(), f).await.unwrap();
    let mut rows = Vec::new();
    for part in &counts.parts {
        rows.extend(
            curated::read_checked(e.curated.as_ref(), part)
                .await
                .unwrap(),
        );
    }
    rows
}

fn opts() -> Options {
    Options {
        series: "serialset".into(),
        parallel: 2,
        ..Options::default()
    }
}

async fn score(e: &Env, out: &Arc<dyn ObjectStore>, opts: &Options) -> anyhow::Result<PassReport> {
    congress_damage::run(Arc::clone(&e.curated), Arc::clone(out), opts).await
}

/// One page of a table, as read back.
#[derive(Debug, PartialEq)]
struct Row {
    doc_id: String,
    serial: Option<String>,
    text_chars: i32,
    skipped: Option<String>,
    damage_rate: Option<f64>,
    days: i32,
}

async fn table(out: &dyn ObjectStore, unit: &str, attempt: u32) -> Vec<Row> {
    let bytes = out
        .get(&table_path("serialset", unit, attempt))
        .await
        .unwrap()
        .expect("the table is written");
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap();
    let mut rows = Vec::new();
    for batch in reader {
        let b = batch.unwrap();
        let col = |n: &str| b.column_by_name(n).unwrap().clone();
        let strings = |n: &str| {
            col(n)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .clone()
        };
        let (doc_id, serial, skipped) = (strings("doc_id"), strings("serial"), strings("skipped"));
        let rate = col("damage_rate");
        let rate = rate.as_any().downcast_ref::<Float64Array>().unwrap();
        let chars = col("text_chars");
        let chars = chars.as_any().downcast_ref::<Int32Array>().unwrap();
        let date = col("date");
        let date = date.as_any().downcast_ref::<Date32Array>().unwrap();
        for i in 0..b.num_rows() {
            rows.push(Row {
                doc_id: doc_id.value(i).to_owned(),
                serial: (!serial.is_null(i)).then(|| serial.value(i).to_owned()),
                text_chars: chars.value(i),
                skipped: (!skipped.is_null(i)).then(|| skipped.value(i).to_owned()),
                damage_rate: (!rate.is_null(i)).then(|| rate.value(i)),
                days: date.value(i),
            });
        }
    }
    rows
}

async fn summary(out: &dyn ObjectStore, unit: &str, attempt: u32) -> Summary {
    let body = out
        .get(&summary_path("serialset", unit, attempt))
        .await
        .unwrap()
        .expect("the summary is written");
    serde_json::from_slice(&body).unwrap()
}

/// Every object under `prefix` with its bytes.
async fn snapshot(store: &dyn ObjectStore, prefix: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for p in store.list(prefix).await.unwrap() {
        let body = store.get(&p).await.unwrap().unwrap();
        out.insert(p, body);
    }
    out
}

fn on_disk(e: &Env, path: &str) -> std::path::PathBuf {
    e.root.join("curated").join(path)
}

/// Every page of the newest attempts is scored into the curated store's
/// `audit/damage/`, none of it is seen as a unit, and the numbers add up
/// against the tables.
#[tokio::test]
async fn every_page_is_scored_beside_the_curated_units() {
    let e = env().await;
    let fixture = put_serialset(&e).await;
    assert_eq!((fixture.pages, fixture.indexable), (6, 4));
    let synthetic = put_synthetic(&e, SYNTHETIC).await;
    let out = Arc::clone(&e.curated);
    let units_before = Listing::load(e.curated.as_ref()).await.unwrap().refs();
    let congress_before = snapshot(e.curated.as_ref(), "congress").await;
    assert_eq!(units_before.len(), 2);

    let report = score(&e, &out, &opts()).await.unwrap();
    assert_eq!(
        (report.volumes, report.scored, report.skipped, report.left),
        (2, 2, 0, 0)
    );
    assert_eq!(report.pages, fixture.pages + synthetic.pages);
    assert!(report.left_out.is_empty());

    // Nothing the release lists changed, and nothing under congress/.
    assert_eq!(
        Listing::load(e.curated.as_ref()).await.unwrap().refs(),
        units_before
    );
    assert_eq!(
        snapshot(e.curated.as_ref(), "congress").await,
        congress_before
    );
    let written: Vec<String> = e.curated.list("audit").await.unwrap();
    assert!(written
        .iter()
        .all(|p| p.starts_with("audit/damage/v2/congress/serialset/")));
    assert_eq!(
        written.len(),
        2 * 2 + 1,
        "a table and a summary each, one roll-up"
    );

    // The synthetic volume: a row for every page, in order, with the scores
    // of the metric on the pages that have them.
    let t = table(out.as_ref(), SYNTHETIC, 1).await;
    let ids: Vec<&str> = t.iter().map(|r| r.doc_id.as_str()).collect();
    assert_eq!(
        ids,
        (1..=5)
            .map(|n| format!("SERIALSET-{SYNTHETIC}-001-0001-0000_p{n}"))
            .collect::<Vec<_>>()
    );
    let clean = damage::score_forced(CLEAN, "eng").unwrap().damage_rate;
    let dirty = damage::score_forced(DAMAGED, "eng").unwrap().damage_rate;
    assert_eq!(t[0].damage_rate, clean);
    assert_eq!(t[1].damage_rate, dirty);
    assert!(clean.unwrap() < damage::DAMAGED && dirty.unwrap() > damage::BADLY_DAMAGED);
    assert_eq!(
        t.iter().map(|r| r.skipped.as_deref()).collect::<Vec<_>>(),
        [None, None, Some(NO_TEXT), Some(NO_TEXT), Some(BELOW_FLOOR)]
    );
    assert_eq!(t[2].text_chars, 2);
    assert_eq!(t[4].text_chars as usize, MIN_CHARS - 1);
    assert_eq!(
        t[4].damage_rate, None,
        "a page under the floor has no scores"
    );
    assert_eq!(t[0].serial.as_deref(), Some("33"));
    let s = summary(out.as_ref(), SYNTHETIC, 1).await;
    assert_eq!(s.counts_sha256, synthetic_sha(&e).await);
    assert_eq!(
        (
            s.total.pages,
            s.total.scored,
            s.total.no_text,
            s.total.below_floor
        ),
        (5, 2, 2, 1)
    );
    assert_eq!((s.total.damaged, s.total.badly_damaged), (1, 1));
    assert_eq!(
        s.by_decade
            .iter()
            .map(|(d, t)| (*d, t.pages))
            .collect::<Vec<_>>(),
        [(1820, 1), (1830, 4)]
    );

    // The fixture volume: its short pages have no text, its pages under the
    // floor are kept without scores, and the rest are scored as the metric
    // scores them.
    let rows = rows_of(&e, VOLUME).await;
    let t = table(out.as_ref(), VOLUME, 1).await;
    assert_eq!(t.len(), rows.len());
    for (r, p) in t.iter().zip(&rows) {
        assert_eq!(r.doc_id, p.doc_id);
        let text = p.text.as_deref();
        match text {
            None => assert_eq!(r.skipped.as_deref(), Some(NO_TEXT), "{}", p.doc_id),
            Some(t) if t.chars().count() < MIN_CHARS => {
                assert_eq!(r.skipped.as_deref(), Some(BELOW_FLOOR), "{}", p.doc_id);
            }
            Some(t) => {
                assert_eq!(r.skipped, None);
                assert_eq!(
                    r.damage_rate,
                    damage::score_forced(t, "eng").unwrap().damage_rate,
                    "{}",
                    p.doc_id
                );
            }
        }
        if r.skipped.is_some() {
            assert_eq!(r.damage_rate, None);
        }
    }
    assert!(
        t.iter().any(|r| r.skipped.is_none()),
        "the fixture has a page to score"
    );
    assert!(
        t.iter().any(|r| r.skipped.as_deref() == Some(NO_TEXT)),
        "and pages with no text"
    );
    let fs = summary(out.as_ref(), VOLUME, 1).await;
    assert_eq!(fs.total.pages, fixture.pages);
    assert_eq!(
        fs.total.scored,
        t.iter().filter(|r| r.skipped.is_none()).count() as u64
    );
    assert_eq!(
        fs.total.pages,
        fs.total.no_text + fs.total.below_floor + fs.total.scored
    );

    // The roll-up is the tables summed, counted again from their rows.
    let c = report.corpus.expect("a run over every volume rolls up");
    assert!(c.written);
    let body = out.get(&c.path).await.unwrap().unwrap();
    let corpus: Corpus = serde_json::from_slice(&body).unwrap();
    assert_eq!(c.path, corpus_path("serialset", &corpus.key));
    assert_eq!(
        corpus.attempts,
        [(VOLUME.to_owned(), 1), (SYNTHETIC.to_owned(), 1)]
    );
    let mut all = table(out.as_ref(), VOLUME, 1).await;
    all.extend(table(out.as_ref(), SYNTHETIC, 1).await);
    let rated: Vec<f64> = all.iter().filter_map(|r| r.damage_rate).collect();
    assert_eq!(corpus.volumes, 2);
    assert_eq!(corpus.total.pages, all.len() as u64);
    assert_eq!(corpus.total.rated, rated.len() as u64);
    assert_eq!(
        corpus.total.damaged,
        rated.iter().filter(|r| **r > damage::DAMAGED).count() as u64
    );
    assert_eq!(
        corpus.total.badly_damaged,
        rated.iter().filter(|r| **r > damage::BADLY_DAMAGED).count() as u64
    );
    // The fixture's 1820 pages and the synthetic 1821 page share a decade.
    assert_eq!(
        corpus.by_decade.keys().copied().collect::<Vec<_>>(),
        [1820, 1830]
    );
    assert_eq!(
        corpus.by_decade.values().map(|d| d.pages).sum::<u64>(),
        corpus.total.pages
    );
    assert_eq!(corpus.by_decade[&1830].pages, 4);
    assert_eq!(corpus.by_decade[&1830].below_floor, 1);
    assert_eq!(corpus.by_decade[&1830].badly_damaged, 1);
    assert_eq!(corpus.by_decade[&1830].scored, 1);
}

async fn synthetic_sha(e: &Env) -> String {
    let f = curated::finished_in(e.curated.as_ref(), "serialset")
        .await
        .unwrap();
    let f = f.iter().find(|f| f.unit == SYNTHETIC).unwrap();
    curated::read_counts(e.curated.as_ref(), f).await.unwrap().1
}

/// A second run over the same volumes writes nothing, and a table is never
/// replaced: not by the run, and not when one is found without its summary
/// unless it is the very same bytes.
#[tokio::test]
async fn a_second_run_writes_nothing_and_a_table_is_never_replaced() {
    let e = env().await;
    put_serialset(&e).await;
    put_synthetic(&e, SYNTHETIC).await;
    let out = Arc::clone(&e.curated);
    let first = score(&e, &out, &opts()).await.unwrap();
    assert_eq!(first.scored, 2);
    let before = snapshot(out.as_ref(), "audit").await;

    let again = score(&e, &out, &opts()).await.unwrap();
    assert_eq!((again.scored, again.skipped, again.pages), (0, 2, 0));
    let corpus = again.corpus.expect("the roll-up is found");
    assert!(!corpus.written);
    assert_eq!(corpus.total, first.corpus.unwrap().total);
    assert_eq!(snapshot(out.as_ref(), "audit").await, before);

    // A table whose summary is missing (a run stopped between the two): the
    // same scores are the same bytes, so the next run writes the summary.
    let summary_file = on_disk(&e, &summary_path("serialset", SYNTHETIC, 1));
    std::fs::remove_file(&summary_file).unwrap();
    let resumed = score(&e, &out, &opts()).await.unwrap();
    assert_eq!((resumed.scored, resumed.skipped), (1, 1));
    assert_eq!(snapshot(out.as_ref(), "audit").await, before);

    // Other bytes in its place: the run stops, and keeps what is there.
    std::fs::remove_file(&summary_file).unwrap();
    let table_file = on_disk(&e, &table_path("serialset", SYNTHETIC, 1));
    std::fs::write(&table_file, b"not the scores").unwrap();
    let e1 = score(&e, &out, &opts()).await.unwrap_err();
    let msg = format!("{e1:#}");
    assert!(
        msg.contains("without its summary") && msg.contains(SYNTHETIC),
        "{msg}"
    );
    assert_eq!(std::fs::read(&table_file).unwrap(), b"not the scores");
    assert!(!summary_file.exists());
}

/// Each unit's newest finished attempt is scored, a later empty attempt
/// takes the unit out, a finished attempt after it puts it back, and a new
/// attempt gets its own tables and its own roll-up beside the old.
#[tokio::test]
async fn the_newest_finished_attempts_are_scored() {
    let e = env().await;
    put_serialset(&e).await;
    let second = put_serialset(&e).await;
    assert_eq!(second.attempt, 2);
    put_synthetic(&e, SYNTHETIC).await;
    let gone = "00035_00_00";
    put_synthetic(&e, gone).await;
    curated::write_empty(
        e.curated.as_ref(),
        "serialset",
        gone,
        at(),
        None,
        None,
        vec![format!("SERIALSET-{gone}-001-0001-0000: undated")],
    )
    .await
    .unwrap();
    // The release's own listing agrees with what is about to be scored.
    let listing = Listing::load(e.curated.as_ref()).await.unwrap();
    assert_eq!(listing.left_out, [format!("serialset/{gone}")]);
    assert!(
        curated::finished_in(e.curated.as_ref(), "serialset")
            .await
            .unwrap()
            .iter()
            .any(|f| f.unit == gone),
        "finished_in alone still lists it"
    );

    let out = Arc::clone(&e.curated);
    let r1 = score(&e, &out, &opts()).await.unwrap();
    assert_eq!((r1.volumes, r1.scored), (2, 2));
    assert_eq!(r1.left_out, [gone]);
    let audit = e.curated.list("audit").await.unwrap();
    assert!(audit.contains(&table_path("serialset", VOLUME, 2)));
    assert!(audit.contains(&summary_path("serialset", VOLUME, 2)));
    assert!(
        audit
            .iter()
            .all(|p| !p.contains(gone) && !p.contains("00033_00_00/0001")),
        "{audit:?}"
    );
    let c1 = r1.corpus.unwrap();
    assert_eq!(c1.volumes, 2);

    // The volume comes back with a finished attempt after the empty one, and
    // the fetch writes the fixture volume again.
    let back = put_synthetic(&e, gone).await;
    assert_eq!(back.attempt, 3);
    let third = put_serialset(&e).await;
    assert_eq!(third.attempt, 3);
    let r2 = score(&e, &out, &opts()).await.unwrap();
    assert_eq!((r2.volumes, r2.scored, r2.skipped), (3, 2, 1));
    assert!(r2.left_out.is_empty());
    assert!(out
        .exists(&table_path("serialset", VOLUME, 3))
        .await
        .unwrap());
    assert!(out.exists(&table_path("serialset", gone, 3)).await.unwrap());
    let c2 = r2.corpus.unwrap();
    assert!(c2.written);
    assert_ne!(c1.path, c2.path);
    assert!(out.exists(&c1.path).await.unwrap(), "the old roll-up stays");
    let corpus: Corpus =
        serde_json::from_slice(&out.get(&c2.path).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        corpus.attempts,
        [
            (VOLUME.to_owned(), 3),
            (SYNTHETIC.to_owned(), 1),
            (gone.to_owned(), 3)
        ]
    );
}

/// `--volumes` and `--serials` pick volumes (and an unknown one is an
/// error), and only a run over every volume rolls up; a deadline leaves
/// volumes for the next run and rolls up nothing.
#[tokio::test]
async fn a_run_can_be_narrowed_and_stopped() {
    let e = env().await;
    put_serialset(&e).await;
    put_synthetic(&e, SYNTHETIC).await;
    let out: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(e.root.join("elsewhere")));

    let by_volume = Options {
        volumes: vec![SYNTHETIC.into()],
        ..opts()
    };
    let r = score(&e, &out, &by_volume).await.unwrap();
    assert_eq!((r.volumes, r.scored), (1, 1));
    assert!(r.corpus.is_none());
    assert_eq!(
        out.list("audit").await.unwrap(),
        [
            table_path("serialset", SYNTHETIC, 1),
            summary_path("serialset", SYNTHETIC, 1)
        ]
    );
    assert!(
        e.curated.list("audit").await.unwrap().is_empty(),
        "--out is not the curated store"
    );

    let by_serial = Options {
        serials: vec![33],
        ..opts()
    };
    let r = score(&e, &out, &by_serial).await.unwrap();
    assert_eq!((r.volumes, r.scored, r.skipped), (1, 1, 0));
    assert!(r.corpus.is_none());

    for (bad, wanted) in [
        (
            Options {
                volumes: vec!["00099_00_00".into()],
                ..opts()
            },
            "00099_00_00",
        ),
        (
            Options {
                serials: vec![99],
                ..opts()
            },
            "serial numbers [99]",
        ),
    ] {
        let msg = score(&e, &out, &bad).await.unwrap_err().to_string();
        assert!(msg.contains(wanted), "{msg}");
    }

    // Everything is done, so the full run only rolls up; with a deadline
    // that has passed and nothing scored yet, it leaves the volumes.
    let fresh: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(e.root.join("fresh")));
    let stopped = Options {
        deadline: Some(tokio::time::Instant::now()),
        ..opts()
    };
    let r = score(&e, &fresh, &stopped).await.unwrap();
    assert_eq!((r.scored, r.skipped, r.left), (0, 0, 2));
    assert!(r.corpus.is_none());
    assert!(fresh.list("audit").await.unwrap().is_empty());
    let full = score(&e, &out, &opts()).await.unwrap();
    assert_eq!((full.scored, full.skipped), (0, 2));
    assert!(full.corpus.unwrap().written);
}

/// A part that is not what `counts.json` says stops the run, and names the
/// volume, as the release does; no roll-up is written.
#[tokio::test]
async fn a_part_that_differs_from_its_counts_stops_the_run() {
    let e = env().await;
    put_serialset(&e).await;
    let synthetic = put_synthetic(&e, SYNTHETIC).await;
    std::fs::write(on_disk(&e, &synthetic.parts[0].path), b"damaged in storage").unwrap();
    let out = Arc::clone(&e.curated);
    let msg = format!(
        "{:#}",
        score(
            &e,
            &out,
            &Options {
                parallel: 1,
                ..opts()
            }
        )
        .await
        .unwrap_err()
    );
    assert!(
        msg.contains(&format!("serialset/{SYNTHETIC} attempt 1"))
            && msg.contains("doesn't match its counts.json"),
        "{msg}"
    );
    let audit = e.curated.list("audit").await.unwrap();
    assert!(audit.iter().all(|p| !p.contains("corpus")), "{audit:?}");
    assert!(audit.iter().all(|p| !p.contains(SYNTHETIC)), "{audit:?}");
}

/// A summary from another counts.json than the attempt's is refused rather
/// than trusted.
#[tokio::test]
async fn a_summary_for_other_inputs_is_not_kept() {
    let e = env().await;
    put_serialset(&e).await;
    let out = Arc::clone(&e.curated);
    score(&e, &out, &opts()).await.unwrap();
    let path = on_disk(&e, &summary_path("serialset", VOLUME, 1));
    let mut s: Summary = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    s.counts_sha256 = "0".repeat(64);
    std::fs::write(&path, serde_json::to_vec(&s).unwrap()).unwrap();
    let msg = format!("{:#}", score(&e, &out, &opts()).await.unwrap_err());
    assert!(
        msg.contains("was not written for this attempt") && msg.contains("delete it"),
        "{msg}"
    );
}

/// The Globe's items are scored the same way (`--series globe`), and an
/// empty series is an error rather than an empty roll-up.
#[tokio::test]
async fn other_series_are_scored_the_same_way() {
    let e = env().await;
    let globe = put_globe(&e, &[1, 2, 3, 300, 301]).await;
    let out = Arc::clone(&e.curated);
    let globe_opts = Options {
        series: "globe".into(),
        ..opts()
    };
    let r = score(&e, &out, &globe_opts).await.unwrap();
    assert_eq!((r.volumes, r.scored, r.pages), (1, 1, globe.pages));
    let audit = e.curated.list("audit").await.unwrap();
    assert!(audit.iter().all(
        |p| p.starts_with("audit/damage/v2/congress/globe/metadc29274/")
            || p.starts_with("audit/damage/v2/congress/globe/corpus/")
    ));
    assert_eq!(audit.len(), 3);
    // The Serial Set has none yet.
    let msg = score(&e, &out, &opts()).await.unwrap_err().to_string();
    assert!(msg.contains("no finished serialset volumes"), "{msg}");
    // Serial numbers are the Serial Set's.
    let msg = score(
        &e,
        &out,
        &Options {
            serials: vec![33],
            ..globe_opts
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(msg.contains("--serials is for the Serial Set"), "{msg}");
}
