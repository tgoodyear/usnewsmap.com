//! Titles and places: the catalog that pages are joined with (04 §4.6).
//!
//! `titles-sync` and `geocode` ([`crate::titles`]) maintain
//! `catalog/titles.json` and `catalog/places.json` in the reference store. Each published reference snapshot carries its own copy,
//! so a published version never changes when the catalog does.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{bail, Context};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use usnm_store::{is_safe_segment, ObjectStore};

pub const TITLES: &str = "catalog/titles.json";
pub const PLACES: &str = "catalog/places.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: String,
    /// Stable small integer, never reused; `place_shard = ordinal % 8`.
    pub ordinal: u32,
    pub name: String,
    pub state: String,
    pub lat: f64,
    pub lon: f64,
    pub precision: String,
    /// The rule `geocode` took the coordinates from (04 §4.6): `override`,
    /// `loc`, `gazetteer`, `gazetteer_over_loc`, `loc_multi_city` or
    /// `state_centroid`. Absent on a place geocode didn't place
    /// (hand-written) and in catalogs written before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinates_from: Option<String>,
    /// The city names of the place's titles other than its own name, and
    /// the rule that put each title here (04 §4.6). Empty when every title
    /// names it as the place does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<Variant>,
}

/// A city name a place's titles use, other than the place's name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Variant {
    /// The city as the titles' LoC records name it (cleaned up: no
    /// trailing state, "City of", …).
    pub name: String,
    /// `spelling` (keys alike: case, punctuation, "St."/"Saint", …),
    /// `alias` (`catalog/overrides/place-aliases.json`), `x_city` ("X City"
    /// next to "X", within 10 km) or `title_place`
    /// (`catalog/overrides/title-places.json`).
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Title {
    pub lccn: String,
    pub name: String,
    /// Stable integer, never reused: the leading part of a hit's same-day
    /// `sort_key` (05 §5.5).
    pub ordinal: u32,
    /// The title's place: where most of its pages were published. A page
    /// dated within one of `place_periods` is in that period's place instead.
    pub place_id: String,
    pub state: String,
    #[serde(default)]
    pub languages: Vec<String>,
    /// The places of a title that moved, by the dates of its pages
    /// (`catalog/overrides/title-places.json`, #1066), oldest first: at least
    /// two, the first without `from`, the last without `to`, each starting
    /// the day after the one before ends ([`Catalog::new`] checks), so every
    /// date has a place. Empty for a title in one place, and in catalogs
    /// written before they were recorded: every page is then in `place_id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub place_periods: Vec<PlacePeriod>,
    /// Anything else the catalog records (dates, ethnicity, …), passed through.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A place a title is in for part of its run: its pages dated from `from`
/// to `to`, both days included. A period without `from` starts with the
/// title, one without `to` runs to its end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacePeriod {
    pub place_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<NaiveDate>,
}

impl PlacePeriod {
    /// Whether a page dated `date` falls in the period.
    pub fn contains(&self, date: NaiveDate) -> bool {
        self.from.is_none_or(|f| f <= date) && self.to.is_none_or(|t| date <= t)
    }
}

impl Title {
    /// The id of the place a page of this title dated `date` is in: the
    /// place of the period the date falls in, else the title's own place.
    pub fn place_id_on(&self, date: NaiveDate) -> &str {
        self.place_periods
            .iter()
            .find(|p| p.contains(date))
            .map_or(self.place_id.as_str(), |p| p.place_id.as_str())
    }

    /// Every place a page of this title can be in, each once: its periods'
    /// places, oldest first (two periods in one place list it once), then
    /// its own place if no period is there. Allocates nothing, which matters
    /// in loops over every title; the few periods of a title that moved are
    /// compared with each other instead.
    pub fn place_ids(&self) -> impl Iterator<Item = &str> + '_ {
        let periods = &self.place_periods;
        periods
            .iter()
            .enumerate()
            .filter(move |&(i, p)| !periods[..i].iter().any(|q| q.place_id == p.place_id))
            .map(|(_, p)| p.place_id.as_str())
            .chain(
                Some(self.place_id.as_str())
                    .filter(move |own| !periods.iter().any(|q| q.place_id == *own)),
            )
    }

    /// The state of a page of this title that is in `place`: the title's
    /// own, or, for a title that moved between places, the place's.
    pub fn state_in<'a>(&'a self, place: &'a Place) -> &'a str {
        if self.place_periods.is_empty() {
            &self.state
        } else {
            &place.state
        }
    }
}

/// A title's `place_periods` as `catalog/overrides/README.md` has them: none,
/// or at least two, oldest first, the first without `from`, the last without
/// `to`, and each starting the day after the one before ends. A date in no
/// period would fall back to the title's own place, putting its pages in a
/// place the source doesn't give them, so a gap or an open end where another
/// period follows is refused.
fn check_periods(lccn: &str, periods: &[PlacePeriod]) -> anyhow::Result<()> {
    if periods.is_empty() {
        return Ok(());
    }
    if periods.len() < 2 {
        bail!("title `{lccn}`: fewer than two place periods: a title in one place has none");
    }
    for (i, p) in periods.iter().enumerate() {
        let n = i + 1;
        let (first, last) = (i == 0, n == periods.len());
        match (first, p.from, last, p.to) {
            (true, Some(_), _, _) => {
                bail!("title `{lccn}`: place period {n}, the first, has a `from`")
            }
            (false, None, _, _) => bail!("title `{lccn}`: place period {n} has no `from`"),
            (_, _, true, Some(_)) => {
                bail!("title `{lccn}`: place period {n}, the last, has a `to`")
            }
            (_, _, false, None) => bail!("title `{lccn}`: place period {n} has no `to`"),
            _ => {}
        }
        if p.from.zip(p.to).is_some_and(|(from, to)| from > to) {
            bail!("title `{lccn}`: place period {n} ends before it starts");
        }
        if i > 0 && periods[i - 1].to.and_then(|to| to.succ_opt()) != p.from {
            bail!(
                "title `{lccn}`: place period {n} doesn't start the day after the one before it ends"
            );
        }
    }
    Ok(())
}

/// Title ordinals fill bits 32 and up of `sort_key` (edition and seq take 16
/// bits each). Capped so every key stays below 2^53 and exact in JSON.
const MAX_TITLE_ORDINAL: u32 = (1 << 21) - 1;

#[derive(Debug, Clone)]
pub struct Catalog {
    pub titles: Vec<Title>,
    pub places: Vec<Place>,
    title_index: HashMap<String, usize>,
    place_index: HashMap<String, usize>,
}

impl Catalog {
    pub fn new(mut titles: Vec<Title>, mut places: Vec<Place>) -> anyhow::Result<Self> {
        titles.sort_by(|a, b| a.lccn.cmp(&b.lccn));
        places.sort_by(|a, b| a.id.cmp(&b.id));
        let mut ordinals = HashSet::new();
        let mut place_index = HashMap::new();
        for (i, p) in places.iter().enumerate() {
            if !is_safe_segment(&p.id) || place_index.insert(p.id.clone(), i).is_some() {
                bail!("place id `{}` is invalid or duplicated", p.id);
            }
            if !ordinals.insert(p.ordinal) {
                bail!("place ordinal {} is used twice", p.ordinal);
            }
        }
        ordinals.clear();
        let mut title_index = HashMap::new();
        for (i, t) in titles.iter().enumerate() {
            usnm_core::ids::PageKey::new(&t.lccn, chrono::NaiveDate::MIN, 1, 1)
                .with_context(|| format!("title `{}`", t.lccn))?;
            if title_index.insert(t.lccn.clone(), i).is_some() {
                bail!("title `{}` is listed twice", t.lccn);
            }
            if t.ordinal > MAX_TITLE_ORDINAL || !ordinals.insert(t.ordinal) {
                bail!("title ordinal {} is out of range or used twice", t.ordinal);
            }
            if !place_index.contains_key(&t.place_id) {
                bail!("title `{}` names unknown place `{}`", t.lccn, t.place_id);
            }
            for p in &t.place_periods {
                if !place_index.contains_key(&p.place_id) {
                    bail!("title `{}` names unknown place `{}`", t.lccn, p.place_id);
                }
            }
            check_periods(&t.lccn, &t.place_periods)?;
        }
        Ok(Self {
            titles,
            places,
            title_index,
            place_index,
        })
    }

    pub async fn load(store: &dyn ObjectStore) -> anyhow::Result<Self> {
        let get = |path: &'static str| async move {
            store
                .get(path)
                .await?
                .with_context(|| format!("`{path}` is missing from the reference store"))
        };
        let titles: Vec<Title> = serde_json::from_slice(&get(TITLES).await?).context(TITLES)?;
        let places: Vec<Place> = serde_json::from_slice(&get(PLACES).await?).context(PLACES)?;
        Self::new(titles, places)
    }

    pub fn title(&self, lccn: &str) -> Option<&Title> {
        self.title_index.get(lccn).map(|&i| &self.titles[i])
    }

    pub fn place(&self, id: &str) -> Option<&Place> {
        self.place_index.get(id).map(|&i| &self.places[i])
    }

    /// The place a page of `title` dated `date` is in ([`Title::place_id_on`]).
    pub fn place_on(&self, title: &Title, date: NaiveDate) -> Option<&Place> {
        self.place(title.place_id_on(date))
    }

    /// For an incremental release: titles already published keep their
    /// published record, so their pages stay with the place the published
    /// indexes give them (changes, a move to another place or places by
    /// date included, wait for the next full rebuild, 04 §4.7). A new
    /// title whose place took in published titles from another place (a
    /// merge) goes where those are published, so the version doesn't gain
    /// a second id for the town. A published place keeps its id, ordinal
    /// and state but takes its name, coordinates and precision (and where
    /// they came from) from the current catalog: index documents carry
    /// only the place's id and shard, so a corrected point needs no
    /// reindex (04 §4.6). New places come from the current catalog.
    pub fn carried_forward(published: &Catalog, current: &Catalog) -> anyhow::Result<Self> {
        // For each current place, the published places of its published
        // titles, with how many titles: the title's own place, and the
        // place of each period the title has in both catalogs for the same
        // dates (pages of those dates are published there). A place only
        // some titles' early pages are in is found this way too.
        let mut published_as: HashMap<&str, BTreeMap<&str, usize>> = HashMap::new();
        for t in &published.titles {
            let Some(now) = current.title(&t.lccn) else {
                continue;
            };
            let mut pairs = BTreeSet::from([(now.place_id.as_str(), t.place_id.as_str())]);
            for was in &t.place_periods {
                if let Some(is) = now
                    .place_periods
                    .iter()
                    .find(|is| (is.from, is.to) == (was.from, was.to))
                {
                    pairs.insert((is.place_id.as_str(), was.place_id.as_str()));
                }
            }
            for (now_id, was_id) in pairs {
                *published_as
                    .entry(now_id)
                    .or_default()
                    .entry(was_id)
                    .or_default() += 1;
            }
        }
        let mut titles = published.titles.clone();
        // Where a new title's place goes: the published place with most of
        // the published titles its place took in, else its own.
        let retarget = |id: &str| -> String {
            if let Some(ids) = published_as.get(id) {
                if !ids.contains_key(id) {
                    // The published place with most of them, then the lowest ordinal.
                    let ordinal = |id: &str| published.place(id).map_or(u32::MAX, |p| p.ordinal);
                    if let Some((id, _)) = ids
                        .iter()
                        .max_by_key(|(id, n)| (**n, std::cmp::Reverse(ordinal(id))))
                    {
                        return (*id).to_owned();
                    }
                }
            }
            id.to_owned()
        };
        for t in &current.titles {
            if published.title(&t.lccn).is_some() {
                continue;
            }
            let mut t = t.clone();
            t.place_id = retarget(&t.place_id);
            for p in &mut t.place_periods {
                p.place_id = retarget(&p.place_id);
            }
            titles.push(t);
        }
        let mut places: Vec<Place> = published
            .places
            .iter()
            .map(|p| match current.place(&p.id) {
                Some(now) => Place {
                    name: now.name.clone(),
                    lat: now.lat,
                    lon: now.lon,
                    precision: now.precision.clone(),
                    coordinates_from: now.coordinates_from.clone(),
                    variants: now.variants.clone(),
                    ..p.clone()
                },
                None => p.clone(),
            })
            .collect();
        places.extend(
            current
                .places
                .iter()
                .filter(|p| published.place(&p.id).is_none())
                .cloned(),
        );
        Self::new(titles, places)
    }

    /// The published titles the current catalog puts in another place (a
    /// merge, an alias, a corrected city) or in other places by date, in
    /// LCCN order: an incremental release keeps them where they were
    /// published (04 §4.6).
    pub fn regrouped(published: &Catalog, current: &Catalog) -> Vec<String> {
        published
            .titles
            .iter()
            .filter(|t| {
                current.title(&t.lccn).is_some_and(|now| {
                    now.place_id != t.place_id || now.place_periods != t.place_periods
                })
            })
            .map(|t| t.lccn.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(id: &str, ordinal: u32) -> Place {
        Place {
            id: id.into(),
            ordinal,
            name: id.into(),
            state: "IL".into(),
            lat: 41.0,
            lon: -87.0,
            precision: "city".into(),
            coordinates_from: None,
            variants: vec![],
        }
    }

    fn title(lccn: &str, ordinal: u32, place_id: &str) -> Title {
        Title {
            lccn: lccn.into(),
            name: lccn.into(),
            ordinal,
            place_id: place_id.into(),
            state: "IL".into(),
            languages: vec!["eng".into()],
            place_periods: vec![],
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn validates_references_and_ordinals() {
        let places = vec![place("P1", 1), place("P2", 2)];
        assert!(Catalog::new(vec![title("sn1", 1, "P1")], places.clone()).is_ok());
        assert!(Catalog::new(vec![title("sn1", 1, "P9")], places.clone()).is_err());
        assert!(Catalog::new(
            vec![title("sn1", 1, "P1"), title("sn2", 1, "P1")],
            places.clone()
        )
        .is_err());
        assert!(Catalog::new(vec![title("SN1", 1, "P1")], places.clone()).is_err());
        assert!(Catalog::new(vec![], vec![place("P1", 1), place("P2", 1)]).is_err());
    }

    #[test]
    fn incremental_releases_keep_published_records() {
        let published = Catalog::new(vec![title("sn1", 1, "P1")], vec![place("P1", 1)]).unwrap();
        let mut moved = title("sn1", 1, "P2");
        moved.name = "renamed".into();
        let current = Catalog::new(
            vec![moved, title("sn2", 2, "P2")],
            vec![place("P1", 1), place("P2", 2)],
        )
        .unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        assert_eq!(c.title("sn1").unwrap().place_id, "P1");
        // sn2 is new in P2, which took in sn1 (published in P1): it joins sn1.
        assert_eq!(c.title("sn2").unwrap().place_id, "P1");
        assert_eq!(c.places.len(), 2);
        assert_eq!(Catalog::regrouped(&published, &current), ["sn1"]);
        assert!(Catalog::regrouped(&published, &published).is_empty());
    }

    #[test]
    fn new_titles_of_a_merged_place_go_where_its_titles_are_published() {
        // Published: sn1 in P1, sn2 in P2 (two spellings of one town).
        let published = Catalog::new(
            vec![title("sn1", 1, "P1"), title("sn2", 2, "P2")],
            vec![place("P1", 1), place("P2", 2)],
        )
        .unwrap();
        // Now merged into P3; sn3 is new; sn4 is new in a town of its own.
        let current = Catalog::new(
            vec![
                title("sn1", 1, "P3"),
                title("sn2", 2, "P3"),
                title("sn3", 3, "P3"),
                title("sn4", 4, "P4"),
            ],
            vec![
                place("P1", 1),
                place("P2", 2),
                place("P3", 3),
                place("P4", 4),
            ],
        )
        .unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        let place_of = |l| c.title(l).unwrap().place_id.as_str();
        assert_eq!(
            (
                place_of("sn1"),
                place_of("sn2"),
                place_of("sn3"),
                place_of("sn4")
            ),
            ("P1", "P2", "P1", "P4"),
            "a tie goes to the lower ordinal"
        );
        // Merged into one of the published places: new titles go there.
        let current = Catalog::new(
            vec![
                title("sn1", 1, "P2"),
                title("sn2", 2, "P2"),
                title("sn3", 3, "P2"),
            ],
            vec![place("P1", 1), place("P2", 2)],
        )
        .unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        assert_eq!(c.title("sn3").unwrap().place_id, "P2");
        assert_eq!(c.title("sn1").unwrap().place_id, "P1");
        assert_eq!(Catalog::regrouped(&published, &current), ["sn1"]);
    }

    #[test]
    fn incremental_releases_take_corrected_points() {
        let published = Catalog::new(vec![title("sn1", 1, "P1")], vec![place("P1", 1)]).unwrap();
        let mut moved = place("P1", 1);
        (moved.name, moved.lat, moved.lon) = ("Memphis".into(), 35.15, -90.05);
        moved.coordinates_from = Some("override".into());
        moved.variants = vec![Variant {
            name: "Memphis Tenn".into(),
            rule: "spelling".into(),
        }];
        let current = Catalog::new(vec![title("sn1", 1, "P1")], vec![moved.clone()]).unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        assert_eq!(c.place("P1").unwrap(), &moved);
        assert!(Catalog::regrouped(&published, &current).is_empty());
    }

    #[test]
    fn a_place_without_a_source_reads_and_writes_as_before() {
        let json = r#"{"id":"P1","ordinal":1,"name":"A","state":"IL","lat":41.0,"lon":-87.0,"precision":"city"}"#;
        let p: Place = serde_json::from_str(json).unwrap();
        assert_eq!((p.coordinates_from.as_deref(), p.variants.len()), (None, 0));
        assert_eq!(serde_json::to_string(&p).unwrap(), json);
        let p = Place {
            coordinates_from: Some("gazetteer".into()),
            variants: vec![Variant {
                name: "Skaguay".into(),
                rule: "alias".into(),
            }],
            ..p
        };
        let back: Place = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    /// sn1 was in P1 to 1833-11-13 and is in P2 from 1833-11-14.
    fn moved() -> Title {
        let mut t = title("sn1", 1, "P2");
        t.place_periods = vec![
            PlacePeriod {
                place_id: "P1".into(),
                from: None,
                to: Some(day("1833-11-13")),
            },
            PlacePeriod {
                place_id: "P2".into(),
                from: Some(day("1833-11-14")),
                to: None,
            },
        ];
        t
    }

    #[test]
    fn a_page_is_in_the_place_of_its_date() {
        let t = moved();
        assert_eq!(t.place_id_on(day("1826-05-01")), "P1");
        assert_eq!(t.place_id_on(day("1833-11-13")), "P1", "the last day is in");
        assert_eq!(
            t.place_id_on(day("1833-11-14")),
            "P2",
            "the first day is in"
        );
        assert_eq!(t.place_id_on(day("1857-04-01")), "P2");
        assert_eq!(t.place_ids().collect::<Vec<_>>(), ["P1", "P2"]);
        // A date in no period is in the title's own place. (`Catalog::new`
        // refuses periods with such a date; this is a title it hasn't seen.)
        let mut gap = moved();
        gap.place_periods[0].from = Some(day("1830-01-01"));
        assert_eq!(gap.place_id_on(day("1826-05-01")), "P2");
        // A title in one place is there whatever the date.
        let one = title("sn2", 2, "P1");
        assert_eq!(one.place_id_on(day("1826-05-01")), "P1");
        assert_eq!(one.place_ids().collect::<Vec<_>>(), ["P1"]);
        // And the page's state is its place's, for a title that moved.
        let c = Catalog::new(vec![t.clone()], vec![place("P1", 1), place("P2", 2)]).unwrap();
        let mut maine = place("P1", 1);
        maine.state = "ME".into();
        assert_eq!(t.state_in(&maine), "ME");
        assert_eq!(
            one.state_in(&maine),
            "IL",
            "a title in one place keeps its own"
        );
        assert_eq!(c.place_on(&t, day("1830-01-01")).unwrap().id, "P1");
        assert_eq!(c.place_on(&t, day("1840-01-01")).unwrap().id, "P2");
    }

    #[test]
    fn a_titles_places_are_each_listed_once() {
        let period = |id: &str, from: Option<&str>, to: Option<&str>| PlacePeriod {
            place_id: id.into(),
            from: from.map(day),
            to: to.map(day),
        };
        // It left P1 and came back, and is in P2 for most of its pages.
        let mut t = title("sn1", 1, "P2");
        t.place_periods = vec![
            period("P1", None, Some("1850-12-31")),
            period("P2", Some("1851-01-01"), Some("1860-12-31")),
            period("P1", Some("1861-01-01"), None),
        ];
        assert_eq!(t.place_ids().collect::<Vec<_>>(), ["P1", "P2"]);
        // A title's own place that no period names is listed too, after them.
        t.place_id = "P3".into();
        assert_eq!(t.place_ids().collect::<Vec<_>>(), ["P1", "P2", "P3"]);
    }

    #[test]
    fn periods_name_known_places() {
        let places = vec![place("P1", 1), place("P2", 2)];
        assert!(Catalog::new(vec![moved()], places.clone()).is_ok());
        let mut t = moved();
        t.place_periods[0].place_id = "P9".into();
        let err = Catalog::new(vec![t], places).unwrap_err().to_string();
        assert!(err.contains("unknown place `P9`"), "{err}");
    }

    /// `catalog/overrides/README.md`: at least two periods, oldest first, the
    /// first without `from`, the last without `to`, each starting the day
    /// after the one before ends, so every date has a place.
    #[test]
    fn periods_cover_every_date_without_a_gap() {
        let places = vec![place("P1", 1), place("P2", 2), place("P3", 3)];
        let period = |id: &str, from: Option<&str>, to: Option<&str>| PlacePeriod {
            place_id: id.into(),
            from: from.map(day),
            to: to.map(day),
        };
        let refused = |periods: Vec<PlacePeriod>| {
            let mut t = moved();
            t.place_periods = periods;
            Catalog::new(vec![t], places.clone())
                .unwrap_err()
                .to_string()
        };
        // Three periods, each the day after the one before.
        let mut three = moved();
        three.place_periods = vec![
            period("P1", None, Some("1833-11-13")),
            period("P2", Some("1833-11-14"), Some("1850-12-31")),
            period("P3", Some("1851-01-01"), None),
        ];
        assert!(Catalog::new(vec![three], places.clone()).is_ok());
        // A period with one day is fine.
        let mut one_day = moved();
        one_day.place_periods = vec![
            period("P1", None, Some("1833-11-13")),
            period("P2", Some("1833-11-14"), Some("1833-11-14")),
            period("P3", Some("1833-11-15"), None),
        ];
        assert!(Catalog::new(vec![one_day], places.clone()).is_ok());

        // One period is a title in one place, which has none.
        let err = refused(vec![period("P1", None, None)]);
        assert!(err.contains("fewer than two"), "{err}");
        // A gap of a day, as the second starting two days after the first.
        let err = refused(vec![
            period("P1", None, Some("1833-11-13")),
            period("P2", Some("1833-11-15"), None),
        ]);
        assert!(
            err.contains("period 2 doesn't start the day after"),
            "{err}"
        );
        // Overlapping, on the same day or earlier, and out of order.
        for from in ["1833-11-13", "1833-01-01"] {
            let err = refused(vec![
                period("P1", None, Some("1833-11-13")),
                period("P2", Some(from), None),
            ]);
            assert!(
                err.contains("period 2 doesn't start the day after"),
                "{err}"
            );
        }
        let err = refused(vec![
            period("P2", Some("1833-11-14"), None),
            period("P1", None, Some("1833-11-13")),
        ]);
        assert!(err.contains("period 1, the first, has a `from`"), "{err}");
        // The first starts with the title, the last runs to its end: a date
        // before or after them would be in no period.
        let err = refused(vec![
            period("P1", Some("1830-01-01"), Some("1833-11-13")),
            period("P2", Some("1833-11-14"), None),
        ]);
        assert!(err.contains("period 1, the first, has a `from`"), "{err}");
        let err = refused(vec![
            period("P1", None, Some("1833-11-13")),
            period("P2", Some("1833-11-14"), Some("1857-12-31")),
        ]);
        assert!(err.contains("period 2, the last, has a `to`"), "{err}");
        // Every other period has both ends.
        let err = refused(vec![
            period("P1", None, Some("1833-11-13")),
            period("P2", Some("1833-11-14"), None),
            period("P3", Some("1851-01-01"), None),
        ]);
        assert!(err.contains("period 2 has no `to`"), "{err}");
        let err = refused(vec![
            period("P1", None, Some("1833-11-13")),
            period("P2", None, Some("1850-12-31")),
            period("P3", Some("1851-01-01"), None),
        ]);
        assert!(err.contains("period 2 has no `from`"), "{err}");
        // A period that ends before it starts.
        let err = refused(vec![
            period("P1", None, Some("1833-11-13")),
            period("P2", Some("1833-11-14"), Some("1833-11-01")),
            period("P3", Some("1833-11-02"), None),
        ]);
        assert!(err.contains("period 2 ends before it starts"), "{err}");
    }

    #[test]
    fn a_title_without_periods_reads_and_writes_as_before() {
        let json =
            r#"{"lccn":"sn1","name":"x","ordinal":1,"place_id":"P1","state":"IL","languages":[]}"#;
        let t: Title = serde_json::from_str(json).unwrap();
        assert!(t.place_periods.is_empty());
        assert_eq!(serde_json::to_string(&t).unwrap(), json);
        // With periods they round trip, the open ends left out.
        let t = moved();
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(
            json["place_periods"],
            serde_json::json!([
                {"place_id": "P1", "to": "1833-11-13"},
                {"place_id": "P2", "from": "1833-11-14"},
            ])
        );
        let back: Title = serde_json::from_value(json).unwrap();
        assert_eq!(back, t);
        assert!(back.extra.is_empty());
    }

    #[test]
    fn an_incremental_release_keeps_a_published_title_in_its_one_place() {
        let published = Catalog::new(
            vec![title("sn1", 1, "P2")],
            vec![place("P1", 1), place("P2", 2)],
        )
        .unwrap();
        // Now sn1 is placed by date, and sn2 is new and placed by date too.
        let mut new = moved();
        new.lccn = "sn2".into();
        new.ordinal = 2;
        let current = Catalog::new(
            vec![moved(), new.clone()],
            vec![place("P1", 1), place("P2", 2), place("P3", 3)],
        )
        .unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        assert!(
            c.title("sn1").unwrap().place_periods.is_empty(),
            "pages already published stay in the one place they were published in"
        );
        assert_eq!(c.title("sn2").unwrap().place_periods, new.place_periods);
        // The change waits for the next full release, which says so.
        assert_eq!(Catalog::regrouped(&published, &current), ["sn1"]);
        assert!(Catalog::regrouped(&current, &current).is_empty());
        // A full release's catalog is the current one.
        assert_eq!(current.title("sn1").unwrap().place_periods.len(), 2);
    }

    #[test]
    fn a_new_titles_periods_follow_a_merged_place() {
        // Published: sn1 in P1, sn2 in P2 (two spellings of one town).
        let published = Catalog::new(
            vec![title("sn1", 1, "P1"), title("sn2", 2, "P2")],
            vec![place("P1", 1), place("P2", 2)],
        )
        .unwrap();
        // Now merged into P3. sn3 is new and was in P3 up to 1900 and in P4
        // after.
        let mut sn3 = title("sn3", 3, "P4");
        sn3.place_periods = vec![
            PlacePeriod {
                place_id: "P3".into(),
                from: None,
                to: Some(day("1900-12-31")),
            },
            PlacePeriod {
                place_id: "P4".into(),
                from: Some(day("1901-01-01")),
                to: None,
            },
        ];
        let current = Catalog::new(
            vec![title("sn1", 1, "P3"), title("sn2", 2, "P3"), sn3],
            vec![
                place("P1", 1),
                place("P2", 2),
                place("P3", 3),
                place("P4", 4),
            ],
        )
        .unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        let ids: Vec<&str> = c
            .title("sn3")
            .unwrap()
            .place_periods
            .iter()
            .map(|p| p.place_id.as_str())
            .collect();
        assert_eq!(ids, ["P1", "P4"], "the merged town keeps its published id");
    }

    /// The published sn1 was in P1 up to 1833-11-13 and in P2 after; now its
    /// early place is P3, and P2 is still where most of its pages are. A new
    /// title with a P3 period goes where sn1's early pages are published,
    /// whichever place of its own it has, so the town keeps one id.
    #[test]
    fn a_new_titles_periods_follow_a_merge_of_a_published_titles_early_place() {
        let places = |n| {
            (1..=n)
                .map(|i| place(&format!("P{i}"), i))
                .collect::<Vec<_>>()
        };
        let period = |id: &str, from: Option<&str>, to: Option<&str>| PlacePeriod {
            place_id: id.into(),
            from: from.map(day),
            to: to.map(day),
        };
        let published = Catalog::new(vec![moved()], places(2)).unwrap();
        let mut sn1 = moved();
        sn1.place_periods[0].place_id = "P3".into();
        let mut sn2 = title("sn2", 2, "P4");
        sn2.place_periods = vec![
            period("P3", None, Some("1900-12-31")),
            period("P4", Some("1901-01-01"), None),
        ];
        let current = Catalog::new(vec![sn1, sn2], places(4)).unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        let ids = |lccn: &str| -> Vec<String> {
            let t = c.title(lccn).unwrap();
            t.place_periods.iter().map(|p| p.place_id.clone()).collect()
        };
        assert_eq!(ids("sn1"), ["P1", "P2"], "the published pages stay in P1");
        assert_eq!(ids("sn2"), ["P1", "P4"], "and so do the new title's");
        assert_eq!(c.title("sn2").unwrap().place_id, "P4");

        // Periods that cover other dates now don't say it is the same town.
        let mut sn1 = moved();
        sn1.place_periods[0].place_id = "P3".into();
        sn1.place_periods[0].to = Some(day("1840-12-31"));
        sn1.place_periods[1].from = Some(day("1841-01-01"));
        let mut sn2 = title("sn2", 2, "P4");
        sn2.place_periods = vec![
            period("P3", None, Some("1900-12-31")),
            period("P4", Some("1901-01-01"), None),
        ];
        let current = Catalog::new(vec![sn1, sn2], places(4)).unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        let ids: Vec<&str> = c
            .title("sn2")
            .unwrap()
            .place_periods
            .iter()
            .map(|p| p.place_id.as_str())
            .collect();
        assert_eq!(ids, ["P3", "P4"]);
    }

    #[test]
    fn extra_title_fields_pass_through() {
        let t: Title = serde_json::from_str(
            r#"{"lccn":"sn1","name":"x","ordinal":1,"place_id":"P1","state":"IL","first":"1895-01-01"}"#,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&t).unwrap()["first"], "1895-01-01");
    }
}
