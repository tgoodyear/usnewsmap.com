//! Titles and places: the catalog that pages are joined with (04 §4.6).
//!
//! `titles-sync` and `geocode` ([`crate::titles`]) maintain
//! `catalog/titles.json` and `catalog/places.json` in the reference store. Each published reference snapshot carries its own copy,
//! so a published version never changes when the catalog does.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Context};
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Title {
    pub lccn: String,
    pub name: String,
    /// Stable integer, never reused: the leading part of a hit's same-day
    /// `sort_key` (05 §5.5).
    pub ordinal: u32,
    pub place_id: String,
    pub state: String,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Anything else the catalog records (dates, ethnicity, …), passed through.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
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

    /// For an incremental release: titles already published keep their
    /// published record, so their pages stay with the place the published
    /// indexes give them (changes, a move to another place included, wait
    /// for the next full rebuild, 04 §4.7). A new title whose place took in
    /// published titles from another place (a merge) goes where those are
    /// published, so the version doesn't gain a second id for the town. A
    /// published place keeps its id, ordinal and state but takes its name,
    /// coordinates and precision from the current catalog: index documents
    /// carry only the place's id and shard, so a corrected point needs no
    /// reindex (04 §4.6). New places come from the current catalog.
    pub fn carried_forward(published: &Catalog, current: &Catalog) -> anyhow::Result<Self> {
        // For each current place, the published places of its published
        // titles, with how many.
        let mut published_as: HashMap<&str, BTreeMap<&str, usize>> = HashMap::new();
        for t in &published.titles {
            if let Some(now) = current.title(&t.lccn) {
                *published_as
                    .entry(now.place_id.as_str())
                    .or_default()
                    .entry(t.place_id.as_str())
                    .or_default() += 1;
            }
        }
        let mut titles = published.titles.clone();
        for t in &current.titles {
            if published.title(&t.lccn).is_some() {
                continue;
            }
            let mut t = t.clone();
            if let Some(ids) = published_as.get(t.place_id.as_str()) {
                if !ids.contains_key(t.place_id.as_str()) {
                    // The published place with most of them, then the lowest ordinal.
                    let ordinal = |id: &str| published.place(id).map_or(u32::MAX, |p| p.ordinal);
                    if let Some((id, _)) = ids
                        .iter()
                        .max_by_key(|(id, n)| (**n, std::cmp::Reverse(ordinal(id))))
                    {
                        t.place_id = (*id).to_owned();
                    }
                }
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
    /// merge, an alias, a corrected city), in LCCN order: an incremental
    /// release keeps them where they were published (04 §4.6).
    pub fn regrouped(published: &Catalog, current: &Catalog) -> Vec<String> {
        published
            .titles
            .iter()
            .filter(|t| {
                current
                    .title(&t.lccn)
                    .is_some_and(|now| now.place_id != t.place_id)
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
        let current = Catalog::new(vec![title("sn1", 1, "P1")], vec![moved.clone()]).unwrap();
        let c = Catalog::carried_forward(&published, &current).unwrap();
        assert_eq!(c.place("P1").unwrap(), &moved);
        assert!(Catalog::regrouped(&published, &current).is_empty());
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
