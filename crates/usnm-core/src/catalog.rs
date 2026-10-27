//! Shared reference-catalog place rows, without catalog tooling or index dependencies.

use serde::{Deserialize, Serialize};

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
