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
//!   LoC title, e.g. "(North Platte, Neb.)". Coordinates are, in order: a
//!   manual override (`catalog/overrides/places.json` in git); LoC's own `latlong` for the
//!   title (precision `city`); else the state's centroid (precision `state`).
//!
//! Ordinals and place ids are stable: records already in the catalog keep
//! them, new ones get the next free number, and nothing is ever removed
//! (published snapshots keep their own copies, 04 §4.7).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use usnm_core::names::{language_code, state_by_name, State, STATES};
use usnm_store::ObjectStore;

use crate::activity::Reporter;
use crate::catalog::{Catalog, Place, Title, PLACES, TITLES};
use crate::source;

/// LoC's item endpoint; a title record is `{base}/{lccn}/?fo=json`.
pub const LOC_ITEMS: &str = "https://www.loc.gov/item";
/// The fields of every fetched title record, by LCCN.
pub const RAW: &str = "raw/titles.json";
/// Manual coordinates, `[{city, state, lat, lon, precision?}]`, kept in git
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
    #[serde(default = "city_precision")]
    pub precision: String,
}

fn city_precision() -> String {
    "city".into()
}

#[derive(Debug, Default, Serialize)]
pub struct GeocodeReport {
    pub titles: usize,
    pub new_titles: usize,
    pub places: usize,
    pub new_places: usize,
    /// Places by how their coordinates were found.
    pub from_override: usize,
    pub from_loc: usize,
    pub state_centroid: usize,
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
    let (catalog, mut report) = build(&raw, titles.clone(), places.clone(), &overrides)?;
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

/// The pure part of [`geocode`].
pub fn build(
    raw: &BTreeMap<String, RawTitle>,
    titles: Vec<Title>,
    places: Vec<Place>,
    overrides: &[PlaceOverride],
) -> anyhow::Result<(Catalog, GeocodeReport)> {
    let mut report = GeocodeReport::default();
    let derived: Vec<Derived> = raw
        .values()
        .filter_map(|r| {
            let d = derive(r);
            if d.is_none() {
                report.unresolved.push(r.lccn.clone());
            }
            d
        })
        .collect();

    // Places, keyed by (normalized city, state); "" is the state itself.
    let key_of =
        |city: Option<&str>, state: &State| (city.map(norm).unwrap_or_default(), state.code);
    let mut groups: BTreeMap<(String, &'static str), Vec<&Derived>> = BTreeMap::new();
    for d in &derived {
        groups
            .entry(key_of(d.city.as_deref(), d.state))
            .or_default()
            .push(d);
    }
    let overrides: HashMap<(String, String), &PlaceOverride> = overrides
        .iter()
        .map(|o| ((norm(&o.city), o.state.to_uppercase()), o))
        .collect();
    let mut places_by_key: HashMap<(String, String), Place> = HashMap::new();
    let mut kept = Vec::new();
    for p in places {
        match place_key(&p) {
            Some(k) if !places_by_key.contains_key(&k) => {
                places_by_key.insert(k, p);
            }
            // Places geocode can't match up are kept as they are.
            _ => kept.push(p),
        }
    }
    let mut next_place = places_by_key
        .values()
        .chain(&kept)
        .map(|p| p.ordinal)
        .max()
        .unwrap_or(0);
    let mut used_ids: HashSet<String> = places_by_key
        .values()
        .chain(&kept)
        .map(|p| p.id.clone())
        .collect();
    let mut place_of: HashMap<(String, &'static str), String> = HashMap::new();
    for ((city_key, code), members) in &groups {
        let state = members[0].state;
        let city = members[0].city.clone();
        let (lat, lon, precision) =
            if let Some(o) = overrides.get(&(city_key.clone(), code.to_string())) {
                report.from_override += 1;
                (o.lat, o.lon, o.precision.clone())
            } else if let Some([lat, lon]) = members
                .iter()
                .filter(|_| city.is_some())
                .find_map(|d| d.raw.latlong)
            {
                report.from_loc += 1;
                (lat, lon, "city".to_owned())
            } else {
                report.state_centroid += 1;
                (state.lat, state.lon, "state".to_owned())
            };
        let name = city.unwrap_or_else(|| format!("{} (state)", state.name));
        let key = (city_key.clone(), code.to_string());
        let place = match places_by_key.remove(&key) {
            Some(mut p) => {
                p.name = name;
                p.lat = lat;
                p.lon = lon;
                p.precision = precision;
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
                    state: code.to_string(),
                    lat,
                    lon,
                    precision,
                }
            }
        };
        place_of.insert((city_key.clone(), *code), place.id.clone());
        kept.push(place);
    }
    kept.extend(places_by_key.into_values());

    // Titles.
    let mut by_lccn: BTreeMap<String, Title> =
        titles.into_iter().map(|t| (t.lccn.clone(), t)).collect();
    let mut next_title = by_lccn.values().map(|t| t.ordinal).max().unwrap_or(0);
    for d in &derived {
        let place_id = place_of[&key_of(d.city.as_deref(), d.state)].clone();
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
    let catalog = Catalog::new(by_lccn.into_values().collect(), kept)?;
    report.titles = catalog.titles.len();
    report.places = catalog.places.len();
    Ok((catalog, report))
}

/// The (city, state) key of a stored place, or `None` for one in no known state.
fn place_key(p: &Place) -> Option<(String, String)> {
    let state = STATES.iter().find(|s| s.code == p.state)?;
    Some(if p.name == format!("{} (state)", state.name) {
        (String::new(), state.code.to_owned())
    } else {
        (norm(&p.name), state.code.to_owned())
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

fn derive(r: &RawTitle) -> Option<Derived<'_>> {
    let (name, place_label) = split_title(&r.title);
    let state = choose_state(&r.states, place_label.as_deref())?;
    // The city the title names, else LoC's first listed city. A label that
    // names only a state ("(Nebraska)") has no city.
    let city = place_label
        .as_deref()
        .and_then(city_from_label)
        .filter(|c| !names_a_state(c))
        .or_else(|| r.cities.first().map(|c| title_case(c)));
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

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
        let (c, _) = build(&r, vec![], vec![], &[]).unwrap();
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
        let (c, rep) = build(&r, vec![], vec![], &[]).unwrap();
        assert_eq!(rep.unresolved, ["sn5"]);
        assert_eq!((rep.new_titles, rep.new_places), (4, 3));
        // Both Miami papers share one place, at the first title's coordinates.
        let miami = c.place(&c.title("sn1").unwrap().place_id).unwrap();
        assert_eq!(c.title("sn2").unwrap().place_id, miami.id);
        assert_eq!(
            (miami.name.as_str(), miami.state.as_str(), miami.lat),
            ("Miami", "FL", 25.78)
        );
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
            precision: "city".into(),
        };
        let (c2, rep2) = build(&r, c.titles.clone(), c.places.clone(), &[o]).unwrap();
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
        assert_eq!(derive(&r).unwrap().city.as_deref(), Some("Canton"));
        // A label naming only the state has no city: the state's place.
        let r = raw(
            "sn2",
            "State Journal (Nebraska) 1880-1881",
            &[],
            &["nebraska"],
            None,
        );
        assert_eq!(derive(&r).unwrap().city, None);
        // No label: LoC's first city.
        let r = raw("sn3", "Plain Name", &["north platte"], &["nebraska"], None);
        assert_eq!(derive(&r).unwrap().city.as_deref(), Some("North Platte"));
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
        let (c, _) = build(&r, vec![], vec![], &[]).unwrap();
        let mut titles = c.titles.clone();
        titles[0]
            .extra
            .insert("note".into(), Value::from("hand-written"));
        assert!(titles[0].extra.contains_key("place_of_publication"));
        // The refreshed record has no label and no dates.
        let mut refreshed = raw("sn1", "Paper", &["akron"], &["ohio"], None);
        refreshed.dates = None;
        r.insert("sn1".to_owned(), refreshed);
        let (c2, _) = build(&r, titles, c.places.clone(), &[]).unwrap();
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
        )
        .unwrap();
        assert_eq!(c.titles, vec![title]);
        assert_eq!(c.places, vec![place]);
    }
}
