//! Place names for `geocode` (04 §4.6): how a title's city is cleaned up,
//! keyed (so spelling variants of one town share a place), renamed by an
//! alias, named for display, and found in the US gazetteer.
//!
//! - **Gazetteer** (`catalog/gazetteer/us-places.csv`, compiled in): the
//!   Census Bureau's national places file reduced to `state,name,lat,lon`
//!   by `scripts/build-gazetteer.py` (public domain, a US government work).
//! - **Aliases** (`catalog/overrides/place-aliases.json`, compiled in):
//!   misspellings and renamed towns, `[{from, note, state, to}]`, applied to
//!   a city before it is keyed.
//! - **Keys** ignore case, accents, punctuation and spacing, and fold the
//!   generic variants LoC's records use: "St."/"Saint", "Ste."/"Sainte",
//!   "Mt."/"Mount", "Ft."/"Fort", a leading "The", "City of", "Borough of",
//!   "Town of", "Village of", a trailing
//!   state name or abbreviation ("Chicago Ill."), "Court House"/"C.H."
//!   (unless the gazetteer knows the full name, as for Washington Court
//!   House, Ohio), "M'" for "Mc", and LoC's "X i.e. Y" corrections.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{bail, Context};
use serde::Deserialize;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;
use usnm_core::names::{State, STATES};

/// `state,name,lat,lon`, from `scripts/build-gazetteer.py`.
pub const GAZETTEER: &str = include_str!("../../../catalog/gazetteer/us-places.csv");

/// `[{from, note, state, to}]`, kept in git and compiled in like the place
/// overrides.
pub const PLACE_ALIASES: &str = include_str!("../../../catalog/overrides/place-aliases.json");

/// `[{city, lccn, note, state}]`: the place of a title whose LoC record
/// lists several, kept in git and compiled in.
pub const TITLE_PLACES: &str = include_str!("../../../catalog/overrides/title-places.json");

/// Real place names with a hyphen between capitalized words, which the
/// display rule would otherwise turn into a space (most come from the
/// gazetteer's spelling anyway; these are for when it has none).
const KEEP_HYPHENS: &[&str] = &[
    "Fuquay-Varina",
    "Jah-Ville",
    "Milton-Freewater",
    "Pointe-A-La-Hache",
    "Sedro-Woolley",
    "Wilkes-Barre",
    "Winston-Salem",
];

/// One town of the gazetteer.
#[derive(Debug, Clone, PartialEq)]
pub struct GazPlace {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// The gazetteer, by (state code, key).
#[derive(Debug, Default)]
pub struct Gazetteer {
    /// The first place of each key, and how many places have it.
    places: HashMap<(&'static str, String), (GazPlace, usize)>,
}

impl Gazetteer {
    /// Parse `state,name,lat,lon` lines after a header. Of two places with
    /// one key in a state (the file lists the preferred one first), the
    /// first is kept and the key counts as ambiguous.
    pub fn parse(csv: &str) -> anyhow::Result<Self> {
        let mut places = HashMap::new();
        let mut lines = csv.lines();
        if lines.next().map(str::trim) != Some("state,name,lat,lon") {
            bail!("the gazetteer's header isn't `state,name,lat,lon`");
        }
        for (i, line) in lines.enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let at = || format!("gazetteer line {}", i + 2);
            let f: Vec<&str> = line.split(',').collect();
            let [code, name, lat, lon] = f.as_slice() else {
                bail!("{}: expected 4 fields", at());
            };
            let state = STATES
                .iter()
                .find(|s| s.code == *code)
                .with_context(|| format!("{}: unknown state `{code}`", at()))?;
            let lat: f64 = lat.parse().with_context(at)?;
            let lon: f64 = lon.parse().with_context(at)?;
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                bail!("{}: coordinates out of range", at());
            }
            let (key, _) = keys(name);
            if key.is_empty() {
                bail!("{}: an empty name", at());
            }
            places
                .entry((state.code, key))
                .or_insert((
                    GazPlace {
                        name: (*name).to_owned(),
                        lat,
                        lon,
                    },
                    0,
                ))
                .1 += 1;
        }
        Ok(Self { places })
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// The town with exactly this key (the preferred one, if several).
    pub fn get(&self, state: &str, key: &str) -> Option<&GazPlace> {
        self.entry(state, key).map(|(p, _)| p)
    }

    /// How many towns of the state have this key.
    pub fn count(&self, state: &str, key: &str) -> usize {
        self.entry(state, key).map_or(0, |(_, n)| *n)
    }

    fn entry(&self, state: &str, key: &str) -> Option<&(GazPlace, usize)> {
        let code = STATES.iter().find(|s| s.code == state)?.code;
        self.places.get(&(code, key.to_owned()))
    }

    /// The town with this key, else "X" for "X City" or "X City" for "X"
    /// (LoC's "Langston City" is the gazetteer's Langston).
    pub fn near(&self, state: &str, key: &str) -> Option<&GazPlace> {
        self.get(state, key)
            .or_else(|| {
                key.strip_suffix("city")
                    .filter(|b| !b.is_empty())
                    .and_then(|b| self.get(state, b))
            })
            .or_else(|| self.get(state, &format!("{key}city")))
    }
}

/// One entry of [`PLACE_ALIASES`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaceAlias {
    /// The city as LoC spells it (any spelling with the same key matches).
    pub from: String,
    /// Why: "misspelling", "renamed 1884", …
    #[serde(default)]
    pub note: Option<String>,
    /// Postal code, e.g. `AK`.
    pub state: String,
    /// The town's name, used as the place's name.
    pub to: String,
}

/// One entry of [`TITLE_PLACES`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitlePlace {
    /// The city, as a title's place label would name it.
    pub city: String,
    pub lccn: String,
    /// Why: where the paper was published, and when.
    #[serde(default)]
    pub note: Option<String>,
    /// Postal code, e.g. `IL`.
    pub state: String,
}

/// The gazetteer, the aliases and the per-title places: what `geocode`
/// uses to place, key and name places.
#[derive(Debug, Default)]
pub struct Geo {
    pub gazetteer: Gazetteer,
    aliases: HashMap<(&'static str, String), String>,
    /// LCCN → (city, state code).
    titles: HashMap<String, (String, &'static str)>,
}

/// A title's city, cleaned and keyed.
#[derive(Debug, Clone, PartialEq)]
pub struct CityName {
    /// Equal for every spelling of one town.
    pub key: String,
    /// The alias's name for the town, when one renamed it.
    pub alias: Option<String>,
    /// LoC's spelling, cleaned up (no state suffix, "City of", …).
    pub clean: String,
}

impl Geo {
    /// The gazetteer and aliases compiled into the binary.
    pub fn compiled() -> anyhow::Result<&'static Geo> {
        static GEO: OnceLock<Result<Geo, String>> = OnceLock::new();
        GEO.get_or_init(|| {
            let aliases: Vec<PlaceAlias> = serde_json::from_str(PLACE_ALIASES)
                .context("catalog/overrides/place-aliases.json")
                .map_err(|e| format!("{e:#}"))?;
            let gazetteer = Gazetteer::parse(GAZETTEER)
                .context("catalog/gazetteer/us-places.csv")
                .map_err(|e| format!("{e:#}"))?;
            let titles: Vec<TitlePlace> = serde_json::from_str(TITLE_PLACES)
                .context("catalog/overrides/title-places.json")
                .map_err(|e| format!("{e:#}"))?;
            Geo::new(gazetteer, &aliases)
                .and_then(|g| g.with_title_places(&titles))
                .map_err(|e| format!("{e:#}"))
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Check the aliases: known states, each `from` once per state, `to` a
    /// different town, and no chains (a `to` that is another alias's `from`).
    pub fn new(gazetteer: Gazetteer, aliases: &[PlaceAlias]) -> anyhow::Result<Self> {
        let mut geo = Self {
            gazetteer,
            ..Self::default()
        };
        let mut targets = Vec::new();
        for a in aliases {
            let state = STATES
                .iter()
                .find(|s| s.code == a.state)
                .with_context(|| format!("alias `{}`: unknown state `{}`", a.from, a.state))?;
            let from = geo.key(&clean_city(&a.from, state), state);
            let to = geo.key(&clean_city(&a.to, state), state);
            if from.is_empty() || to.is_empty() || from == to {
                bail!(
                    "alias `{}` → `{}` ({}): not two different towns",
                    a.from,
                    a.to,
                    a.state
                );
            }
            if geo
                .aliases
                .insert((state.code, from), a.to.clone())
                .is_some()
            {
                bail!("alias `{}` ({}) is listed twice", a.from, a.state);
            }
            targets.push((state.code, to, &a.to));
        }
        for (code, to, name) in targets {
            if geo.aliases.contains_key(&(code, to)) {
                bail!("alias target `{name}` ({code}) is itself an alias: name the final town");
            }
        }
        Ok(geo)
    }

    /// Place these titles in the city and state given, whatever their LoC
    /// record lists. Each LCCN once, in a known state.
    pub fn with_title_places(mut self, entries: &[TitlePlace]) -> anyhow::Result<Self> {
        for t in entries {
            let state = STATES.iter().find(|s| s.code == t.state).with_context(|| {
                format!("title place `{}`: unknown state `{}`", t.lccn, t.state)
            })?;
            if t.city.trim().is_empty()
                || usnm_core::ids::PageKey::new(&t.lccn, chrono::NaiveDate::MIN, 1, 1).is_err()
            {
                bail!("title place `{}`: an invalid LCCN or no city", t.lccn);
            }
            if self
                .titles
                .insert(t.lccn.clone(), (t.city.trim().to_owned(), state.code))
                .is_some()
            {
                bail!("title place `{}` is listed twice", t.lccn);
            }
        }
        Ok(self)
    }

    /// The city and state a title is placed in by [`TITLE_PLACES`].
    pub fn title_place(&self, lccn: &str) -> Option<(&str, &'static State)> {
        let (city, code) = self.titles.get(lccn)?;
        Some((city.as_str(), STATES.iter().find(|s| s.code == *code)?))
    }

    /// Whether the gazetteer has `city` in `state` (as "X" or "X City").
    pub fn has(&self, city: &str, state: &State) -> bool {
        let key = self.city(city, state).key;
        !key.is_empty() && self.gazetteer.near(state.code, &key).is_some()
    }

    /// `city` (in `state`) cleaned up, renamed by an alias, and keyed.
    pub fn city(&self, city: &str, state: &State) -> CityName {
        let clean = clean_city(city, state);
        let key = self.key(&clean, state);
        match self.aliases.get(&(state.code, key.clone())) {
            Some(to) => CityName {
                key: self.key(&clean_city(to, state), state),
                alias: Some(to.clone()),
                clean,
            },
            None => CityName {
                key,
                alias: None,
                clean,
            },
        }
    }

    /// The key of a cleaned name; "Court House" is dropped unless the
    /// gazetteer has the full name in that state.
    fn key(&self, clean: &str, state: &State) -> String {
        match keys(clean) {
            (full, Some(short))
                if !short.is_empty() && self.gazetteer.get(state.code, &full).is_none() =>
            {
                short
            }
            (full, _) => full,
        }
    }
}

/// The words of a name: accents folded, lowercase, apostrophes dropped
/// ("Harper's" → "harpers"), split at anything else that isn't a letter or
/// digit.
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.nfd().filter(|c| !is_combining_mark(*c)) {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if c == '\'' || c == '\u{2019}' {
            // Within a word.
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The words that make a key: no leading "The", and "St.", "Ste.", "Mt.",
/// "Ft." spelled out.
fn key_words(name: &str) -> Vec<String> {
    let mut w = words(name);
    if w.len() > 1 && w[0] == "the" {
        w.remove(0);
    }
    for x in &mut w {
        let long = match x.as_str() {
            "st" => "saint",
            "ste" => "sainte",
            "mt" => "mount",
            "ft" => "fort",
            _ => continue,
        };
        *x = long.to_owned();
    }
    w
}

/// A cleaned name's key, and the key without a trailing "Court House" or
/// "C.H." when it has one.
fn keys(clean: &str) -> (String, Option<String>) {
    let w = key_words(clean);
    let n = w.len();
    let court = if n > 2 && (w[n - 2..] == ["court", "house"] || w[n - 2..] == ["c", "h"]) {
        Some(n - 2)
    } else if n > 1 && w[n - 1] == "courthouse" {
        Some(n - 1)
    } else {
        None
    };
    (w.concat(), court.map(|i| w[..i].concat()))
}

/// Whether the gazetteer's `name` is only another spelling of LoC's `loc`
/// (case, accents, "St."/"Saint", "The"), or `loc` has punctuation the
/// gazetteer's spelling fixes ("Newbury-Port", "Tarboro'"). LoC's "Tule
/// Lake" (the camp) isn't the gazetteer's "Tulelake" (the town), though
/// both key alike.
pub fn respelling(loc: &str, name: &str) -> bool {
    loc.contains(['-', '.', '\'', '\u{2019}']) || key_words(loc) == key_words(name)
}

/// LoC's spelling of a city, tidied: LoC's own correction of a misprint
/// ("Nueva-Orleans i.e. New Orleans" → "New Orleans"), no trailing state
/// ("Little Rock Ark." → "Little Rock"), no "City of"/"Borough of" prefix,
/// and "M'" as "Mc" ("M'arthur" → "McArthur").
pub fn clean_city(city: &str, state: &State) -> String {
    let mut s = city.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(i) = s.find(" i.e. ") {
        let rest = s[i + " i.e. ".len()..].trim();
        if !rest.is_empty() {
            s = rest.to_owned();
        }
    }
    for form in std::iter::once(state.name).chain(state.abbrevs.iter().copied()) {
        if let Some(cut) = s.len().checked_sub(form.len()) {
            let head = s.get(..cut).unwrap_or("");
            let tail = s.get(cut..).unwrap_or("");
            if cut > 0 && tail.eq_ignore_ascii_case(form) && head.ends_with([' ', ',']) {
                let head = head.trim_end_matches([' ', ',']);
                if !head.is_empty() {
                    s = head.to_owned();
                    break;
                }
            }
        }
    }
    for prefix in ["City of ", "Borough of ", "Town of ", "Village of "] {
        if s.len() > prefix.len()
            && s.get(..prefix.len())
                .is_some_and(|h| h.eq_ignore_ascii_case(prefix))
        {
            s = s[prefix.len()..].trim_start().to_owned();
            break;
        }
    }
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match (c.next(), c.next(), c.next()) {
                (Some('M' | 'm'), Some('\'' | '\u{2019}'), Some(l)) if l.is_alphabetic() => {
                    format!("Mc{}{}", l.to_uppercase(), c.as_str())
                }
                _ => w.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// LoC's 19th-century hyphens ("New-York", "Baton-Rouge") as spaces: a
/// hyphen between two capitalized words, except in real hyphenated names.
pub fn unhyphenate(name: &str) -> String {
    if KEEP_HYPHENS.iter().any(|k| k.eq_ignore_ascii_case(name)) {
        return name.to_owned();
    }
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    for (i, &c) in chars.iter().enumerate() {
        let capital_before = || {
            let start = chars[..i]
                .iter()
                .rposition(|c| !c.is_alphabetic())
                .map_or(0, |p| p + 1);
            start < i && chars[start].is_uppercase()
        };
        let capital_after = chars.get(i + 1).is_some_and(|c| c.is_uppercase());
        if c == '-' && capital_after && capital_before() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Great-circle distance in km.
pub fn distance_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (
        a.0.to_radians(),
        a.1.to_radians(),
        b.0.to_radians(),
        b.1.to_radians(),
    );
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(code: &str) -> &'static State {
        STATES.iter().find(|s| s.code == code).unwrap()
    }

    fn key(city: &str, code: &str) -> String {
        Geo::default().city(city, st(code)).key
    }

    #[test]
    fn spelling_variants_share_a_key() {
        for (a, b, code) in [
            ("St. Paul", "Saint Paul", "MN"),
            ("Ste. Genevieve", "Sainte Genevieve", "MO"),
            ("Mt. Vernon", "Mount Vernon", "KY"),
            ("Ft. Fetterman", "Fort Fetterman", "WY"),
            ("Chicago Ill.", "Chicago", "IL"),
            ("Little Rock Ark.", "Little Rock", "AR"),
            ("Nome Alaska", "Nome", "AK"),
            ("Cheraw S.C.", "Cheraw", "SC"),
            ("Cincinnati Ohio", "Cincinnati", "OH"),
            ("Wheeling, W. Va.", "Wheeling", "WV"),
            ("Anderson Court House", "Anderson", "SC"),
            ("Union C.H.", "Union", "SC"),
            ("Jackson C.H.", "Jackson", "OH"),
            ("City of Washington", "Washington", "DC"),
            ("City of Council Bluffs", "Council Bluffs", "IA"),
            (
                "Borough of Peapack and Gladstone",
                "Peapack and Gladstone",
                "NJ",
            ),
            ("M'arthur", "McArthur", "OH"),
            ("M'gregor", "McGregor", "IA"),
            ("Cañon City", "Canon City", "CO"),
            ("The Dalles", "Dalles", "OR"),
            ("New-York", "New York", "NY"),
            ("Newbury-Port", "Newburyport", "MA"),
            ("Tarboro'", "Tarboro", "NC"),
            ("Harper's Ferry", "Harpers Ferry", "WV"),
            ("Nueva-Orleans i.e. New Orleans", "New Orleans", "LA"),
        ] {
            assert_eq!(key(a, code), key(b, code), "{a} / {b}");
        }
        // Different towns stay apart.
        for (a, b, code) in [
            ("East Providence", "Providence", "RI"),
            ("East Las Vegas", "Las Vegas", "NM"),
            ("Carlisle Barracks", "Carlisle", "PA"),
            ("Winston", "Winston-Salem", "NC"),
            ("Yazoo City", "Yazoo", "MS"),
            ("Elizabeth-Town", "Elizabeth", "NJ"),
        ] {
            assert_ne!(key(a, code), key(b, code), "{a} / {b}");
        }
        // A city named for its state keeps its name.
        assert_eq!(key("Delaware", "DE"), "delaware");
        assert_eq!(key("Iowa City", "IA"), "iowacity");
        assert_eq!(clean_city("Rohwer Ark.", st("AR")), "Rohwer");
        assert_eq!(clean_city("M'connelsville", st("OH")), "McConnelsville");
    }

    #[test]
    fn court_house_stays_when_the_gazetteer_names_it() {
        let g =
            Gazetteer::parse("state,name,lat,lon\nOH,Washington Court House,39.5,-83.4\n").unwrap();
        let geo = Geo::new(g, &[]).unwrap();
        assert_eq!(
            geo.city("Washington Court House", st("OH")).key,
            "washingtoncourthouse"
        );
        assert_eq!(geo.city("Jackson C.H.", st("OH")).key, "jackson");
    }

    #[test]
    fn aliases_rename_before_keying() {
        let alias = |from: &str, to: &str, state: &str| PlaceAlias {
            from: from.into(),
            note: None,
            state: state.into(),
            to: to.into(),
        };
        let geo = Geo::new(
            Gazetteer::default(),
            &[
                alias("Skaguay", "Skagway", "AK"),
                alias("Elizabeth-Town", "Elizabeth", "NJ"),
            ],
        )
        .unwrap();
        let c = geo.city("Skaguay Alaska", st("AK"));
        assert_eq!(
            (c.key.as_str(), c.alias.as_deref(), c.clean.as_str()),
            ("skagway", Some("Skagway"), "Skaguay")
        );
        assert_eq!(geo.city("Elizabeth-Town", st("NJ")).key, "elizabeth");
        // Only in the alias's state.
        assert_eq!(geo.city("Elizabeth-Town", st("KY")).key, "elizabethtown");
        assert_eq!(geo.city("Skagway", st("AK")).alias, None);

        // Chains, repeats, unknown states and no-ops are refused.
        let bad = |a: &[PlaceAlias]| Geo::new(Gazetteer::default(), a).is_err();
        assert!(bad(&[alias("A", "B", "AK"), alias("B", "C", "AK")]));
        assert!(bad(&[alias("A", "B", "AK"), alias("a.", "C", "AK")]));
        assert!(bad(&[alias("A", "B", "XX")]));
        assert!(bad(&[alias("St. Paul", "Saint Paul", "MN")]));
        assert!(!bad(&[alias("A", "B", "AK"), alias("B", "C", "AL")]));
    }

    #[test]
    fn the_gazetteer_respells_only_alike_words() {
        assert!(respelling("Canon City", "Cañon City"));
        assert!(respelling("Bay Saint Louis", "Bay St. Louis"));
        assert!(respelling("Coeur D'alene", "Coeur d'Alene"));
        assert!(respelling("Newbury-Port", "Newburyport"));
        assert!(respelling("Dalles", "The Dalles"));
        assert!(!respelling("Tule Lake", "Tulelake"));
        assert!(!respelling("Leonard Town", "Leonardtown"));
    }

    #[test]
    fn display_names_drop_old_hyphens() {
        assert_eq!(unhyphenate("New-York"), "New York");
        assert_eq!(unhyphenate("Baton-Rouge"), "Baton Rouge");
        assert_eq!(unhyphenate("Terre-Haute"), "Terre Haute");
        assert_eq!(unhyphenate("Winston-Salem"), "Winston-Salem");
        assert_eq!(unhyphenate("Wilkes-Barre"), "Wilkes-Barre");
        assert_eq!(unhyphenate("Croton-on-Hudson"), "Croton-on-Hudson");
    }

    #[test]
    fn parses_the_gazetteer() {
        let g = Gazetteer::parse(
            "state,name,lat,lon\nMN,St. Paul,44.95,-93.1\nMN,Saint Paul,1,1\nOR,The Dalles,45.6,-121.18\nOK,Langston,35.94,-97.26\n",
        )
        .unwrap();
        assert_eq!(g.len(), 3, "one entry per key");
        assert_eq!(g.get("MN", "saintpaul").unwrap().name, "St. Paul");
        assert_eq!(
            (g.count("MN", "saintpaul"), g.count("OR", "dalles")),
            (2, 1)
        );
        assert_eq!(g.count("OR", "nowhere"), 0);
        assert_eq!(g.get("OR", "dalles").unwrap().lat, 45.6);
        assert_eq!(g.near("OK", "langstoncity").unwrap().name, "Langston");
        assert!(g.get("XX", "x").is_none());
        assert!(Gazetteer::parse("name,lat\n").is_err());
        assert!(Gazetteer::parse("state,name,lat,lon\nXX,A,1,1\n").is_err());
        assert!(Gazetteer::parse("state,name,lat,lon\nMN,A,100,1\n").is_err());
    }

    #[test]
    fn the_compiled_gazetteer_and_aliases_load() {
        let geo = Geo::compiled().unwrap();
        assert!(geo.gazetteer.len() > 30_000);
        let nyc = geo.gazetteer.get("NY", "newyork").unwrap();
        assert!(distance_km((nyc.lat, nyc.lon), (40.71, -74.0)) < 25.0);
        assert_eq!(
            geo.gazetteer.get("HI", "honolulu").unwrap().name,
            "Honolulu"
        );
        assert_eq!(geo.city("Skaguay", st("AK")).key, "skagway");
    }

    #[test]
    fn the_aliases_in_git_are_sorted() {
        let entries: Vec<serde_json::Value> = serde_json::from_str(PLACE_ALIASES).unwrap();
        let order: Vec<(String, String)> = entries
            .iter()
            .map(|e| {
                (
                    e["state"].as_str().unwrap().to_owned(),
                    e["from"].as_str().unwrap().to_lowercase(),
                )
            })
            .collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "entries out of order (by state, then from)");
        // Each object's keys in ascending order, as written.
        for keys in object_keys_in_order(PLACE_ALIASES) {
            let mut s = keys.clone();
            s.sort();
            assert_eq!(keys, s, "keys out of order");
        }
    }

    /// The keys of each object of a JSON array, in the order the text has
    /// them (a parsed map would sort them).
    pub(crate) fn object_keys_in_order(json: &str) -> Vec<Vec<String>> {
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
        serde_json::from_str::<Vec<Keys>>(json)
            .unwrap()
            .into_iter()
            .map(|k| k.0)
            .collect()
    }

    #[test]
    fn the_title_places_in_git_are_sorted_and_valid() {
        let entries: Vec<TitlePlace> = serde_json::from_str(TITLE_PLACES).unwrap();
        let lccns: Vec<&str> = entries.iter().map(|t| t.lccn.as_str()).collect();
        let mut sorted = lccns.clone();
        sorted.sort();
        assert_eq!(lccns, sorted, "entries out of order (by lccn)");
        for keys in object_keys_in_order(TITLE_PLACES) {
            let mut s = keys.clone();
            s.sort();
            assert_eq!(keys, s, "keys out of order");
        }
        let geo = Geo::compiled().unwrap();
        let (city, state) = geo.title_place("sn84024055").unwrap();
        assert_eq!((city, state.code), ("Chicago", "IL"));
        let bad = |json: &str| {
            let t: Vec<TitlePlace> = serde_json::from_str(json).unwrap();
            Geo::default().with_title_places(&t).is_err()
        };
        assert!(bad(r#"[{"city": "X", "lccn": "sn1", "state": "XX"}]"#));
        assert!(bad(r#"[{"city": "X", "lccn": "BAD/1", "state": "IL"}]"#));
        assert!(bad(
            r#"[{"city": "X", "lccn": "sn1", "state": "IL"}, {"city": "Y", "lccn": "sn1", "state": "IL"}]"#
        ));
    }

    #[test]
    fn the_place_overrides_in_git_have_sorted_keys() {
        for keys in object_keys_in_order(crate::titles::PLACE_OVERRIDES) {
            let mut s = keys.clone();
            s.sort();
            assert_eq!(keys, s, "keys out of order");
        }
    }

    #[test]
    fn distances() {
        let d = distance_km((35.1495, -90.049), (32.4074, -87.0211));
        assert!((d - 411.0).abs() < 10.0, "Memphis to Selma: {d}");
    }
}
