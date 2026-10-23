use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use usnm_store::ObjectStore;

use super::{load_catalog, load_raw, save_raw, valid_lccn, RawTitle, TITLE_OVERRIDES};
use crate::activity::Reporter;
use crate::audit::{Audit, Earlier};
use crate::source;

/// One entry of [`TITLE_OVERRIDES`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleOverride {
    /// When it was decided (`YYYY-MM-DD`).
    pub added: String,
    /// Its earlier versions, oldest first (with `updated`).
    #[serde(default)]
    pub history: Vec<Earlier>,
    /// Why.
    pub reason: String,
    /// The LCCN of the LoC record that describes the title.
    pub record: String,
    /// The issue or pull request where it was decided.
    #[serde(rename = "ref")]
    pub reference: u32,
    /// Where the record was found.
    pub source: String,
    /// When it last changed (`YYYY-MM-DD`), with `history`.
    #[serde(default)]
    pub updated: Option<String>,
}

impl TitleOverride {
    pub fn audit(&self) -> Audit<'_> {
        Audit {
            added: &self.added,
            history: &self.history,
            reason: &self.reason,
            reference: self.reference,
            source: &self.source,
            updated: self.updated.as_deref(),
        }
    }
}

/// The record LCCN to fetch for each overridden title, from `json` in the
/// form of [`TITLE_OVERRIDES`].
pub fn record_lccns(json: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let entries: BTreeMap<String, TitleOverride> =
        serde_json::from_str(json).context("catalog/overrides/titles.json")?;
    let mut records = BTreeMap::new();
    for (lccn, o) in entries {
        o.audit()
            .check(&[("record", |v| v.as_str().is_some_and(valid_lccn))])
            .with_context(|| format!("catalog/overrides/titles.json: `{lccn}`"))?;
        anyhow::ensure!(
            valid_lccn(&lccn) && valid_lccn(&o.record) && o.record != lccn,
            "catalog/overrides/titles.json: `{lccn}` → `{}` isn't a pair of different, valid LCCNs",
            o.record
        );
        records.insert(lccn, o.record);
    }
    Ok(records)
}

/// The record LCCNs of the overrides in git.
pub fn title_records() -> anyhow::Result<BTreeMap<String, String>> {
    record_lccns(TITLE_OVERRIDES)
}

/// Parse LoC's item JSON (`{"item": {...}}`) for `lccn`.
pub fn parse_item(lccn: &str, bytes: &[u8]) -> anyhow::Result<RawTitle> {
    let v: Value = serde_json::from_slice(bytes).context("title record")?;
    let item = v.get("item").context("title record has no `item`")?;
    let strings = |key: &str| -> Vec<String> {
        match item.get(key) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_owned()],
            _ => vec![],
        }
    };
    let listed = strings("number_lccn");
    if !listed.is_empty() && !listed.iter().any(|l| l == lccn) {
        anyhow::bail!("record is for {listed:?}, not `{lccn}`");
    }
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .context("title record has no title")?
        .trim()
        .to_owned();
    let latlong = match item.get("latlong") {
        Some(Value::Array(a)) if a.len() == 2 => {
            let num = |x: &Value| {
                x.as_f64()
                    .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
            };
            match (num(&a[0]), num(&a[1])) {
                (Some(lat), Some(lon))
                    if (-90.0..=90.0).contains(&lat)
                        && (-180.0..=180.0).contains(&lon)
                        && (lat, lon) != (0.0, 0.0) =>
                {
                    Some([lat, lon])
                }
                _ => None,
            }
        }
        _ => None,
    };
    Ok(RawTitle {
        lccn: lccn.to_owned(),
        title,
        cities: strings("location_city"),
        states: strings("location_state"),
        latlong,
        dates: item
            .get("dates_of_publication")
            .and_then(Value::as_str)
            .map(str::to_owned),
        languages: strings("language"),
        record: None,
    })
}

/// loc.gov's JSON API allows 20 requests a minute and blocks for an hour
/// beyond that; this stays under it.
pub const LOC_INTERVAL: Duration = Duration::from_millis(3500);

/// After LoC rate limits us: past its one-hour block, with no request
/// during it (a request would start the hour again).
pub const LOC_BLOCK_PAUSE: Duration = Duration::from_secs(65 * 60);

/// The slowest pace after repeated blocks (about 6 requests a minute).
const MAX_INTERVAL: Duration = Duration::from_secs(10);

/// How `sync` paces its requests and what it does when LoC rate limits it.
#[derive(Debug, Clone)]
pub struct Pacing {
    /// Between the starts of two requests.
    pub interval: Duration,
    /// After a 429 or CAPTCHA page, how long to send nothing before
    /// trying again, 1.5 times slower.
    pub block_pause: Duration,
    /// When to stop: no request starts after it, and a pause that would
    /// end after it stops the sync instead. `None`: stop at the first block.
    pub deadline: Option<tokio::time::Instant>,
    /// Where the titles fetched so far and any pause are recorded for the
    /// status page (`ops/activity`).
    pub report: Reporter,
}

impl Pacing {
    /// loc.gov's pace, until `deadline`.
    pub fn loc(deadline: Option<tokio::time::Instant>) -> Self {
        Self {
            interval: LOC_INTERVAL,
            block_pause: LOC_BLOCK_PAUSE,
            deadline,
            report: Reporter::off(),
        }
    }

    /// The same, recording progress with `report`.
    pub fn reporting(self, report: Reporter) -> Self {
        Self { report, ..self }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct SyncReport {
    pub wanted: usize,
    pub fetched: usize,
    pub not_found: Vec<String>,
    pub failed: Vec<String>,
    /// Stopped early because LoC rate limited us; the next run continues.
    pub throttled: bool,
    /// Stopped early at the deadline; the next run continues.
    pub out_of_time: bool,
    /// Times LoC rate limited us and the sync waited it out.
    pub paused: u32,
    /// Titles not tried because the sync stopped early.
    pub left: usize,
}

impl SyncReport {
    /// Every wanted title was tried (some may be missing or have failed).
    pub fn finished(&self) -> bool {
        !self.throttled && !self.out_of_time
    }
}

/// Fetch the records of `lccns` that are neither cached nor already in the
/// catalog (all of them with `refresh`), one at a time as `pacing` says, and
/// save them to [`RAW`](super::RAW). A title in `records` is fetched from the record
/// named there and cached under its own LCCN. Failures are reported, not
/// fatal, and retried by the next run. Rate limiting pauses the sync, or
/// stops it (see [`Pacing`]).
pub async fn sync(
    reference: &dyn ObjectStore,
    lccns: &BTreeSet<String>,
    records: &BTreeMap<String, String>,
    refresh: bool,
    items_base: &str,
    pacing: &Pacing,
) -> anyhow::Result<SyncReport> {
    let mut raw = load_raw(reference).await?;
    let known: HashSet<String> = load_catalog(reference)
        .await?
        .0
        .into_iter()
        .map(|t| t.lccn)
        .collect();
    let todo: Vec<String> = lccns
        .iter()
        .filter(|l| refresh || (!raw.contains_key(*l) && !known.contains(*l)))
        .filter(|l| {
            let ok = valid_lccn(l);
            if !ok {
                tracing::warn!(lccn = %l, "skipping an invalid LCCN");
            }
            ok
        })
        .cloned()
        .collect();
    let mut report = SyncReport {
        wanted: todo.len(),
        ..Default::default()
    };
    let base = items_base.trim_end_matches('/');
    let mut interval = pacing.interval.max(Duration::from_millis(1));
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let past = |t: tokio::time::Instant| pacing.deadline.is_some_and(|d| t >= d);
    let mut unsaved = 0;
    let mut next = 0;
    let total = todo.len() as u64;
    pacing.report.progress(0, total);
    while let Some(lccn) = todo.get(next) {
        tick.tick().await;
        if past(tokio::time::Instant::now()) {
            tracing::warn!(
                left = todo.len() - next,
                "titles-sync deadline reached; stopping until the next run"
            );
            report.out_of_time = true;
            break;
        }
        let record = records.get(lccn).unwrap_or(lccn);
        let url = format!("{base}/{record}/?fo=json");
        let r = match source::get_json(&url).await {
            Ok(Some(b)) => parse_item(record, &b).map(|mut t| {
                if record != lccn {
                    t.lccn = lccn.clone();
                    t.record = Some(record.clone());
                }
                Some(t)
            }),
            Ok(None) => Ok(None),
            Err(e) if e.is::<source::Throttled>() => {
                if pacing.deadline.is_none()
                    || past(tokio::time::Instant::now() + pacing.block_pause)
                {
                    tracing::warn!(error = %e, left = todo.len() - next, "LoC is rate limiting; stopping until the next run");
                    report.throttled = true;
                    break;
                }
                // Keep what was fetched in case the replica stops meanwhile.
                if unsaved > 0 {
                    save_raw(reference, &raw).await?;
                    unsaved = 0;
                }
                interval = (interval * 3 / 2).min(MAX_INTERVAL.max(pacing.interval));
                tracing::warn!(
                    error = %e,
                    fetched = report.fetched,
                    left = todo.len() - next,
                    pause_secs = pacing.block_pause.as_secs(),
                    interval_ms = interval.as_millis() as u64,
                    "LoC is rate limiting; sending nothing for the pause, then resuming more slowly"
                );
                report.paused += 1;
                let until = chrono::Duration::from_std(pacing.block_pause)
                    .ok()
                    .map(|p| chrono::Utc::now() + p);
                pacing.report.paused_until(until).await;
                tokio::time::sleep(pacing.block_pause).await;
                pacing.report.paused_until(None).await;
                tick = tokio::time::interval(interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // The same title again.
                continue;
            }
            Err(e) => Err(e),
        };
        next += 1;
        pacing.report.progress(next as u64, total);
        match r {
            Ok(Some(t)) => {
                raw.insert(lccn.clone(), t);
                report.fetched += 1;
                unsaved += 1;
            }
            Ok(None) => report.not_found.push(lccn.clone()),
            Err(e) => {
                tracing::warn!(%lccn, error = %format!("{e:#}"), "title record fetch failed");
                report.failed.push(lccn.clone());
            }
        }
        // Save as we go, so a stopped run resumes where it was.
        if unsaved >= 100 {
            save_raw(reference, &raw).await?;
            unsaved = 0;
            tracing::info!(
                fetched = report.fetched,
                of = report.wanted,
                "title records"
            );
        }
    }
    report.left = todo.len() - next;
    if unsaved > 0 {
        save_raw(reference, &raw).await?;
    }
    report.not_found.sort();
    report.failed.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Catalog, Place, Title, PLACES, TITLES};
    use crate::titles::geocode;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn parses_loc_title_records() {
        let json = br#"{"item": {"number_lccn": ["sn85042252"],
            "title": "Black Republican (New York, N.Y.) 18??-18??",
            "location_city": ["new york"], "location_state": ["new york"],
            "latlong": [40.7130466, -74.0072301], "dates_of_publication": "18??-18??",
            "language": ["english"]}}"#;
        let t = parse_item("sn85042252", json).unwrap();
        assert_eq!(t.latlong, Some([40.7130466, -74.0072301]));
        assert_eq!(t.cities, ["new york"]);
        assert!(
            parse_item("sn1", json).is_err(),
            "a record for another title"
        );
        let no_ll = br#"{"item": {"title": "X", "latlong": ["", ""]}}"#;
        assert_eq!(parse_item("sn1", no_ll).unwrap().latlong, None);
    }

    /// No title overrides.
    fn no_title_overrides() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// No pace, and stop at the first block.
    fn unpaced() -> Pacing {
        Pacing {
            interval: Duration::ZERO,
            block_pause: Duration::ZERO,
            deadline: None,
            report: Reporter::off(),
        }
    }

    /// A loopback stand-in for LoC's item endpoint, counting requests.
    async fn fake_loc() -> (String, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://127.0.0.1:{}/item",
            listener.local_addr().unwrap().port()
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        // sn6 is rate limited once, then served.
        let sn6_hits = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let sn6_hits = sn6_hits.clone();
                tokio::spawn(async move {
                    let mut req = vec![0u8; 4096];
                    let n = sock.read(&mut req).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&req[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let (status, body) = if path.starts_with("/item/sn1/") {
                        ("200 OK", r#"{"item": {"number_lccn": ["sn1"], "title": "The Akron Beacon (Akron, Ohio) 1890-1900",
                            "location_city": ["akron"], "location_state": ["ohio"], "latlong": [41.08, -81.51],
                            "dates_of_publication": "1890-1900", "language": ["english"]}}"#.to_owned())
                    } else if path.starts_with("/item/sn6/") {
                        if sn6_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                            ("429 Too Many Requests", "<html>slow down</html>".to_owned())
                        } else {
                            ("200 OK", r#"{"item": {"number_lccn": ["sn6"], "title": "The Elko Free Press (Elko, Nev.) 1883-1900",
                                "location_city": ["elko"], "location_state": ["nevada"], "latlong": [40.83, -115.76],
                                "dates_of_publication": "1883-1900", "language": ["english"]}}"#.to_owned())
                        }
                    } else if path.starts_with("/item/sn84022797/") {
                        // The record of a title whose own LCCN has none.
                        ("200 OK", r#"{"item": {"number_lccn": ["sn84022797"],
                            "title": "The Vancouver Independent (Vancouver, Wash. Territory [i.e. Wash.]) 1875-1910",
                            "location_city": ["vancouver"], "location_state": ["washington"], "latlong": null,
                            "dates_of_publication": "1875-1910", "language": ["english"]}}"#.to_owned())
                    } else if path.starts_with("/item/sn2/") {
                        ("200 OK", "{not json".to_owned())
                    } else if path.starts_with("/item/sn8/") {
                        ("429 Too Many Requests", "<html>slow down</html>".to_owned())
                    } else if path.starts_with("/item/sn7/") {
                        // A challenge page with a non-429 status.
                        ("403 Forbidden", "<html>are you human?</html>".to_owned())
                    } else {
                        ("404 Not Found", String::new())
                    };
                    let ctype = if body.starts_with('<') {
                        "text/html"
                    } else {
                        "application/json"
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (base, hits)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn syncs_records_and_rebuilds_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let store = usnm_store::open(dir.path().to_str().unwrap()).unwrap();
        // A catalog title that is already known is never fetched.
        let known = Title {
            lccn: "sn9".into(),
            name: "Known".into(),
            ordinal: 7,
            place_id: "P00003".into(),
            state: "IL".into(),
            languages: vec![],
            extra: BTreeMap::new(),
        };
        let place = Place {
            id: "P00003".into(),
            ordinal: 3,
            name: "Chicago".into(),
            state: "IL".into(),
            lat: 41.88,
            lon: -87.63,
            precision: "city".into(),
            coordinates_from: None,
            variants: vec![],
        };
        store
            .put(
                TITLES,
                serde_json::to_vec(&[&known]).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        store
            .put(
                PLACES,
                serde_json::to_vec(&[&place]).unwrap(),
                "application/json",
            )
            .await
            .unwrap();

        let (base, hits) = fake_loc().await;
        let lccns: BTreeSet<String> = ["sn1", "sn2", "sn3", "sn9", "BAD/x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let r = sync(
            store.as_ref(),
            &lccns,
            &no_title_overrides(),
            false,
            &base,
            &unpaced(),
        )
        .await
        .unwrap();
        assert_eq!((r.wanted, r.fetched), (3, 1));
        assert_eq!(r.not_found, ["sn3"]);
        assert_eq!(r.failed, ["sn2"]);
        let g = geocode(store.as_ref()).await.unwrap();
        assert!(g.written);
        let c = Catalog::load(store.as_ref()).await.unwrap();
        assert_eq!(c.title("sn9").unwrap(), &known);
        let akron = c.title("sn1").unwrap();
        assert_eq!(
            (akron.name.as_str(), akron.ordinal, akron.state.as_str()),
            ("The Akron Beacon", 8, "OH")
        );
        let p = c.place(&akron.place_id).unwrap();
        assert_eq!(
            (p.id.as_str(), p.name.as_str(), p.lat),
            ("P00004", "Akron", 41.08)
        );

        // A second run only retries what failed, and changes nothing.
        let before = hits.load(Ordering::SeqCst);
        let r = sync(
            store.as_ref(),
            &lccns,
            &no_title_overrides(),
            false,
            &base,
            &unpaced(),
        )
        .await
        .unwrap();
        assert_eq!((r.wanted, r.fetched), (2, 0));
        assert!(hits.load(Ordering::SeqCst) - before >= 2);
        assert!(!geocode(store.as_ref()).await.unwrap().written);

        // Rate limiting stops the run at once: nothing after it is requested.
        let before = hits.load(Ordering::SeqCst);
        let limited: BTreeSet<String> = ["sn8", "sn99"].iter().map(|s| s.to_string()).collect();
        let r = sync(
            store.as_ref(),
            &limited,
            &no_title_overrides(),
            false,
            &base,
            &unpaced(),
        )
        .await
        .unwrap();
        assert!(r.throttled);
        assert_eq!((r.wanted, r.fetched, r.not_found.len()), (2, 0, 0));
        assert_eq!(hits.load(Ordering::SeqCst) - before, 1);

        // So does an HTML challenge page, whatever its status.
        let before = hits.load(Ordering::SeqCst);
        let challenged: BTreeSet<String> = ["sn7", "sn99"].iter().map(|s| s.to_string()).collect();
        let r = sync(
            store.as_ref(),
            &challenged,
            &no_title_overrides(),
            false,
            &base,
            &unpaced(),
        )
        .await
        .unwrap();
        assert!(r.throttled);
        assert_eq!(hits.load(Ordering::SeqCst) - before, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn with_a_deadline_a_block_is_waited_out() {
        let dir = tempfile::tempdir().unwrap();
        let store = usnm_store::open(dir.path().to_str().unwrap()).unwrap();
        let (base, hits) = fake_loc().await;
        let set = |v: &[&str]| -> BTreeSet<String> { v.iter().map(|s| s.to_string()).collect() };
        let now = tokio::time::Instant::now();

        // Rate limited once: nothing is sent during the pause, then the same
        // title is asked for again and the sync goes on.
        let pacing = Pacing {
            interval: Duration::ZERO,
            block_pause: Duration::from_millis(300),
            deadline: Some(now + Duration::from_secs(30)),
            report: Reporter::off(),
        };
        let started = std::time::Instant::now();
        let r = sync(
            store.as_ref(),
            &set(&["sn6", "sn3"]),
            &no_title_overrides(),
            false,
            &base,
            &pacing,
        )
        .await
        .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(r.finished(), "{r:?}");
        assert_eq!((r.wanted, r.fetched, r.paused, r.left), (2, 1, 1, 0));
        assert_eq!(r.not_found, ["sn3"]);
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        // A pause that would end after the deadline stops the sync instead.
        let before = hits.load(Ordering::SeqCst);
        let pacing = Pacing {
            interval: Duration::ZERO,
            block_pause: Duration::from_secs(3600),
            deadline: Some(now + Duration::from_secs(30)),
            report: Reporter::off(),
        };
        let r = sync(
            store.as_ref(),
            &set(&["sn8", "sn99"]),
            &no_title_overrides(),
            false,
            &base,
            &pacing,
        )
        .await
        .unwrap();
        assert!(r.throttled && !r.finished());
        assert_eq!((r.paused, r.left), (0, 2));
        assert_eq!(hits.load(Ordering::SeqCst) - before, 1);

        // Past the deadline, nothing more is asked for.
        let before = hits.load(Ordering::SeqCst);
        let pacing = Pacing {
            deadline: Some(now),
            ..unpaced()
        };
        let r = sync(
            store.as_ref(),
            &set(&["sn98", "sn99"]),
            &no_title_overrides(),
            false,
            &base,
            &pacing,
        )
        .await
        .unwrap();
        assert!(r.out_of_time && !r.finished());
        assert_eq!((r.wanted, r.fetched, r.left), (2, 0, 2));
        assert_eq!(hits.load(Ordering::SeqCst), before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_title_without_its_own_record_takes_the_one_its_override_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = usnm_store::open(dir.path().to_str().unwrap()).unwrap();
        let (base, _) = fake_loc().await;
        let lccns: BTreeSet<String> = ["sn87093109"].iter().map(|s| s.to_string()).collect();

        // Without the override, LoC has nothing under the title's LCCN.
        let r = sync(
            store.as_ref(),
            &lccns,
            &no_title_overrides(),
            false,
            &base,
            &unpaced(),
        )
        .await
        .unwrap();
        assert_eq!(r.not_found, ["sn87093109"]);

        let records = record_lccns(TITLE_OVERRIDES).unwrap();
        assert_eq!(records["sn87093109"], "sn84022797");
        let r = sync(store.as_ref(), &lccns, &records, false, &base, &unpaced())
            .await
            .unwrap();
        assert_eq!((r.wanted, r.fetched), (1, 1), "{r:?}");
        let raw = load_raw(store.as_ref()).await.unwrap();
        assert!(!raw.contains_key("sn84022797"));
        let t = &raw["sn87093109"];
        assert_eq!(
            (t.lccn.as_str(), t.record.as_deref()),
            ("sn87093109", Some("sn84022797"))
        );

        // The catalog has it under the title's LCCN, placed from the record.
        assert!(geocode(store.as_ref()).await.unwrap().written);
        let c = Catalog::load(store.as_ref()).await.unwrap();
        let t = c.title("sn87093109").unwrap();
        assert_eq!(
            (t.name.as_str(), t.state.as_str(), t.languages.as_slice()),
            (
                "The Vancouver Independent",
                "WA",
                ["eng".to_owned()].as_slice()
            )
        );
        assert_eq!(t.extra["loc_record"], "sn84022797");
        let p = c.place(&t.place_id).unwrap();
        assert_eq!((p.name.as_str(), p.state.as_str()), ("Vancouver", "WA"));
        // Neither LoC record has coordinates: the override in git places it
        // in the city, not at Washington's centroid.
        assert_eq!(
            (p.lat, p.lon, p.precision.as_str()),
            (45.6387, -122.6615, "city")
        );

        // Once catalogued, it isn't asked for again.
        let r = sync(store.as_ref(), &lccns, &records, false, &base, &unpaced())
            .await
            .unwrap();
        assert_eq!(r.wanted, 0);
    }

    #[test]
    fn title_overrides_must_name_another_valid_lccn() {
        let entry = |record: &str, more: &str| {
            format!(
                r#"{{"sn1": {{"added": "2026-10-05", "reason": "Why.", "record": "{record}", "ref": 166, "source": "Where.",{more} "updated": null}}}}"#
            )
        };
        assert_eq!(record_lccns(&entry("sn2", "")).unwrap()["sn1"], "sn2");
        assert!(record_lccns(&entry("sn1", "")).is_err());
        assert!(record_lccns(&entry("BAD/x", "")).is_err());
        assert!(record_lccns(&entry("sn2", r#" "place": "x","#)).is_err());
        // The audit trail is checked; a bare note no longer does.
        assert!(record_lccns(&entry("sn2", "").replace("Where.", "")).is_err());
        assert!(record_lccns(&entry("sn2", "").replace("166", "-1")).is_err());
        assert!(record_lccns(r#"{"sn1": {"note": "why", "record": "sn2"}}"#).is_err());
        // An earlier version's record is an LCCN too.
        let changed = |earlier: &str| {
            format!(
                r#"{{"sn1": {{"added": "2026-10-05", "history": [{{"reason": "Old.", "record": {earlier}, "ref": 120, "source": "Old.", "updated": "2026-10-05"}}], "reason": "Why.", "record": "sn2", "ref": 166, "source": "Where.", "updated": "2026-10-06"}}}}"#
            )
        };
        assert!(record_lccns(&changed(r#""sn3""#)).is_ok());
        assert!(record_lccns(&changed("123")).is_err());
        assert!(record_lccns(&changed(r#""BAD/x""#)).is_err());
    }

    #[test]
    fn the_title_overrides_in_git_parse_with_sorted_keys() {
        let records = title_records().unwrap();
        assert!(!records.is_empty());
        crate::audit::assert_keys_sorted(TITLE_OVERRIDES);
        // Keyed JSON in git keeps its keys in ascending order: read the
        // object's keys in file order with a JSON parser (a text search could
        // match inside a value), then compare with the sorted map's.
        let in_file = object_keys_in_order(TITLE_OVERRIDES);
        assert_eq!(
            in_file,
            records.keys().cloned().collect::<Vec<_>>(),
            "keys out of order"
        );
    }

    /// The top-level keys of a JSON object, in the order the text has them.
    fn object_keys_in_order(json: &str) -> Vec<String> {
        struct Keys(Vec<String>);
        impl<'de> serde::Deserialize<'de> for Keys {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> serde::de::Visitor<'de> for V {
                    type Value = Keys;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a JSON object")
                    }
                    fn visit_map<A: serde::de::MapAccess<'de>>(
                        self,
                        mut m: A,
                    ) -> Result<Keys, A::Error> {
                        let mut keys = Vec::new();
                        while let Some(k) = m.next_key::<String>()? {
                            m.next_value::<serde::de::IgnoredAny>()?;
                            keys.push(k);
                        }
                        Ok(Keys(keys))
                    }
                }
                d.deserialize_map(V)
            }
        }
        serde_json::from_str::<Keys>(json).unwrap().0
    }

    #[test]
    fn keys_are_read_in_file_order_not_by_text_search() {
        let json = r#"{"b": {"note": "\"a\": inside a value"}, "a": {}}"#;
        assert_eq!(object_keys_in_order(json), ["b", "a"]);
    }
}
