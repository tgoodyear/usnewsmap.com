//! The status document's sections, built from the published reference data
//! and a [`Summary`] of the pipeline state. Pure functions of their inputs,
//! so they are tested without a database.

mod pipeline;
mod published;
mod titles;

pub use pipeline::{
    backfill, indexing, Backfill, ByStatus, Failed, HourBin, InProgress, Indexing, LocPacer,
    Recent, ReleaseProgress, Run, RunState, Throughput, Writer, HISTORY_HOURS, RATE_WINDOW_HOURS,
};
pub use published::{
    by_language, by_state, published, sources, AmericanStories, ByLanguage, EnginePages,
    JaLatinPages, LanguagePages, OcrLocalSummary, Published, ReasonPages, Sources, StatePages,
    TitleOcrPages, YearPages,
};
pub use titles::{titles, Catalog, CatalogTitles, Titles, TitlesPipeline};

use schemars::JsonSchema;
use serde::Serialize;

/// A section that needs the pipeline state: its data, or why there is none.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(rename = "{T}Section")]
pub struct Section<T> {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T> Section<T> {
    pub fn of(data: T) -> Self {
        Self {
            available: true,
            reason: None,
            data: Some(data),
        }
    }

    pub fn unavailable(reason: &str) -> Self {
        Self {
            available: false,
            reason: Some(reason.to_owned()),
            data: None,
        }
    }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        round1(100.0 * part as f64 / whole as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refdata::RefData;
    use chrono::{DateTime, Duration, Utc};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use usnm_state::summary::{BatchSummary, LockSummary, RunSummary};
    use usnm_state::{state::BatchStatus, summary::Summary};

    fn at() -> DateTime<Utc> {
        "2026-09-29T12:30:00Z".parse().unwrap()
    }

    fn batch(v: Value) -> BatchSummary {
        serde_json::from_value(v).unwrap()
    }

    fn curated(name: &str, hours_ago: i64, pages: u64, lccns: &[&str]) -> BatchSummary {
        batch(json!({
            "batch": name, "version": 1, "status": "curated", "attempts": 1,
            "updated_at": at() - Duration::minutes(hours_ago * 60 + 1),
            "curated_version": 1, "curated_pages": pages, "curated_ok_pages": pages - 1,
            "curated_lccns": lccns,
        }))
    }

    fn summary() -> Summary {
        Summary {
            batches: vec![
                curated("batch_a", 0, 10, &["sn1"]),
                curated("batch_b", 1, 20, &["sn1", "sn2"]),
                curated("batch_c", 30, 5, &["sn3"]),
                curated("batch_old", 100, 7, &["sn1"]),
                batch(json!({"batch": "batch_q", "version": 1, "status": "queued",
                         "updated_at": at()})),
                batch(
                    json!({"batch": "batch_r", "version": 2, "status": "queued", "attempts": 2,
                         "last_error": "GET https://chroniclingamerica.loc.gov/x returned 503",
                         "updated_at": at(), "curated_version": 1, "curated_pages": 3,
                         "curated_at": at() - Duration::hours(30),
                         "curated_lccns": ["sn1"]}),
                ),
                batch(
                    json!({"batch": "batch_d", "version": 1, "status": "downloading",
                         "attempts": 1, "updated_at": at() - Duration::minutes(5),
                         "lease": {"owner": "caj-usnm-backfill-x-12-0a1b2c3d",
                                   "until": at() + Duration::hours(1)}}),
                ),
                batch(
                    json!({"batch": "batch_e", "version": 1, "status": "downloading",
                         "attempts": 3, "updated_at": at() - Duration::hours(5),
                         "lease": {"owner": "w-1-deadbeef", "until": at() - Duration::hours(1)}}),
                ),
                batch(
                    json!({"batch": "batch_f", "version": 1, "status": "failed", "attempts": 5,
                         "last_error": "put https://stusnmdata.blob.core.windows.net/curated/x: 403 from 192.0.2.4",
                         "updated_at": at() - Duration::hours(2)}),
                ),
            ],
            ..Summary::default()
        }
    }

    #[test]
    fn backfill_counts_rates_and_lists() {
        let mut s = summary();
        s.ops.pacer_blocked_until = Some(at() + Duration::minutes(40));
        s.ops.pacer_next = Some(at() - Duration::minutes(1));
        let b = backfill(at(), &s);
        assert_eq!(b.total, 9);
        assert_eq!(
            (
                b.by_status.queued,
                b.by_status.downloading,
                b.by_status.curated,
                b.by_status.failed
            ),
            (2, 2, 4, 1)
        );
        assert_eq!((b.in_progress, b.stale_leases, b.retrying), (1, 1, 1));
        assert_eq!(b.percent, 44.4);
        assert_eq!((b.pages, b.ok_pages), (42, 38));
        assert_eq!(b.versions.get("01"), Some(&8));
        assert_eq!(b.versions.get("02"), Some(&1));
        assert_eq!(b.newer_versions_pending, 1);
        // 48 hourly bins ending with this hour; batch_old is older than that.
        let t = &b.throughput;
        assert_eq!(t.hours.len(), 48);
        assert_eq!(
            t.hours[47].start,
            "2026-09-29T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        // batch_r's curation (queued again for a newer version) still counts.
        assert_eq!(t.hours.iter().map(|h| h.batches).sum::<u64>(), 4);
        assert_eq!(t.hours.iter().map(|h| h.pages).sum::<u64>(), 38);
        // Two batches in the last 12 hours: 2/12 per hour, 4 remaining → 24 h.
        assert_eq!(t.rate_per_hour, 0.2);
        assert_eq!(t.remaining, 4);
        assert_eq!(t.eta, Some(at() + Duration::hours(24)));
        assert!(b.loc.throttled);
        assert_eq!(b.loc.next_slot, None);
        // Workers appear only as short ids; the stopped one is flagged.
        let ids: Vec<_> = b
            .in_progress_batches
            .iter()
            .map(|a| (a.batch.as_str(), a.worker.as_deref(), a.lease_expired))
            .collect();
        assert_eq!(
            ids,
            [
                ("batch_e", Some("adbeef"), true),
                ("batch_d", Some("1b2c3d"), false)
            ]
        );
        assert_eq!(b.recent[0].batch, "batch_a");
        assert_eq!(b.recent.len(), 5);
        let err = b.failed_batches[0].error.as_deref().unwrap();
        assert_eq!(err, "put [url]: 403 from [ip]");
        let text = serde_json::to_string(&b).unwrap();
        assert!(!text.contains("caj-usnm"), "{text}");
        assert!(!text.contains("blob.core"), "{text}");
    }

    #[test]
    fn no_rate_means_no_eta() {
        let mut s = summary();
        s.batches.retain(|b| b.status != BatchStatus::Curated);
        let b = backfill(at(), &s);
        assert_eq!((b.throughput.rate_per_hour, b.throughput.eta), (0.0, None));
        assert_eq!(b.percent, 0.0);
        assert!(!b.loc.throttled);
    }

    fn run(v: Value) -> RunSummary {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn indexing_runs_writer_and_live_progress() {
        let mut s = Summary {
            runs: vec![
                run(
                    json!({"index_version": "v1", "full": true, "indexes": ["b1"], "new_index": "b1",
                       "status": "published", "docs": 9, "pages": 10, "batches_len": 2,
                       "started_at": "2026-09-28T10:00:00Z", "published_at": "2026-09-28T11:00:00Z"}),
                ),
                run(
                    json!({"index_version": "v2", "full": false, "indexes": ["b1", "d1"], "new_index": "d1",
                       "status": "failed", "started_at": "2026-09-29T01:00:00Z",
                       "last_error": "lock `quickwit-writer` is held by `ca-x-1-0a1b2c3d` until later"}),
                ),
                run(
                    json!({"index_version": "v3", "full": false, "indexes": ["b1", "d2"], "new_index": "d2",
                       "status": "building", "batch_count": 3, "started_at": "2026-09-29T12:00:00Z"}),
                ),
            ],
            ..Summary::default()
        };
        s.ops.current_version = Some("v1".into());
        s.ops.writer = Some(LockSummary {
            owner: "caj-usnm-ingest-9-0badf00d".into(),
            until: at() + Duration::minutes(20),
        });
        s.ops.release_progress = Some(usnm_state::state::ReleaseProgress {
            index_version: "v3".into(),
            docs_sent: 250,
            docs_expected: 1000,
            mb_sent: 12.5,
            updated_at: at(),
        });
        let i = indexing(at(), &s);
        assert_eq!(
            i.runs
                .iter()
                .map(|r| r.index_version.as_str())
                .collect::<Vec<_>>(),
            ["v3", "v2", "v1"]
        );
        assert_eq!(i.runs[2].duration_secs, Some(3600));
        // An older run's inline list length, or a newer run's batch_count.
        assert_eq!(i.runs[2].batches, Some(2));
        assert_eq!(i.runs[0].batches, Some(3));
        assert_eq!(
            i.runs[1].error.as_deref(),
            Some("lock `quickwit-writer` is held by `[worker]` until later")
        );
        assert_eq!(i.failed_runs, 1);
        // v2 failed after v1 published, and nothing has published since.
        assert_eq!(i.failed_since_last_publish, 1);
        assert!(i.writer.held);
        assert_eq!(i.writer.holder.as_deref(), Some("adf00d"));
        assert_eq!(i.release.as_ref().unwrap().percent, 25.0);
        assert_eq!(
            i.last_published_at,
            Some("2026-09-28T11:00:00Z".parse().unwrap())
        );

        // An older run left `building` by a crash isn't the running one.
        let mut crashed = s.clone();
        crashed.runs.push(run(
            json!({"index_version": "v4", "full": false, "indexes": ["b1", "d3"],
            "new_index": "d3", "status": "failed", "started_at": "2026-09-29T12:20:00Z"}),
        ));
        assert!(indexing(at(), &crashed).release.is_none());

        // A run left `building` under a newer one was stopped; so is the newest
        // once no writer holds the lock.
        let mut stopped = s.clone();
        stopped.runs.push(run(
            json!({"index_version": "v0", "full": true, "indexes": ["b0"], "new_index": "b0",
            "status": "building", "started_at": "2026-09-27T12:00:00Z"}),
        ));
        let i = indexing(at(), &stopped);
        let state =
            |i: &Indexing, v: &str| i.runs.iter().find(|r| r.index_version == v).unwrap().status;
        assert_eq!(state(&i, "v0"), RunState::Stopped);
        assert_eq!(
            state(&i, &i.runs[0].index_version.clone()),
            RunState::Building
        );
        stopped.ops.writer.as_mut().unwrap().until = at() - Duration::minutes(1);
        let i = indexing(at(), &stopped);
        assert_eq!(
            state(&i, &i.runs[0].index_version.clone()),
            RunState::Stopped
        );

        // A version published after the failure supersedes it.
        let mut recovered = s.clone();
        recovered.runs.push(run(
            json!({"index_version": "v5", "full": false, "indexes": ["b1", "d4"], "new_index": "d4",
            "status": "published", "started_at": "2026-09-29T02:00:00Z",
            "published_at": "2026-09-29T02:30:00Z"}),
        ));
        let r = indexing(at(), &recovered);
        assert_eq!((r.failed_runs, r.failed_since_last_publish), (1, 0));

        // Progress from a release that is no longer running isn't shown.
        s.ops.writer.as_mut().unwrap().until = at() - Duration::minutes(1);
        let i = indexing(at(), &s);
        assert!(!i.writer.held && i.release.is_none() && i.writer.holder.is_none());
    }

    async fn fixture_refdata() -> RefData {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data");
        RefData::load(&usnm_store::LocalStore::new(dir))
            .await
            .unwrap()
    }

    fn place(id: &str, state: &str) -> crate::refdata::Place {
        crate::refdata::Place {
            id: id.into(),
            ordinal: 0,
            name: id.into(),
            state: state.into(),
            lat: 0.0,
            lon: 0.0,
            precision: "city".into(),
        }
    }

    fn title(lccn: &str, place_id: &str, languages: &[&str]) -> crate::refdata::Title {
        crate::refdata::Title {
            lccn: lccn.into(),
            name: lccn.into(),
            place_id: place_id.into(),
            state: String::new(),
            languages: languages.iter().map(|l| l.to_string()).collect(),
            first_year: None,
            last_year: None,
            place_periods: vec![],
        }
    }

    /// The fixture version with these places, titles and pages instead.
    async fn with(
        places: Vec<crate::refdata::Place>,
        titles: Vec<(crate::refdata::Title, u64)>,
    ) -> RefData {
        let mut rd = fixture_refdata().await;
        rd.place_index = places
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id.clone(), i))
            .collect();
        rd.places = places;
        rd.place_pages = HashMap::new();
        for (t, pages) in &titles {
            *rd.place_pages.entry(t.place_id.clone()).or_default() += pages;
        }
        rd.pages = titles.iter().map(|(_, n)| n).sum();
        rd.title_pages = Some(titles.iter().map(|(t, n)| (t.lccn.clone(), *n)).collect());
        rd.titles = titles
            .into_iter()
            .map(|(t, _)| (t.lccn.clone(), t))
            .collect();
        rd
    }

    #[tokio::test]
    async fn pages_by_year_add_up_to_the_versions_pages() {
        let rd = fixture_refdata().await;
        let p = published(&rd);
        assert!(!p.by_year.is_empty());
        assert_eq!(p.by_year.iter().map(|y| y.pages).sum::<u64>(), p.pages);
        assert!(p.by_year.windows(2).all(|w| w[0].year < w[1].year));
        assert!(p.by_year.iter().all(|y| y.pages > 0));
        assert_eq!(p.by_year.first().unwrap().year, 1895);
        // The fixture searches LoC's text alone.
        assert_eq!(
            p.sources,
            Sources {
                ocr_credits: None,
                ocr_ja: None,
                american_stories: None,
                ja_latin: None,
                ocr_local: None
            }
        );
    }

    #[tokio::test]
    async fn sources_count_the_texts_besides_locs() {
        use usnm_state::ocr_local::{Engine, Entry};
        use usnm_state::state::JaLatinPage;
        let mut rd = fixture_refdata().await;
        let lccn = rd.titles.keys().min().unwrap().clone();
        let name = rd.titles[&lccn].name.clone();
        let page = |reason: &str| JaLatinPage {
            batch: "b".into(),
            loc_text: reason.into(),
            ocr_source: "usnm-ndlocr".into(),
            ocr_engine: "ndlocr-lite".into(),
        };
        rd.ja_latin = HashMap::from([
            (format!("{lccn}_1942-01-01_ed-1_seq-1"), page("missing")),
            (format!("{lccn}_1942-01-01_ed-1_seq-2"), page("missing")),
            ("sn99999999_1942-01-01_ed-1_seq-1".into(), page("mixed")),
        ]);
        let engine = |source: &str, engine: &str| Engine {
            source: source.into(),
            engine: engine.into(),
        };
        let entry = |hash: u64, engine: u8| Entry {
            hash,
            engine,
            batch: 0,
        };
        rd.ocr_local = usnm_state::ocr_local::OcrLocalPages {
            engines: vec![
                engine("usnm-a", "a/1 x"),
                engine("usnm-b", "b/1 y"),
                engine("usnm-c", "c/1 z"),
            ],
            entries: vec![entry(1, 1), entry(2, 1), entry(3, 0)],
        };
        rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
        rd.american_stories_built = Some(crate::refdata::AmericanStoriesBuilt {
            years: 3,
            docs: 900,
            only_american_stories: 12,
        });
        let s = sources(&rd);
        let ja = s.ja_latin.unwrap();
        assert_eq!(
            ja.by_engine,
            vec![EnginePages {
                source: "usnm-ndlocr".into(),
                engine: "ndlocr-lite".into(),
                pages: 3
            }]
        );
        assert_eq!(ja.pages, 3);
        let reasons: Vec<_> = ja
            .by_reason
            .iter()
            .map(|r| (r.reason.as_str(), r.pages))
            .collect();
        assert_eq!(reasons, [("missing", 2), ("mixed", 1)]);
        assert_eq!(
            ja.by_title,
            [
                TitleOcrPages {
                    lccn: lccn.clone(),
                    name,
                    pages: 2
                },
                // Not in the catalog: named by its LCCN.
                TitleOcrPages {
                    lccn: "sn99999999".into(),
                    name: "sn99999999".into(),
                    pages: 1
                }
            ]
        );
        let local = s.ocr_local.unwrap();
        assert_eq!(local.pages, 3);
        // Most pages first; an engine with no pages is left out.
        let engines: Vec<_> = local
            .by_engine
            .iter()
            .map(|e| (e.engine.as_str(), e.pages))
            .collect();
        assert_eq!(engines, [("b/1 y", 2), ("a/1 x", 1)]);
        // The fixture has a delta: the manifest's count isn't the version's.
        assert_eq!(
            s.american_stories,
            Some(AmericanStories {
                searched: true,
                pages: None,
                only_pages: None,
                years: None
            })
        );
        rd.current.indexes.truncate(1);
        rd.american_stories_search = false;
        assert_eq!(
            sources(&rd).american_stories,
            Some(AmericanStories {
                searched: false,
                pages: Some(900),
                only_pages: Some(12),
                years: Some(3)
            })
        );
    }

    #[tokio::test]
    async fn pages_by_state_sum_each_states_places() {
        let rd = with(
            vec![
                place("P1", "IL"),
                place("P2", "IL"),
                place("P3", "NY"),
                place("P4", "XX"),
                place("P5", "DC"),
            ],
            vec![
                (title("a", "P1", &["eng"]), 600),
                (title("b", "P1", &["ger"]), 200),
                (title("c", "P2", &["eng"]), 100),
                (title("d", "P3", &["eng"]), 99),
                (title("e", "P4", &["eng"]), 1),
                (title("f", "P5", &["eng"]), 0),
            ],
        )
        .await;
        let rows = by_state(&rd);
        let got: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.state.as_str(),
                    r.name.as_str(),
                    r.places,
                    r.titles,
                    r.pages,
                    r.percent,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("IL", "Illinois", 2, 3, 900, 90.0),
                ("NY", "New York", 1, 1, 99, 9.9),
                // Not a known state: shown by its code.
                ("XX", "XX", 1, 1, 1, 0.1),
                ("DC", "District of Columbia", 1, 1, 0, 0.0),
            ]
        );
        assert_eq!(rows.iter().map(|r| r.pages).sum::<u64>(), rd.pages);
    }

    #[tokio::test]
    async fn a_title_that_moved_counts_in_each_state_it_was_in() {
        let mut moved = title("a", "P2", &["eng"]);
        moved.place_periods = serde_json::from_value(serde_json::json!([
            {"place_id": "P1", "to": "1833-11-13"},
            {"place_id": "P2", "from": "1833-11-14"},
        ]))
        .unwrap();
        // Two of its places are in Maine, one a town apart.
        let mut three = title("b", "P3", &["eng"]);
        three.place_periods = serde_json::from_value(serde_json::json!([
            {"place_id": "P1", "to": "1850-12-31"},
            {"place_id": "P3", "from": "1851-01-01", "to": "1860-12-31"},
            {"place_id": "P1", "from": "1861-01-01"},
        ]))
        .unwrap();
        let rd = with(
            vec![place("P1", "ME"), place("P2", "NH"), place("P3", "ME")],
            vec![(moved, 10), (three, 5), (title("c", "P2", &["eng"]), 1)],
        )
        .await;
        let got: Vec<_> = by_state(&rd)
            .iter()
            .map(|r| (r.state.clone(), r.places, r.titles))
            .collect();
        assert_eq!(
            got,
            [("NH".to_owned(), 1, 2), ("ME".to_owned(), 2, 2)],
            "a title counts once in a state however many of its places are there"
        );
    }

    #[tokio::test]
    async fn a_title_in_two_languages_counts_in_both() {
        let rd = with(
            vec![place("P1", "PA")],
            vec![
                (title("a", "P1", &["eng"]), 100),
                (title("b", "P1", &["eng", "ger"]), 40),
                (title("c", "P1", &["ger"]), 10),
                // No language, or only an empty one: "Not recorded".
                (title("d", "P1", &[]), 4),
                (title("g", "P1", &[""]), 1),
                // A repeated code counts once.
                (title("e", "P1", &["eng", "eng"]), 1),
                (title("f", "P1", &["pennsylvania german"]), 4),
            ],
        )
        .await;
        let l = by_language(&rd);
        assert!(l.pages_known);
        assert_eq!((l.multilingual_titles, l.multilingual_pages), (1, Some(40)));
        let got: Vec<_> = l
            .rows
            .iter()
            .map(|r| {
                (
                    r.code.as_deref(),
                    r.name.as_str(),
                    r.titles,
                    r.pages,
                    r.percent,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (Some("eng"), "English", 3, Some(141), Some(88.1)),
                (Some("ger"), "German", 2, Some(50), Some(31.3)),
                (None, "Not recorded", 2, Some(5), Some(3.1)),
                (
                    Some("pennsylvania german"),
                    "Pennsylvania German",
                    1,
                    Some(4),
                    Some(2.5)
                ),
            ]
        );
        // The bilingual title's pages are in both rows: more than the total.
        let rows: u64 = l.rows.iter().filter_map(|r| r.pages).sum();
        assert_eq!((rows, rd.pages), (200, 160));

        // An older snapshot without pages per title: newspapers only, most first.
        let mut old = rd;
        old.title_pages = None;
        let l = by_language(&old);
        assert!(!l.pages_known);
        assert_eq!((l.multilingual_titles, l.multilingual_pages), (1, None));
        let got: Vec<_> = l
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.titles, r.pages, r.percent))
            .collect();
        assert_eq!(
            got,
            [
                ("English", 3, None, None),
                ("German", 2, None, None),
                ("Not recorded", 2, None, None),
                ("Pennsylvania German", 1, None, None),
            ]
        );
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["rows"][0]["pages"], Value::Null);
        assert_eq!(v["multilingual_pages"], Value::Null);
    }

    #[tokio::test]
    async fn published_and_titles_sync() {
        let mut rd = fixture_refdata().await;
        let p = published(&rd);
        assert_eq!((p.deltas, p.max_deltas, p.next_release_full), (1, 8, false));
        assert_eq!(p.batches, None);

        let s = summary();
        let catalog = CatalogTitles {
            lccns: ["sn1", "sn2"].iter().map(|s| s.to_string()).collect(),
            places: 2,
        };
        // Without the published batch list, only the sync counts.
        let t = titles(&rd, Some(&catalog), "", Some(&s), "");
        let tp = t.pipeline.data.as_ref().unwrap();
        assert_eq!(tp.curated_titles, 3);
        assert_eq!(tp.awaiting_sync, Some(1));
        assert_eq!(tp.batches_waiting_for_titles, Some(1));
        assert_eq!(tp.unpublished_batches, None);

        rd.published_batches = Some(
            [("batch_a".to_owned(), 1), ("batch_r".to_owned(), 0)]
                .into_iter()
                .collect(),
        );
        let t = titles(&rd, Some(&catalog), "", Some(&s), "");
        let tp = t.pipeline.data.as_ref().unwrap();
        // b, c and old aren't published, and c waits for sn3; r was
        // published at another version than it is curated at now.
        assert_eq!(tp.unpublished_batches, Some(3));
        assert_eq!(tp.ready_for_release, Some(2));
        assert_eq!(tp.recurated_awaiting_full, Some(1));
        assert_eq!(t.catalog.data.as_ref().unwrap().titles, 2);
        // After 8 deltas the next release is full and takes batch_r too.
        rd.current.indexes = (0..9).map(|i| format!("i{i}")).collect();
        let t = titles(&rd, Some(&catalog), "", Some(&s), "");
        assert_eq!(t.pipeline.data.as_ref().unwrap().ready_for_release, Some(3));

        let t = titles(&rd, None, "no catalog", None, "no pipeline");
        assert!(!t.catalog.available && !t.pipeline.available);
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(
            v["pipeline"],
            json!({"available": false, "reason": "no pipeline"})
        );
    }
}
