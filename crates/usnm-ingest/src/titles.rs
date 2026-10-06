//! `titles-sync` and `geocode` (04 §4.6): the working catalog
//! (`catalog/titles.json`, `catalog/places.json`) from LoC's title records.
//!
//! - **titles-sync** fetches `https://www.loc.gov/item/{lccn}/?fo=json` for
//!   every title that has pages (LoC's batch listing names them) and isn't
//!   cached yet, keeping the fields we use in `raw/titles.json`. Then it
//!   runs `geocode`. Requests are paced under loc.gov's JSON API limit (20 a
//!   minute; exceeding it blocks for an hour, and any request during the
//!   block starts the hour again). LoC also limits below that when it is
//!   busy: in prod, a 429 or CAPTCHA page came after about 340 requests at
//!   about 13 a minute. With a deadline, the sync then sends nothing for 65
//!   minutes and resumes 1.5 times slower; without one, or when the pause
//!   would pass it, the run stops early. The cache keeps what was fetched,
//!   so the next run continues. A title takes about 4.5 s (LoC's response
//!   time, over the 3.5 s pace), so ~3,100 titles take about 4 hours, plus
//!   an hour for each block. A few titles have no record under the LCCN
//!   their pages ship with, but one under another LCCN:
//!   `catalog/overrides/titles.json` names it, and the sync fetches that
//!   record for the title instead.
//! - **geocode** builds the catalog from that cache alone, so it is
//!   deterministic and needs no network. A title's place is the city in its
//!   LoC title, e.g. "(North Platte, Neb.)", in the state LoC lists at the
//!   city's position (or `catalog/overrides/title-places.json`); spellings of one town share a
//!   place ([`crate::places`]: generic variants, `catalog/overrides/place-aliases.json`,
//!   and "X City" next to "X"). Coordinates are, in order: a manual override
//!   (`catalog/overrides/places.json` in git); the median of LoC's
//!   `latlong` on records that list only this city (unless it is more than
//!   100 km from the state's only gazetteer town of the name); the US gazetteer; the
//!   median of LoC's `latlong` on records that list several cities (one
//!   point for all of them, so a last resort); else the state's centroid
//!   (precision `state`).
//!
//! Ordinals and place ids are stable: records already in the catalog keep
//! them, new ones get the next free number, and nothing is ever removed
//! (published snapshots keep their own copies, 04 §4.7). Merged places keep
//! the id most of their titles had; the others stay, named by no title.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use usnm_core::names::{language_code, state_by_name, State, STATES};
use usnm_store::ObjectStore;

use crate::activity::Reporter;
use crate::catalog::{Catalog, Place, Title, PLACES, TITLES};
use crate::places::{self, CityName, Geo};
use crate::source;

/// LoC's item endpoint; a title record is `{base}/{lccn}/?fo=json`.
pub const LOC_ITEMS: &str = "https://www.loc.gov/item";
/// The fields of every fetched title record, by LCCN.
pub const RAW: &str = "raw/titles.json";
/// Manual coordinates, `[{city, lat, lon, name?, precision?, state}]`, kept in git
/// (`catalog/overrides/places.json`) and compiled in, so every environment
/// applies the same corrections (08 §8.9).
pub const PLACE_OVERRIDES: &str = include_str!("../../../catalog/overrides/places.json");
/// Titles whose LoC record is under another LCCN, `{lccn: {record, note?}}`,
/// kept in git (`catalog/overrides/titles.json`) and compiled in like the
/// place overrides.
pub const TITLE_OVERRIDES: &str = include_str!("../../../catalog/overrides/titles.json");

/// One entry of [`TITLE_OVERRIDES`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleOverride {
    /// The LCCN of the LoC record that describes the title.
    pub record: String,
    /// Why, and where that was found.
    #[serde(default)]
    pub note: Option<String>,
}

/// The record LCCN to fetch for each overridden title, from `json` in the
/// form of [`TITLE_OVERRIDES`].
pub fn record_lccns(json: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let entries: BTreeMap<String, TitleOverride> =
        serde_json::from_str(json).context("catalog/overrides/titles.json")?;
    let mut records = BTreeMap::new();
    for (lccn, o) in entries {
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

/// What `geocode` uses from a LoC title record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawTitle {
    pub lccn: String,
    /// e.g. "The North Platte Tribune (North Platte, Neb.) 1890-1894".
    pub title: String,
    #[serde(default)]
    pub cities: Vec<String>,
    #[serde(default)]
    pub states: Vec<String>,
    #[serde(default)]
    pub latlong: Option<[f64; 2]>,
    #[serde(default)]
    pub dates: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    /// The LCCN of the record this came from, when it isn't the title's own
    /// (`catalog/overrides/titles.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
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
/// save them to [`RAW`]. A title in `records` is fetched from the record
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

fn valid_lccn(lccn: &str) -> bool {
    usnm_core::ids::PageKey::new(lccn, chrono::NaiveDate::MIN, 1, 1).is_ok()
}

async fn load_raw(store: &dyn ObjectStore) -> anyhow::Result<BTreeMap<String, RawTitle>> {
    Ok(match store.get(RAW).await? {
        Some(b) => serde_json::from_slice(&b).context(RAW)?,
        None => BTreeMap::new(),
    })
}

async fn save_raw(store: &dyn ObjectStore, raw: &BTreeMap<String, RawTitle>) -> anyhow::Result<()> {
    store
        .put(RAW, serde_json::to_vec(raw)?, "application/json")
        .await?;
    Ok(())
}

/// The working catalog as stored, or empty before the first sync.
async fn load_catalog(store: &dyn ObjectStore) -> anyhow::Result<(Vec<Title>, Vec<Place>)> {
    let titles = match store.get(TITLES).await? {
        Some(b) => serde_json::from_slice(&b).context(TITLES)?,
        None => vec![],
    };
    let places = match store.get(PLACES).await? {
        Some(b) => serde_json::from_slice(&b).context(PLACES)?,
        None => vec![],
    };
    Ok((titles, places))
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaceOverride {
    pub city: String,
    /// Postal code, e.g. `MD`.
    pub state: String,
    pub lat: f64,
    pub lon: f64,
    /// The place's name, when neither an alias, the gazetteer nor LoC has
    /// it right.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "city_precision")]
    pub precision: String,
}

fn city_precision() -> String {
    "city".into()
}

/// A place whose LoC point (from records that list only its city) is
/// further than this from the gazetteer's is listed for review.
pub const DISAGREE_KM: f64 = 25.0;
/// "X City" and "X" in one state are one place when their points are
/// within this.
pub const CITY_SUFFIX_KM: f64 = 10.0;
/// A LoC point (from single-city records) further than this from the
/// gazetteer's only town of the name in the state gives way to the
/// gazetteer's.
pub const FAR_KM: f64 = 100.0;
/// The report lists at most this many disagreements.
const MAX_LISTED: usize = 50;

#[derive(Debug, Default, Serialize)]
pub struct GeocodeReport {
    pub titles: usize,
    pub new_titles: usize,
    pub places: usize,
    pub new_places: usize,
    /// Places by how their coordinates were found (04 §4.6).
    pub from_override: usize,
    /// LoC's `latlong` on records that list only this city.
    pub from_loc: usize,
    pub from_gazetteer: usize,
    /// The gazetteer's point over LoC's single-city point, more than
    /// [`FAR_KM`] away (the name has one town in the state), and which.
    pub gazetteer_over_loc: usize,
    pub gazetteer_over_loc_places: Vec<String>,
    /// LoC's `latlong` on records that list several cities: one point for
    /// all of them, so only a last resort.
    pub from_loc_multi_city: usize,
    pub state_centroid: usize,
    /// Titles whose city an alias renamed.
    pub aliased: usize,
    /// Places some title named before this build and none names now
    /// (folded into another place). They stay in the catalog, so their ids
    /// are never reused.
    pub retired: Vec<String>,
    /// Places whose LoC point is more than [`DISAGREE_KM`] from the
    /// gazetteer's (LoC's is used), and the furthest of them.
    pub disagreements: usize,
    pub disagreement_examples: Vec<String>,
    /// Titles whose state couldn't be determined (left out).
    pub unresolved: Vec<String>,
    pub written: bool,
}

/// Rebuild the catalog from the cache and overrides, and write it if it
/// changed (places first, so titles never name a place that isn't there).
pub async fn geocode(reference: &dyn ObjectStore) -> anyhow::Result<GeocodeReport> {
    let raw = load_raw(reference).await?;
    let (titles, places) = load_catalog(reference).await?;
    let overrides: Vec<PlaceOverride> =
        serde_json::from_str(PLACE_OVERRIDES).context("catalog/overrides/places.json")?;
    let geo = Geo::compiled()?;
    let (catalog, mut report) = build(&raw, titles.clone(), places.clone(), &overrides, geo)?;
    let new_places = serde_json::to_vec_pretty(&catalog.places)?;
    let new_titles = serde_json::to_vec_pretty(&catalog.titles)?;
    // Compare against the stored files after the same sort Catalog applies.
    let (old_titles, old_places) = match Catalog::new(titles, places) {
        Ok(c) => (
            serde_json::to_vec_pretty(&c.titles)?,
            serde_json::to_vec_pretty(&c.places)?,
        ),
        Err(_) => (vec![], vec![]),
    };
    if new_places != old_places || new_titles != old_titles {
        reference
            .put(PLACES, new_places, "application/json")
            .await?;
        reference
            .put(TITLES, new_titles, "application/json")
            .await?;
        report.written = true;
    }
    Ok(report)
}

/// One title's catalog facts, derived from its record.
struct Derived<'a> {
    raw: &'a RawTitle,
    name: String,
    place_label: Option<String>,
    /// Display name of the city, if the record has one.
    city: Option<String>,
    state: &'static State,
}

/// Where a place's coordinates came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Override,
    Loc,
    Gazetteer,
    /// The gazetteer's only town of the name, over a LoC point more than
    /// [`FAR_KM`] from it.
    GazetteerOverLoc,
    LocMultiCity,
    StateCentroid,
}

/// A place's coordinates.
struct Point {
    lat: f64,
    lon: f64,
    precision: String,
    source: Source,
    /// An override's name for the place.
    name: Option<String>,
    /// LoC's point and the gazetteer's, when both exist.
    check: Option<((f64, f64), (f64, f64))>,
}

/// The titles of one town: every spelling ("variant") that keys alike,
/// plus an "X City" next to an "X" (or the other way round).
struct Group {
    /// The variant with the most titles.
    key: String,
    code: &'static str,
    /// Its key first.
    variants: Vec<String>,
    /// Indexes into the derived titles, in LCCN order.
    members: Vec<usize>,
}

/// What resolving a place's coordinates looks at.
struct Places<'a> {
    derived: &'a [Derived<'a>],
    /// Each title's city, keyed; `None` for a title of a whole state.
    names: &'a [Option<CityName>],
    overrides: HashMap<(String, &'static str), &'a PlaceOverride>,
    geo: &'a Geo,
}

impl Places<'_> {
    /// Coordinates for the titles `members` of a place known by `keys`
    /// (04 §4.6): an override; else the median of LoC's points on records
    /// that list only this city; else the gazetteer; else the median of
    /// LoC's points on records that list several cities; else the state's
    /// centroid.
    fn resolve(&self, keys: &[String], members: &[usize], code: &'static str) -> Point {
        let point = |lat, lon, precision: &str, source| Point {
            lat,
            lon,
            precision: precision.to_owned(),
            source,
            name: None,
            check: None,
        };
        if let Some(o) = keys
            .iter()
            .find_map(|k| self.overrides.get(&(k.clone(), code)))
        {
            return Point {
                name: o.name.clone(),
                ..point(o.lat, o.lon, &o.precision, Source::Override)
            };
        }
        let state = self.derived[members[0]].state;
        let centroid = point(state.lat, state.lon, "state", Source::StateCentroid);
        if self.names[members[0]].is_none() {
            return centroid;
        }
        let (mut single, mut multi) = (Vec::new(), Vec::new());
        for &i in members {
            let d = &self.derived[i];
            let (Some([lat, lon]), Some(own)) = (d.raw.latlong, &self.names[i]) else {
                continue;
            };
            // A record lists one latlong however many cities it lists: it
            // is this city's only if the record lists no other.
            let only_this = d
                .raw
                .cities
                .iter()
                .all(|c| self.geo.city(&title_case(c), d.state).key == own.key);
            if only_this {
                single.push((lat, lon));
            } else {
                multi.push((lat, lon));
            }
        }
        let gazetteer = &self.geo.gazetteer;
        // The town with this key, and whether it is the state's only one.
        let exact = keys.iter().find_map(|k| {
            gazetteer
                .get(code, k)
                .map(|g| ((g.lat, g.lon), gazetteer.count(code, k) == 1))
        });
        let gaz = exact.map(|(g, _)| g).or_else(|| {
            keys.iter()
                .find_map(|k| gazetteer.near(code, k))
                .map(|g| (g.lat, g.lon))
        });
        if let Some((lat, lon)) = median(&single) {
            // LoC's point far from the state's only town of the name is
            // LoC's mistake (a state's or another town's point).
            if let Some((g, true)) = exact {
                if places::distance_km((lat, lon), g) > FAR_KM {
                    return Point {
                        check: Some(((lat, lon), g)),
                        ..point(g.0, g.1, "city", Source::GazetteerOverLoc)
                    };
                }
            }
            return Point {
                check: gaz.map(|g| ((lat, lon), g)),
                ..point(lat, lon, "city", Source::Loc)
            };
        }
        if let Some((lat, lon)) = gaz {
            return point(lat, lon, "city", Source::Gazetteer);
        }
        if let Some((lat, lon)) = median(&multi) {
            return point(lat, lon, "city", Source::LocMultiCity);
        }
        centroid
    }
}

/// A place whose LoC and gazetteer points are apart: km, name, state, LoC's
/// point, the gazetteer's.
type Gap<'a> = (f64, String, &'a str, (f64, f64), (f64, f64));

/// The median latitude and longitude, each on its own.
fn median(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let mid = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        }
    };
    Some((
        mid(points.iter().map(|p| p.0).collect()),
        mid(points.iter().map(|p| p.1).collect()),
    ))
}

/// The pure part of [`geocode`].
pub fn build(
    raw: &BTreeMap<String, RawTitle>,
    titles: Vec<Title>,
    places: Vec<Place>,
    overrides: &[PlaceOverride],
    geo: &Geo,
) -> anyhow::Result<(Catalog, GeocodeReport)> {
    let mut report = GeocodeReport::default();
    let derived: Vec<Derived> = raw
        .values()
        .filter_map(|r| {
            let d = derive(r, geo);
            if d.is_none() {
                report.unresolved.push(r.lccn.clone());
            }
            d
        })
        .collect();

    // Each title's city, cleaned, renamed by any alias, and keyed.
    let names: Vec<Option<CityName>> = derived
        .iter()
        .map(|d| {
            d.city
                .as_deref()
                .map(|c| geo.city(c, d.state))
                .filter(|n| !n.key.is_empty())
        })
        .collect();
    report.aliased = names.iter().flatten().filter(|n| n.alias.is_some()).count();

    // Spellings that key alike are one variant; "" is the state itself.
    let mut variants: BTreeMap<(String, &'static str), Vec<usize>> = BTreeMap::new();
    for (i, d) in derived.iter().enumerate() {
        let key = names[i].as_ref().map(|n| n.key.clone()).unwrap_or_default();
        variants.entry((key, d.state.code)).or_default().push(i);
    }
    let ctx = Places {
        derived: &derived,
        names: &names,
        overrides: overrides
            .iter()
            .filter_map(|o| {
                let state = STATES
                    .iter()
                    .find(|s| s.code.eq_ignore_ascii_case(&o.state))?;
                Some(((geo.city(&o.city, state).key, state.code), o))
            })
            .collect(),
        geo,
    };

    // "X City" next to "X" in the same state is one town when their points
    // agree, under the name with more titles (on a tie, "X").
    let mut joins: HashMap<(String, &'static str), (String, &'static str)> = HashMap::new();
    for ((key, code), members) in &variants {
        let Some(base) = key.strip_suffix("city").filter(|b| !b.is_empty()) else {
            continue;
        };
        let base_key = (base.to_owned(), *code);
        let Some(base_members) = variants.get(&base_key) else {
            continue;
        };
        let a = ctx.resolve(std::slice::from_ref(key), members, code);
        let b = ctx.resolve(std::slice::from_ref(&base_key.0), base_members, code);
        let placed = |p: &Point| p.source != Source::StateCentroid;
        if !placed(&a)
            || !placed(&b)
            || places::distance_km((a.lat, a.lon), (b.lat, b.lon)) > CITY_SUFFIX_KM
        {
            continue;
        }
        let this = (key.clone(), *code);
        if members.len() > base_members.len() {
            joins.insert(base_key, this);
        } else {
            joins.insert(this, base_key);
        }
    }
    let mut groups: BTreeMap<(String, &'static str), Group> = BTreeMap::new();
    for ((key, code), members) in variants {
        let mut target = (key.clone(), code);
        for _ in 0..4 {
            match joins.get(&target) {
                Some(next) => target = next.clone(),
                None => break,
            }
        }
        let g = groups.entry(target.clone()).or_insert_with(|| Group {
            key: target.0.clone(),
            code,
            variants: vec![target.0.clone()],
            members: Vec::new(),
        });
        if key != g.key {
            g.variants.push(key);
        }
        g.members.extend(members);
    }
    let groups: Vec<Group> = groups
        .into_values()
        .map(|mut g| {
            g.members.sort_unstable();
            g
        })
        .collect();

    // Stored places: each goes to the group with most of its titles (else
    // the one its name keys to), and each group keeps the place with most
    // of its titles (then the lowest ordinal). The others stay as they are.
    let stored_place_of: HashMap<&str, &str> = titles
        .iter()
        .map(|t| (t.lccn.as_str(), t.place_id.as_str()))
        .collect();
    let index: HashMap<&str, usize> = places
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.as_str(), i))
        .collect();
    let mut by_key: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, p) in places.iter().enumerate() {
        if let Some(k) = place_key(p, geo) {
            by_key.entry(k).or_default().push(i);
        }
    }
    type Rank = (usize, bool, std::cmp::Reverse<usize>);
    let mut claims: HashMap<usize, (Rank, usize)> = HashMap::new();
    let mut counts: Vec<HashMap<usize, usize>> = Vec::with_capacity(groups.len());
    for (gi, g) in groups.iter().enumerate() {
        let mut c: HashMap<usize, (usize, bool)> = HashMap::new();
        for &m in &g.members {
            // Only a place in the group's state: a title that moved state
            // starts a place there.
            if let Some(&pi) = stored_place_of
                .get(derived[m].raw.lccn.as_str())
                .and_then(|id| index.get(id))
                .filter(|&&pi| places[pi].state == g.code)
            {
                c.entry(pi).or_default().0 += 1;
            }
        }
        for v in &g.variants {
            for &pi in by_key
                .get(&(v.clone(), g.code.to_owned()))
                .into_iter()
                .flatten()
            {
                c.entry(pi).or_default().1 = true;
            }
        }
        for (&pi, &(n, named)) in &c {
            let rank = (n, named, std::cmp::Reverse(gi));
            if claims.get(&pi).is_none_or(|(r, _)| rank > *r) {
                claims.insert(pi, (rank, gi));
            }
        }
        counts.push(c.into_iter().map(|(pi, (n, _))| (pi, n)).collect());
    }
    let mut chosen: Vec<Option<usize>> = vec![None; groups.len()];
    for (&pi, &(_, gi)) in &claims {
        let rank = |p: usize| (counts[gi][&p], std::cmp::Reverse(places[p].ordinal));
        if chosen[gi].is_none_or(|cur| rank(pi) > rank(cur)) {
            chosen[gi] = Some(pi);
        }
    }
    // The places a group took but didn't keep (folded into its place).
    let mut folded: Vec<Vec<usize>> = vec![Vec::new(); groups.len()];
    for (&pi, &(_, gi)) in &claims {
        if chosen[gi] != Some(pi) {
            folded[gi].push(pi);
        }
    }

    let referenced_before: BTreeSet<String> = titles.iter().map(|t| t.place_id.clone()).collect();
    let mut next_place = places.iter().map(|p| p.ordinal).max().unwrap_or(0);
    let mut used_ids: HashSet<String> = places.iter().map(|p| p.id.clone()).collect();
    let mut slots: Vec<Option<Place>> = places.into_iter().map(Some).collect();
    let mut kept = Vec::new();
    let mut place_of: HashMap<usize, String> = HashMap::new();
    let mut disagreements = Vec::new();
    let mut over_loc = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let point = ctx.resolve(&g.variants, &g.members, g.code);
        match point.source {
            Source::Override => report.from_override += 1,
            Source::Loc => report.from_loc += 1,
            Source::Gazetteer => report.from_gazetteer += 1,
            Source::GazetteerOverLoc => report.gazetteer_over_loc += 1,
            Source::LocMultiCity => report.from_loc_multi_city += 1,
            Source::StateCentroid => report.state_centroid += 1,
        }
        let first = g.members[0];
        let state = derived[first].state;
        let name = match &names[first] {
            None => format!("{} (state)", state.name),
            Some(_) => {
                // The name: an override's, else an alias's, else the
                // gazetteer's spelling of LoC's, else LoC's (from the first
                // title of the variant with most titles), tidied.
                let own: Vec<&CityName> = g
                    .members
                    .iter()
                    .filter_map(|&m| names[m].as_ref())
                    .filter(|n| n.key == g.key)
                    .collect();
                let loc = &own.first().expect("the group's own key has a title").clean;
                point
                    .name
                    .clone()
                    .or_else(|| own.iter().find_map(|n| n.alias.clone()))
                    .or_else(|| {
                        geo.gazetteer
                            .get(g.code, &g.key)
                            .filter(|p| places::respelling(loc, &p.name))
                            .map(|p| p.name.clone())
                    })
                    .unwrap_or_else(|| places::unhyphenate(loc))
            }
        };
        if let Some((loc, gaz)) = point.check {
            let km = places::distance_km(loc, gaz);
            if point.source == Source::GazetteerOverLoc {
                over_loc.push((km, name.clone(), g.code, loc, gaz));
            } else if km > DISAGREE_KM {
                disagreements.push((km, name.clone(), g.code, loc, gaz));
            }
        }
        // A folded place shows the same town until a full release moves
        // its published titles (an incremental release keeps them there).
        for &pi in &folded[gi] {
            if let Some(p) = slots[pi].as_mut() {
                p.name = name.clone();
                p.lat = point.lat;
                p.lon = point.lon;
                p.precision = point.precision.clone();
            }
        }
        let place = match chosen[gi].and_then(|pi| slots[pi].take()) {
            Some(mut p) => {
                p.name = name;
                p.lat = point.lat;
                p.lon = point.lon;
                p.precision = point.precision;
                p
            }
            None => {
                report.new_places += 1;
                let (ordinal, id) = loop {
                    next_place += 1;
                    let id = format!("P{next_place:05}");
                    if used_ids.insert(id.clone()) {
                        break (next_place, id);
                    }
                };
                Place {
                    id,
                    ordinal,
                    name,
                    state: g.code.to_owned(),
                    lat: point.lat,
                    lon: point.lon,
                    precision: point.precision,
                }
            }
        };
        for &m in &g.members {
            place_of.insert(m, place.id.clone());
        }
        kept.push(place);
    }
    // Places no group took (hand-written ones, and places folded into
    // another) are kept as they are: ids are never reused.
    kept.extend(slots.into_iter().flatten());
    let listed = |mut v: Vec<Gap>| {
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        v.iter()
            .take(MAX_LISTED)
            .map(|(km, name, code, loc, gaz)| {
                format!(
                    "{name}, {code}: LoC {:.4},{:.4}, gazetteer {:.4},{:.4} ({km:.0} km)",
                    loc.0, loc.1, gaz.0, gaz.1
                )
            })
            .collect::<Vec<_>>()
    };
    report.disagreements = disagreements.len();
    report.disagreement_examples = listed(disagreements);
    report.gazetteer_over_loc_places = listed(over_loc);

    // Titles.
    let mut by_lccn: BTreeMap<String, Title> =
        titles.into_iter().map(|t| (t.lccn.clone(), t)).collect();
    let mut next_title = by_lccn.values().map(|t| t.ordinal).max().unwrap_or(0);
    for (i, d) in derived.iter().enumerate() {
        let place_id = place_of[&i].clone();
        let mut extra = BTreeMap::new();
        extra.insert("loc_title".into(), Value::from(d.raw.title.clone()));
        if let Some(r) = &d.raw.record {
            extra.insert("loc_record".into(), Value::from(r.clone()));
        }
        if let Some(p) = &d.place_label {
            extra.insert("place_of_publication".into(), Value::from(p.clone()));
        }
        if let Some(dates) = &d.raw.dates {
            extra.insert("dates".into(), Value::from(dates.clone()));
            let (first, last) = years(dates);
            if let Some(y) = first {
                extra.insert("first_year".into(), Value::from(y));
            }
            if let Some(y) = last {
                extra.insert("last_year".into(), Value::from(y));
            }
        }
        // Two of LoC's names can be one language (navaho, navajo): one code each.
        let mut languages: Vec<String> = Vec::new();
        for code in d.raw.languages.iter().map(|l| language_code(l)) {
            if !languages.contains(&code) {
                languages.push(code);
            }
        }
        match by_lccn.get_mut(&d.raw.lccn) {
            Some(t) => {
                t.name = d.name.clone();
                t.place_id = place_id;
                t.state = d.state.code.into();
                t.languages = languages;
                // Replace what geocode derives (a refreshed record may lack
                // some of it); keep any other, hand-written fields.
                t.extra.retain(|k, _| !DERIVED_KEYS.contains(&k.as_str()));
                t.extra.extend(extra);
            }
            None => {
                report.new_titles += 1;
                next_title += 1;
                by_lccn.insert(
                    d.raw.lccn.clone(),
                    Title {
                        lccn: d.raw.lccn.clone(),
                        name: d.name.clone(),
                        ordinal: next_title,
                        place_id,
                        state: d.state.code.into(),
                        languages,
                        extra,
                    },
                );
            }
        }
    }
    let referenced: HashSet<&str> = by_lccn.values().map(|t| t.place_id.as_str()).collect();
    report.retired = referenced_before
        .into_iter()
        .filter(|id| !referenced.contains(id.as_str()))
        .collect();
    let catalog = Catalog::new(by_lccn.into_values().collect(), kept)?;
    report.titles = catalog.titles.len();
    report.places = catalog.places.len();
    Ok((catalog, report))
}

/// The (city, state) key of a stored place, or `None` for one in no known state.
fn place_key(p: &Place, geo: &Geo) -> Option<(String, String)> {
    let state = STATES.iter().find(|s| s.code == p.state)?;
    Some(if p.name == format!("{} (state)", state.name) {
        (String::new(), state.code.to_owned())
    } else {
        (geo.city(&p.name, state).key, state.code.to_owned())
    })
}

/// The `extra` fields geocode owns on a title.
const DERIVED_KEYS: &[&str] = &[
    "loc_record",
    "loc_title",
    "place_of_publication",
    "dates",
    "first_year",
    "last_year",
];

fn derive<'a>(r: &'a RawTitle, geo: &Geo) -> Option<Derived<'a>> {
    let (name, place_label) = split_title(&r.title);
    // A title placed by hand (catalog/overrides/title-places.json).
    if let Some((city, state)) = geo.title_place(&r.lccn) {
        return Some(Derived {
            raw: r,
            name,
            place_label,
            city: Some(city.to_owned()),
            state,
        });
    }
    // The city the title names, else LoC's first listed city. A label that
    // names only a state ("(Nebraska)") has no city.
    let city = place_label
        .as_deref()
        .and_then(city_from_label)
        .filter(|c| !names_a_state(c))
        .or_else(|| r.cities.first().map(|c| title_case(c)));
    // LoC lists a record's cities and states by position: when the lists
    // line up, the city's state is the one at its position (the first, if
    // it is listed twice).
    let aligned = (r.cities.len() == r.states.len() && r.states.len() > 1)
        .then_some(city.as_deref())
        .flatten()
        .and_then(|c| {
            r.cities.iter().zip(&r.states).find_map(|(lc, ls)| {
                let s = state_by_name(ls)?;
                (geo.city(&title_case(lc), s).key == geo.city(c, s).key).then_some(s)
            })
        });
    let mut state = aligned.or_else(|| choose_state(&r.states, place_label.as_deref()))?;
    // Never a city in a state the gazetteer lacks it in, when another
    // listed state has it ("Salt Lake City" with Illinois and Utah listed).
    if let Some(c) = city.as_deref() {
        if !geo.has(c, state) {
            if let Some(other) = r
                .states
                .iter()
                .filter_map(|s| state_by_name(s))
                .find(|s| geo.has(c, s))
            {
                state = other;
            }
        }
    }
    Some(Derived {
        raw: r,
        name,
        place_label,
        city,
        state,
    })
}

/// "Name (City, St.) 1890-1894" → ("Name", Some("City, St.")).
fn split_title(title: &str) -> (String, Option<String>) {
    let mut t = title.trim();
    // A trailing date range: digits, `?` and `-`, or "…-Current".
    if let Some((rest, last)) = t.rsplit_once(' ') {
        let is_dates = last.contains('-')
            && last
                .to_ascii_lowercase()
                .trim_end_matches("current")
                .chars()
                .all(|c| c.is_ascii_digit() || c == '?' || c == '-');
        if is_dates {
            t = rest.trim_end();
        }
    }
    if let Some(body) = t.strip_suffix(')') {
        let mut depth = 1;
        for (i, c) in body.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        let name = body[..i].trim();
                        if !name.is_empty() {
                            return (name.to_owned(), Some(body[i + 1..].trim().to_owned()));
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    (t.to_owned(), None)
}

/// "Canton, Dakota Territory, [S.D.]" → "Canton"; "Cincinnati [Ohio]" → "Cincinnati".
fn city_from_label(label: &str) -> Option<String> {
    // The first place named: "Minneapolis ; St. Paul" → "Minneapolis".
    let first = label.split([',', ';']).next()?;
    let mut out = String::new();
    let mut depth = 0;
    for c in first.chars() {
        match c {
            // Editorial notes: "[Ohio]", "(Oahu)".
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let city = out.split_whitespace().collect::<Vec<_>>().join(" ");
    // "the Dalles, Or." → "The Dalles".
    let mut chars = city.chars();
    let city: String = chars
        .next()
        .map(|f| f.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    (!city.is_empty()).then_some(city)
}

fn names_a_state(s: &str) -> bool {
    STATES.iter().any(|st| {
        st.name.eq_ignore_ascii_case(s) || st.abbrevs.iter().any(|a| a.eq_ignore_ascii_case(s))
    })
}

fn choose_state(states: &[String], label: Option<&str>) -> Option<&'static State> {
    let candidates: Vec<&'static State> = states.iter().filter_map(|s| state_by_name(s)).collect();
    match candidates.as_slice() {
        [] => None,
        [one] => Some(one),
        many => {
            // Several states: the one the title's own place label names.
            let label = label.unwrap_or("");
            let score = |s: &State| {
                std::iter::once(s.name)
                    .chain(s.abbrevs.iter().copied())
                    .filter(|a| contains_word(label, a))
                    .map(str::len)
                    .max()
                    .unwrap_or(0)
            };
            let best = many.iter().map(|s| score(s)).max().unwrap_or(0);
            many.iter().find(|s| score(s) == best).copied()
        }
    }
}

/// `needle` in `hay`, not as part of a longer word ("Va." in "W. Va." counts,
/// "Va." in "Nev." doesn't).
fn contains_word(hay: &str, needle: &str) -> bool {
    let hay_l = hay.to_lowercase();
    let needle = needle.to_lowercase();
    hay_l.match_indices(&needle).any(|(i, _)| {
        let before = hay_l[..i].chars().next_back();
        let after = hay_l[i + needle.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
    })
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| {
                f.to_uppercase().chain(c).collect::<String>()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "1890-1894" → (1890, 1894); unknown digits ("18??") give `None`.
fn years(dates: &str) -> (Option<u16>, Option<u16>) {
    let mut parts = dates.split('-');
    let year = |p: Option<&str>| {
        p.and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|y| *y >= 1600)
    };
    (year(parts.next()), year(parts.next()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::places::{Gazetteer, PlaceAlias};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn geo() -> &'static Geo {
        Geo::compiled().unwrap()
    }

    fn raw(
        lccn: &str,
        title: &str,
        cities: &[&str],
        states: &[&str],
        ll: Option<[f64; 2]>,
    ) -> RawTitle {
        RawTitle {
            lccn: lccn.into(),
            title: title.into(),
            cities: cities.iter().map(|s| s.to_string()).collect(),
            states: states.iter().map(|s| s.to_string()).collect(),
            latlong: ll,
            dates: Some("1890-18??".into()),
            languages: vec!["english".into(), "german".into()],
            record: None,
        }
    }

    #[test]
    fn two_names_for_one_language_are_one_code() {
        let mut t = raw(
            "sn92024097",
            "Adahooniłigii (Phoenix, Ariz.) 1943-????",
            &["phoenix"],
            &["arizona"],
            Some([33.45, -112.07]),
        );
        t.languages = vec!["navaho".into(), "navajo".into(), "english".into()];
        let r = BTreeMap::from([(t.lccn.clone(), t)]);
        let (c, _) = build(&r, vec![], vec![], &[], geo()).unwrap();
        assert_eq!(c.title("sn92024097").unwrap().languages, ["nav", "eng"]);
    }

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

    #[test]
    fn splits_titles_and_places() {
        assert_eq!(
            split_title("The North Platte Tribune (North Platte, Neb.) 1890-1894"),
            (
                "The North Platte Tribune".into(),
                Some("North Platte, Neb.".into())
            )
        );
        assert_eq!(
            split_title("Dirva = Field (Cleveland, Ohio) 1915-Current").0,
            "Dirva = Field"
        );
        assert_eq!(
            split_title("Sauk Rapids Frontierman (Sauk Rapids, M.T. [i.e. Minn.]) 1855-1860").1,
            Some("Sauk Rapids, M.T. [i.e. Minn.]".into())
        );
        assert_eq!(split_title("No Place 1901-1902"), ("No Place".into(), None));
        assert_eq!(
            city_from_label("Cincinnati [Ohio]").as_deref(),
            Some("Cincinnati")
        );
        assert_eq!(
            city_from_label("the Dalles, Or.").as_deref(),
            Some("The Dalles")
        );
        assert_eq!(city_from_label("[Topaz, Utah]"), None);
        assert_eq!(
            city_from_label("Minneapolis ; St. Paul").as_deref(),
            Some("Minneapolis")
        );
        assert_eq!(
            city_from_label("Honolulu (Oahu), Hawaii").as_deref(),
            Some("Honolulu")
        );
        assert_eq!(
            city_from_label("Canton, Dakota Territory, [S.D.]").as_deref(),
            Some("Canton")
        );
        assert_eq!(years("1???-1914"), (None, Some(1914)));
        assert_eq!(years("1915-current"), (Some(1915), None));
    }

    #[test]
    fn picks_the_state_the_title_names() {
        let s = |states: &[&str], label: &str| {
            choose_state(
                &states.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                Some(label),
            )
            .map(|s| s.code)
        };
        assert_eq!(s(&["alabama", "mississippi"], "Petal, Miss."), Some("MS"));
        assert_eq!(
            s(&["virginia", "west virginia"], "Wheeling, W. Va."),
            Some("WV")
        );
        assert_eq!(
            s(&["virginia", "west virginia"], "Richmond, Va."),
            Some("VA")
        );
        assert_eq!(s(&["nebraska"], "anything"), Some("NE"));
        assert_eq!(s(&["atlantis"], "x"), None);
    }

    #[test]
    fn builds_a_stable_catalog() {
        let mut r = BTreeMap::new();
        for t in [
            raw(
                "sn2",
                "Miami Citizen (Miami, Fla.) 1937-1949",
                &["miami"],
                &["florida"],
                Some([25.77, -80.19]),
            ),
            raw(
                "sn1",
                "Miami Labor Citizen (Miami, Fla.) 1949-1956",
                &["miami"],
                &["florida"],
                Some([25.78, -80.2]),
            ),
            raw(
                "sn3",
                "Somewhere Gazette (Nowhere, Iowa) 1900-1901",
                &["nowhere"],
                &["iowa"],
                None,
            ),
            raw("sn4", "State Paper 1900-1901", &[], &["ohio"], None),
            raw(
                "sn5",
                "Lost Paper (Atlantis) 1900-1901",
                &["x"],
                &["atlantis"],
                None,
            ),
        ] {
            r.insert(t.lccn.clone(), t);
        }
        let (c, rep) = build(&r, vec![], vec![], &[], geo()).unwrap();
        assert_eq!(rep.unresolved, ["sn5"]);
        assert_eq!((rep.new_titles, rep.new_places), (4, 3));
        // Both Miami papers share one place, at the median of their points.
        let miami = c.place(&c.title("sn1").unwrap().place_id).unwrap();
        assert_eq!(c.title("sn2").unwrap().place_id, miami.id);
        assert_eq!((miami.name.as_str(), miami.state.as_str()), ("Miami", "FL"));
        assert!((miami.lat - 25.775).abs() < 1e-9 && (miami.lon + 80.195).abs() < 1e-9);
        assert_eq!(miami.precision, "city");
        let nowhere = c.place(&c.title("sn3").unwrap().place_id).unwrap();
        assert_eq!(
            (nowhere.name.as_str(), nowhere.precision.as_str()),
            ("Nowhere", "state")
        );
        let ohio = c.place(&c.title("sn4").unwrap().place_id).unwrap();
        assert_eq!(ohio.name, "Ohio (state)");
        let t1 = c.title("sn1").unwrap();
        assert_eq!(t1.name, "Miami Labor Citizen");
        assert_eq!(t1.languages, ["eng", "ger"]);
        assert_eq!(t1.extra["first_year"], 1890);

        // A rebuild with a new title and an override keeps every existing
        // ordinal and id, and moves Nowhere to its override.
        r.insert(
            "sn0".into(),
            raw(
                "sn0",
                "Early Bird (Akron, Ohio) 1850-1851",
                &["akron"],
                &["ohio"],
                Some([41.08, -81.51]),
            ),
        );
        let o = PlaceOverride {
            city: "Nowhere".into(),
            state: "IA".into(),
            lat: 42.0,
            lon: -93.0,
            name: None,
            precision: "city".into(),
        };
        let (c2, rep2) = build(&r, c.titles.clone(), c.places.clone(), &[o], geo()).unwrap();
        assert_eq!(
            (rep2.new_titles, rep2.new_places, rep2.from_override),
            (1, 1, 1)
        );
        for t in &c.titles {
            let t2 = c2.title(&t.lccn).unwrap();
            assert_eq!((t2.ordinal, &t2.place_id), (t.ordinal, &t.place_id));
        }
        assert_eq!(c2.title("sn0").unwrap().ordinal, 5);
        let nowhere2 = c2.place(&nowhere.id).unwrap();
        assert_eq!((nowhere2.lat, nowhere2.precision.as_str()), (42.0, "city"));
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

        let records = record_lccns(r#"{"sn87093109": {"record": "sn84022797"}}"#).unwrap();
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
        assert!(record_lccns(r#"{"sn1": {"record": "sn1"}}"#).is_err());
        assert!(record_lccns(r#"{"sn1": {"record": "BAD/x"}}"#).is_err());
        assert!(record_lccns(r#"{"sn1": {"record": "sn2", "place": "x"}}"#).is_err());
        assert_eq!(
            record_lccns(r#"{"sn1": {"record": "sn2", "note": "why"}}"#).unwrap()["sn1"],
            "sn2"
        );
    }

    #[test]
    fn the_title_overrides_in_git_parse_with_sorted_keys() {
        let records = title_records().unwrap();
        assert!(!records.is_empty());
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

    #[test]
    fn the_city_comes_from_the_title_label() {
        // No location_city: the label still names the city.
        let r = raw(
            "sn1",
            "Weekly Star (Canton, Ohio) 1880-1881",
            &[],
            &["ohio"],
            None,
        );
        assert_eq!(derive(&r, geo()).unwrap().city.as_deref(), Some("Canton"));
        // A label naming only the state has no city: the state's place.
        let r = raw(
            "sn2",
            "State Journal (Nebraska) 1880-1881",
            &[],
            &["nebraska"],
            None,
        );
        assert_eq!(derive(&r, geo()).unwrap().city, None);
        // No label: LoC's first city.
        let r = raw("sn3", "Plain Name", &["north platte"], &["nebraska"], None);
        assert_eq!(
            derive(&r, geo()).unwrap().city.as_deref(),
            Some("North Platte")
        );
    }

    #[test]
    fn a_refresh_replaces_derived_fields_and_keeps_others() {
        let mut r = BTreeMap::new();
        let first = raw(
            "sn1",
            "Paper (Akron, Ohio) 1890-1900",
            &["akron"],
            &["ohio"],
            None,
        );
        r.insert("sn1".to_owned(), first);
        let (c, _) = build(&r, vec![], vec![], &[], geo()).unwrap();
        let mut titles = c.titles.clone();
        titles[0]
            .extra
            .insert("note".into(), Value::from("hand-written"));
        assert!(titles[0].extra.contains_key("place_of_publication"));
        // The refreshed record has no label and no dates.
        let mut refreshed = raw("sn1", "Paper", &["akron"], &["ohio"], None);
        refreshed.dates = None;
        r.insert("sn1".to_owned(), refreshed);
        let (c2, _) = build(&r, titles, c.places.clone(), &[], geo()).unwrap();
        let extra = &c2.title("sn1").unwrap().extra;
        for gone in ["place_of_publication", "dates", "first_year", "last_year"] {
            assert!(!extra.contains_key(gone), "{gone} should be gone");
        }
        assert_eq!(extra["loc_title"], "Paper");
        assert_eq!(extra["note"], "hand-written");
    }

    #[test]
    fn the_overrides_in_git_parse() {
        let o: Vec<PlaceOverride> = serde_json::from_str(PLACE_OVERRIDES).unwrap();
        for p in &o {
            assert!(
                STATES.iter().any(|s| s.code == p.state),
                "{}: unknown state {}",
                p.city,
                p.state
            );
            assert!((-90.0..=90.0).contains(&p.lat) && (-180.0..=180.0).contains(&p.lon));
        }
    }

    #[test]
    fn keeps_hand_written_titles_and_places() {
        let place = Place {
            id: "P00001".into(),
            ordinal: 1,
            name: "Fixture City A (Chicago area)".into(),
            state: "IL".into(),
            lat: 41.88,
            lon: -87.63,
            precision: "city".into(),
        };
        let title = Title {
            lccn: "sn99000001".into(),
            name: "The Fixture Gazette 1".into(),
            ordinal: 1,
            place_id: "P00001".into(),
            state: "IL".into(),
            languages: vec!["eng".into()],
            extra: BTreeMap::new(),
        };
        let (c, _) = build(
            &BTreeMap::new(),
            vec![title.clone()],
            vec![place.clone()],
            &[],
            geo(),
        )
        .unwrap();
        assert_eq!(c.titles, vec![title]);
        assert_eq!(c.places, vec![place]);
    }

    const MEMPHIS: [f64; 2] = [35.1495, -90.0490];
    const SELMA: [f64; 2] = [32.4074, -87.0211];

    fn catalog_of(titles: Vec<RawTitle>, geo: &Geo) -> (Catalog, GeocodeReport) {
        let r: BTreeMap<String, RawTitle> =
            titles.into_iter().map(|t| (t.lccn.clone(), t)).collect();
        build(&r, vec![], vec![], &[], geo).unwrap()
    }

    fn place_of<'a>(c: &'a Catalog, lccn: &str) -> &'a Place {
        c.place(&c.title(lccn).unwrap().place_id).unwrap()
    }

    fn near(p: &Place, ll: [f64; 2]) -> bool {
        places::distance_km((p.lat, p.lon), (ll[0], ll[1])) < 1.0
    }

    /// A paper that moved lists every city but has one latlong (Selma's).
    fn memphis_daily_appeal() -> RawTitle {
        raw(
            "sn1",
            "Memphis Daily Appeal (Memphis, Tenn.) 1847-1886",
            &[
                "memphis",
                "grenada",
                "jackson",
                "atlanta",
                "montgomery",
                "columbus",
                "selma",
            ],
            &["tennessee", "mississippi", "georgia", "alabama"],
            Some(SELMA),
        )
    }

    #[test]
    fn a_multi_city_record_listed_first_no_longer_moves_its_place() {
        let gaz = Gazetteer::parse("state,name,lat,lon\nTN,Memphis,35.1,-89.97\n").unwrap();
        let geo = Geo::new(gaz, &[]).unwrap();
        // With a single-city Memphis record: its point, whatever comes first.
        let (c, rep) = catalog_of(
            vec![
                memphis_daily_appeal(),
                raw(
                    "sn2",
                    "Public Ledger (Memphis, Tenn.) 1865-1893",
                    &["memphis"],
                    &["tennessee"],
                    Some(MEMPHIS),
                ),
            ],
            &geo,
        );
        let p = place_of(&c, "sn1");
        assert_eq!((p.name.as_str(), p.state.as_str()), ("Memphis", "TN"));
        assert!(near(p, MEMPHIS), "{p:?}");
        assert_eq!(c.title("sn2").unwrap().place_id, p.id);
        assert_eq!((rep.from_loc, rep.from_loc_multi_city), (1, 0));

        // Only the multi-city record: the gazetteer, not Selma.
        let (c, rep) = catalog_of(vec![memphis_daily_appeal()], &geo);
        let p = place_of(&c, "sn1");
        assert_eq!((p.lat, p.lon, p.precision.as_str()), (35.1, -89.97, "city"));
        assert_eq!(rep.from_gazetteer, 1);

        // Not in the gazetteer either: LoC's point after all, as a last resort.
        let (c, rep) = catalog_of(vec![memphis_daily_appeal()], &Geo::default());
        assert!(near(place_of(&c, "sn1"), SELMA));
        assert_eq!(rep.from_loc_multi_city, 1);
    }

    #[test]
    fn far_from_the_gazetteer() {
        let memphis = |ll| {
            vec![raw(
                "sn1",
                "Paper (Memphis, Tenn.) 1900-1901",
                &["memphis"],
                &["tennessee"],
                Some(ll),
            )]
        };
        // The state's only Memphis, 400 km from LoC's point: the gazetteer's.
        let gaz = Gazetteer::parse("state,name,lat,lon\nTN,Memphis,35.1,-89.97\n").unwrap();
        let geo = Geo::new(gaz, &[]).unwrap();
        let (c, rep) = catalog_of(memphis(SELMA), &geo);
        let p = place_of(&c, "sn1");
        assert_eq!((p.lat, p.lon, p.precision.as_str()), (35.1, -89.97, "city"));
        assert_eq!((rep.gazetteer_over_loc, rep.disagreements), (1, 0));
        assert!(
            rep.gazetteer_over_loc_places[0].starts_with("Memphis, TN: LoC 32.4074,-87.0211"),
            "{:?}",
            rep.gazetteer_over_loc_places
        );

        // 50 km off: LoC's point is kept, and listed for review.
        let (c, rep) = catalog_of(memphis([35.1, -89.42]), &geo);
        assert!(near(place_of(&c, "sn1"), [35.1, -89.42]));
        assert_eq!((rep.gazetteer_over_loc, rep.disagreements), (0, 1));
        assert!(rep.disagreement_examples[0].starts_with("Memphis, TN: "));

        // Two towns of the name in the state: LoC's point, for review.
        let gaz =
            Gazetteer::parse("state,name,lat,lon\nTN,Memphis,35.1,-89.97\nTN,Memphis,36.0,-84.0\n")
                .unwrap();
        let geo = Geo::new(gaz, &[]).unwrap();
        let (c, rep) = catalog_of(memphis(SELMA), &geo);
        assert!(near(place_of(&c, "sn1"), SELMA));
        assert_eq!((rep.gazetteer_over_loc, rep.disagreements), (0, 1));
    }

    #[test]
    fn multi_city_records_pair_cities_and_states_by_position() {
        let gaz = Gazetteer::parse(
            "state,name,lat,lon\nIL,Chicago,41.88,-87.63\nUT,Salt Lake City,40.76,-111.89\n",
        )
        .unwrap();
        let geo = Geo::new(gaz, &[]).unwrap();
        let state = |r: &RawTitle| derive(r, &geo).unwrap().state.code;
        // The label names Salt Lake City and both states: the position pairs it with Utah.
        let r = raw(
            "sn1",
            "The Broad Ax (Salt Lake City, Utah ; Chicago, Ill.) 1895-19??",
            &["chicago", "salt lake city"],
            &["illinois", "utah"],
            None,
        );
        assert_eq!(state(&r), "UT");
        // No label: the record's first city, in its own state.
        let r = raw(
            "sn2",
            "The Broad Ax",
            &["salt lake city", "chicago"],
            &["utah", "illinois"],
            None,
        );
        let d = derive(&r, &geo).unwrap();
        assert_eq!(
            (d.city.as_deref(), d.state.code),
            (Some("Salt Lake City"), "UT")
        );
        // Lists that don't line up: never a state the gazetteer lacks the
        // city in, when another listed state has it.
        let r = raw(
            "sn3",
            "Intermountain Catholic (Salt Lake City, Colo.) 1899-1926",
            &["salt lake city"],
            &["colorado", "utah"],
            None,
        );
        assert_eq!(state(&r), "UT");
        // A city no gazetteer lists keeps the state its label names.
        let r = raw(
            "sn4",
            "Paper (Nowhere, Colo.) 1899-1926",
            &["nowhere"],
            &["colorado", "utah"],
            None,
        );
        assert_eq!(state(&r), "CO");
    }

    #[test]
    fn a_title_place_overrides_the_record() {
        let geo = Geo::default()
            .with_title_places(&[places::TitlePlace {
                city: "Chicago".into(),
                lccn: "sn84024055".into(),
                note: None,
                state: "IL".into(),
            }])
            .unwrap();
        let r = raw(
            "sn84024055",
            "The Broad Ax (Salt Lake City, Utah) 1895-19??",
            &["chicago", "salt lake city"],
            &["illinois", "utah"],
            Some([40.76, -111.89]),
        );
        let d = derive(&r, &geo).unwrap();
        assert_eq!((d.city.as_deref(), d.state.code), (Some("Chicago"), "IL"));
        assert_eq!(d.place_label.as_deref(), Some("Salt Lake City, Utah"));
        // Its record lists two cities, so its latlong doesn't place Chicago.
        let (c, rep) = catalog_of(vec![r], &geo);
        assert_eq!(place_of(&c, "sn84024055").name, "Chicago");
        assert_eq!(rep.from_loc_multi_city, 1);
    }

    #[test]
    fn spelling_variants_and_aliases_share_a_place() {
        let alias = PlaceAlias {
            from: "Skaguay".into(),
            note: Some("old spelling".into()),
            state: "AK".into(),
            to: "Skagway".into(),
        };
        let geo = Geo::new(Gazetteer::default(), &[alias]).unwrap();
        let (c, rep) = catalog_of(
            vec![
                raw(
                    "sn1",
                    "A (St. Paul, Minn.) 1900-1901",
                    &["st. paul"],
                    &["minnesota"],
                    Some([44.95, -93.09]),
                ),
                raw(
                    "sn2",
                    "B (Saint Paul, Minn.) 1900-1901",
                    &["saint paul"],
                    &["minnesota"],
                    Some([44.94, -93.1]),
                ),
                raw(
                    "sn3",
                    "C (Chicago Ill.) 1900-1901",
                    &["chicago"],
                    &["illinois"],
                    Some([41.88, -87.63]),
                ),
                raw(
                    "sn4",
                    "D (Chicago, Ill.) 1900-1901",
                    &["chicago"],
                    &["illinois"],
                    Some([41.88, -87.63]),
                ),
                raw(
                    "sn5",
                    "E (Skaguay, Alaska) 1900-1901",
                    &["skaguay"],
                    &["alaska"],
                    Some([59.45, -135.31]),
                ),
                raw(
                    "sn6",
                    "F (Skagway, Alaska) 1900-1901",
                    &["skagway"],
                    &["alaska"],
                    Some([59.46, -135.31]),
                ),
                raw(
                    "sn7",
                    "G (Anderson Court House, S.C.) 1860-1861",
                    &["anderson court house"],
                    &["south carolina"],
                    Some([34.5, -82.65]),
                ),
                raw(
                    "sn8",
                    "H (Anderson, S.C.) 1900-1901",
                    &["anderson"],
                    &["south carolina"],
                    Some([34.5, -82.65]),
                ),
                raw(
                    "sn9",
                    "I (East Providence, R.I.) 1900-1901",
                    &["east providence"],
                    &["rhode island"],
                    Some([41.81, -71.37]),
                ),
                raw(
                    "sn10",
                    "J (Providence, R.I.) 1900-1901",
                    &["providence"],
                    &["rhode island"],
                    Some([41.82, -71.41]),
                ),
            ],
            &geo,
        );
        for (a, b) in [
            ("sn1", "sn2"),
            ("sn3", "sn4"),
            ("sn5", "sn6"),
            ("sn7", "sn8"),
        ] {
            assert_eq!(place_of(&c, a).id, place_of(&c, b).id, "{a} {b}");
        }
        assert_ne!(place_of(&c, "sn9").id, place_of(&c, "sn10").id);
        assert_eq!(place_of(&c, "sn5").name, "Skagway");
        assert_eq!(place_of(&c, "sn3").name, "Chicago");
        assert_eq!(rep.aliased, 1);
        assert_eq!(c.places.len(), 6);
    }

    #[test]
    fn x_city_joins_x_only_when_close() {
        let yazoo = [32.855, -90.405];
        let (c, _) = catalog_of(
            vec![
                raw(
                    "sn1",
                    "A (Yazoo City, Miss.) 1900-1901",
                    &["yazoo city"],
                    &["mississippi"],
                    Some(yazoo),
                ),
                raw(
                    "sn2",
                    "B (Yazoo City, Miss.) 1900-1901",
                    &["yazoo city"],
                    &["mississippi"],
                    Some(yazoo),
                ),
                // 3 km away: the same town.
                raw(
                    "sn3",
                    "C (Yazoo, Miss.) 1900-1901",
                    &["yazoo"],
                    &["mississippi"],
                    Some([32.88, -90.41]),
                ),
                // 50 km apart: two towns.
                raw(
                    "sn4",
                    "D (Foo City, Iowa) 1900-1901",
                    &["foo city"],
                    &["iowa"],
                    Some([42.0, -93.0]),
                ),
                raw(
                    "sn5",
                    "E (Foo, Iowa) 1900-1901",
                    &["foo"],
                    &["iowa"],
                    Some([42.45, -93.0]),
                ),
                // Neither placed (state centroids): not joined.
                raw(
                    "sn6",
                    "F (Bar City, Ohio) 1900-1901",
                    &["bar city"],
                    &["ohio"],
                    None,
                ),
                raw("sn7", "G (Bar, Ohio) 1900-1901", &["bar"], &["ohio"], None),
            ],
            &Geo::default(),
        );
        let y = place_of(&c, "sn3");
        assert_eq!(y.id, place_of(&c, "sn1").id);
        // The name with more titles.
        assert_eq!(y.name, "Yazoo City");
        assert!(near(y, yazoo), "the median of the three points");
        assert_ne!(place_of(&c, "sn4").id, place_of(&c, "sn5").id);
        assert_ne!(place_of(&c, "sn6").id, place_of(&c, "sn7").id);
    }

    #[test]
    fn merged_places_keep_the_id_with_most_titles() {
        let titles = |cities: &[(&str, &str)]| -> BTreeMap<String, RawTitle> {
            cities
                .iter()
                .map(|(lccn, city)| {
                    let t = raw(
                        lccn,
                        &format!("Paper ({city}, Minn.) 1900-1901"),
                        &[&city.to_lowercase()],
                        &["minnesota"],
                        Some([44.95, -93.09]),
                    );
                    (t.lccn.clone(), t)
                })
                .collect()
        };
        // Before: "St. Paul" and "Saint Paul" were two places (as on the live
        // site, which keyed only by letters), and another place came later.
        let place = |id: &str, ordinal, name: &str| Place {
            id: id.into(),
            ordinal,
            name: name.into(),
            state: "MN".into(),
            lat: 44.95,
            lon: -93.09,
            precision: "city".into(),
        };
        let title = |lccn: &str, ordinal, place_id: &str| Title {
            lccn: lccn.into(),
            name: "Paper".into(),
            ordinal,
            place_id: place_id.into(),
            state: "MN".into(),
            languages: vec![],
            extra: BTreeMap::new(),
        };
        let stored_places = vec![
            place("P00001", 1, "Saint Paul"),
            place("P00002", 2, "St. Paul"),
            place("P00003", 3, "Duluth"),
        ];
        let stored_titles = vec![
            title("sn1", 1, "P00001"),
            title("sn2", 2, "P00002"),
            title("sn3", 3, "P00002"),
            title("sn4", 4, "P00003"),
        ];
        let r = titles(&[
            ("sn1", "Saint Paul"),
            ("sn2", "St. Paul"),
            ("sn3", "St. Paul"),
            ("sn4", "Duluth"),
            ("sn5", "Winona"),
        ]);
        let (c, rep) = build(
            &r,
            stored_titles.clone(),
            stored_places.clone(),
            &[],
            &Geo::default(),
        )
        .unwrap();
        // The variant with more titles keeps its id; the other is retired
        // but stays in the catalog, so no new place takes its id.
        for lccn in ["sn1", "sn2", "sn3"] {
            assert_eq!(c.title(lccn).unwrap().place_id, "P00002");
        }
        assert_eq!(rep.retired, ["P00001"]);
        // It shows the same town as the place it was folded into.
        let (kept, folded) = (c.place("P00002").unwrap(), c.place("P00001").unwrap());
        assert_eq!(
            (&folded.name, folded.lat, folded.lon),
            (&kept.name, kept.lat, kept.lon)
        );
        assert_eq!(c.title("sn5").unwrap().place_id, "P00004");
        assert_eq!(c.title("sn4").unwrap().place_id, "P00003");
        // A second build changes nothing.
        let (c2, rep2) =
            build(&r, c.titles.clone(), c.places.clone(), &[], &Geo::default()).unwrap();
        assert_eq!((c2.titles, c2.places), (c.titles.clone(), c.places.clone()));
        assert!(rep2.retired.is_empty());

        // A tie goes to the lower ordinal.
        let r = titles(&[("sn1", "Saint Paul"), ("sn2", "St. Paul")]);
        let (c, rep) = build(
            &r,
            vec![title("sn1", 1, "P00002"), title("sn2", 2, "P00001")],
            stored_places[..2].to_vec(),
            &[],
            &Geo::default(),
        )
        .unwrap();
        assert_eq!(c.title("sn2").unwrap().place_id, "P00001");
        assert_eq!(rep.retired, ["P00002"]);
    }

    #[test]
    fn display_names() {
        let gaz = Gazetteer::parse("state,name,lat,lon\nMN,St. Paul,44.95,-93.09\n").unwrap();
        let geo = Geo::new(gaz, &[]).unwrap();
        let (c, _) = catalog_of(
            vec![
                raw(
                    "sn1",
                    "A (New-York, N.Y.) 1800-1801",
                    &["new york"],
                    &["new york"],
                    Some([40.71, -74.0]),
                ),
                raw(
                    "sn2",
                    "B (Winston-Salem, N.C.) 1920-1921",
                    &["winston-salem"],
                    &["north carolina"],
                    Some([36.1, -80.24]),
                ),
                raw(
                    "sn3",
                    "C (Saint Paul, Minn.) 1900-1901",
                    &["saint paul"],
                    &["minnesota"],
                    Some([44.95, -93.09]),
                ),
                raw(
                    "sn4",
                    "D (M'arthur, Ohio) 1850-1851",
                    &["m'arthur"],
                    &["ohio"],
                    Some([39.25, -82.48]),
                ),
                raw(
                    "sn5",
                    "E (Rohwer Ark.) 1943-1945",
                    &["rohwer"],
                    &["arkansas"],
                    Some([33.77, -91.27]),
                ),
            ],
            &geo,
        );
        let name = |lccn| place_of(&c, lccn).name.clone();
        assert_eq!(name("sn1"), "New York");
        assert_eq!(name("sn2"), "Winston-Salem");
        // The gazetteer's spelling.
        assert_eq!(name("sn3"), "St. Paul");
        assert_eq!(name("sn4"), "McArthur");
        assert_eq!(name("sn5"), "Rohwer");

        // An override's name wins.
        let o = PlaceOverride {
            city: "Saint Paul".into(),
            state: "MN".into(),
            lat: 44.9,
            lon: -93.1,
            name: Some("Saint Paul".into()),
            precision: "city".into(),
        };
        let r: BTreeMap<String, RawTitle> = [raw(
            "sn3",
            "C (Saint Paul, Minn.) 1900-1901",
            &["saint paul"],
            &["minnesota"],
            None,
        )]
        .into_iter()
        .map(|t| (t.lccn.clone(), t))
        .collect();
        let (c, rep) = build(&r, vec![], vec![], &[o], &geo).unwrap();
        let p = place_of(&c, "sn3");
        assert_eq!(
            (p.name.as_str(), p.lat, rep.from_override),
            ("Saint Paul", 44.9, 1)
        );
    }
}
