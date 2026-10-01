//! Names LoC uses for languages and states, shared by the ingest pipeline
//! (which turns LoC's title records into codes) and the API (which shows the
//! codes as names again on the status page).

/// LoC's language names (as its title records give them, lowercase) and the
/// MARC codes the catalog stores.
pub static LANGUAGES: &[(&str, &str)] = &[
    ("english", "eng"),
    ("german", "ger"),
    ("spanish", "spa"),
    ("french", "fre"),
    ("italian", "ita"),
    ("polish", "pol"),
    ("czech", "cze"),
    ("slovak", "slo"),
    ("slovenian", "slv"),
    ("lithuanian", "lit"),
    ("swedish", "swe"),
    ("norwegian", "nor"),
    ("danish", "dan"),
    ("dutch", "dut"),
    ("finnish", "fin"),
    ("hungarian", "hun"),
    ("yiddish", "yid"),
    ("hebrew", "heb"),
    ("chinese", "chi"),
    ("japanese", "jpn"),
    ("portuguese", "por"),
    ("russian", "rus"),
    ("ukrainian", "ukr"),
    ("greek", "gre"),
    ("romanian", "rum"),
    ("croatian", "hrv"),
    ("serbian", "srp"),
    ("arabic", "ara"),
    ("armenian", "arm"),
    ("welsh", "wel"),
    ("icelandic", "ice"),
    ("latin", "lat"),
    ("hawaiian", "haw"),
    ("cherokee", "chr"),
    ("choctaw", "cho"),
    ("dakota", "dak"),
    ("ojibwa", "oji"),
    ("basque", "baq"),
    ("irish", "gle"),
    ("tagalog", "tgl"),
    ("korean", "kor"),
    ("estonian", "est"),
    ("latvian", "lav"),
    ("bulgarian", "bul"),
    ("albanian", "alb"),
    ("creek", "mus"),
];

/// LoC language names to MARC codes; anything else stays as LoC wrote it.
pub fn language_code(name: &str) -> String {
    let n = name.trim().to_lowercase();
    LANGUAGES
        .iter()
        .find(|(k, _)| *k == n)
        .map_or(n.clone(), |(_, v)| (*v).to_owned())
}

/// The name for a language as the catalog stores it: LoC's name for a MARC
/// code, capitalized the way loc.gov shows it ("eng" is "English"). A value
/// that isn't a known code is already LoC's own name and is only capitalized.
pub fn language_name(code: &str) -> String {
    let name = LANGUAGES
        .iter()
        .find(|(_, c)| *c == code)
        .map_or(code, |(n, _)| *n);
    capitalize(name)
}

/// The first letter of each word in upper case ("pennsylvania german" is
/// "Pennsylvania German").
fn capitalize(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map(|f| f.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// A state or territory: postal code, LoC's name, the abbreviations titles
/// use, and a rough geographic centre (the `state`-precision fallback).
pub struct State {
    pub code: &'static str,
    pub name: &'static str,
    pub abbrevs: &'static [&'static str],
    pub lat: f64,
    pub lon: f64,
}

/// A state by LoC's name for it (case-insensitive), with the variants LoC
/// also uses for the District of Columbia and the Virgin Islands.
pub fn state_by_name(name: &str) -> Option<&'static State> {
    let n = name.trim().to_lowercase();
    let n = match n.as_str() {
        "washington, d.c." | "washington (d.c.)" | "d.c." => "district of columbia",
        "virgin islands of the united states" | "u.s. virgin islands" => "virgin islands",
        other => other,
    };
    STATES.iter().find(|s| s.name.eq_ignore_ascii_case(n))
}

macro_rules! states {
    ($($code:literal $name:literal [$($a:literal),*] $lat:literal $lon:literal;)*) => {
        &[$(State { code: $code, name: $name, abbrevs: &[$($a),*], lat: $lat, lon: $lon }),*]
    };
}

pub static STATES: &[State] = states! {
    "AL" "Alabama" ["Ala."] 32.8 -86.8;
    "AK" "Alaska" ["Alaska"] 64.2 -149.5;
    "AZ" "Arizona" ["Ariz.", "A.T."] 34.3 -111.7;
    "AR" "Arkansas" ["Ark."] 34.9 -92.4;
    "CA" "California" ["Calif.", "Cal."] 37.2 -119.5;
    "CO" "Colorado" ["Colo.", "Col."] 39.0 -105.5;
    "CT" "Connecticut" ["Conn."] 41.6 -72.7;
    "DE" "Delaware" ["Del."] 39.0 -75.5;
    "DC" "District of Columbia" ["D.C."] 38.9 -77.03;
    "FL" "Florida" ["Fla."] 28.6 -82.4;
    "GA" "Georgia" ["Ga."] 32.7 -83.4;
    "HI" "Hawaii" ["Hawaii", "H.I."] 20.8 -156.3;
    "ID" "Idaho" ["Idaho"] 44.4 -114.6;
    "IL" "Illinois" ["Ill."] 40.0 -89.2;
    "IN" "Indiana" ["Ind."] 39.9 -86.3;
    "IA" "Iowa" ["Iowa"] 42.1 -93.5;
    "KS" "Kansas" ["Kan.", "Kans."] 38.5 -98.4;
    "KY" "Kentucky" ["Ky."] 37.5 -85.3;
    "LA" "Louisiana" ["La."] 31.1 -92.0;
    "ME" "Maine" ["Me."] 45.4 -69.2;
    "MD" "Maryland" ["Md."] 39.0 -76.8;
    "MA" "Massachusetts" ["Mass."] 42.3 -71.8;
    "MI" "Michigan" ["Mich."] 44.3 -85.4;
    "MN" "Minnesota" ["Minn."] 46.3 -94.3;
    "MS" "Mississippi" ["Miss."] 32.7 -89.7;
    "MO" "Missouri" ["Mo."] 38.4 -92.5;
    "MT" "Montana" ["Mont."] 47.0 -109.6;
    "NE" "Nebraska" ["Neb.", "Nebr."] 41.5 -99.8;
    "NV" "Nevada" ["Nev."] 39.3 -116.6;
    "NH" "New Hampshire" ["N.H."] 43.7 -71.6;
    "NJ" "New Jersey" ["N.J."] 40.2 -74.7;
    "NM" "New Mexico" ["N.M.", "N. Mex."] 34.4 -106.1;
    "NY" "New York" ["N.Y."] 42.9 -75.5;
    "NC" "North Carolina" ["N.C."] 35.6 -79.4;
    "ND" "North Dakota" ["N.D.", "N. Dak."] 47.5 -100.5;
    "OH" "Ohio" ["Ohio"] 40.3 -82.8;
    "OK" "Oklahoma" ["Okla.", "Ind. T.", "I.T."] 35.6 -97.5;
    "OR" "Oregon" ["Or.", "Ore.", "Oreg."] 43.9 -120.6;
    "PA" "Pennsylvania" ["Pa."] 40.9 -77.8;
    "RI" "Rhode Island" ["R.I."] 41.7 -71.5;
    "SC" "South Carolina" ["S.C."] 33.9 -80.9;
    "SD" "South Dakota" ["S.D.", "S. Dak."] 44.4 -100.2;
    "TN" "Tennessee" ["Tenn."] 35.9 -86.4;
    "TX" "Texas" ["Tex."] 31.5 -99.3;
    "UT" "Utah" ["Utah"] 39.3 -111.7;
    "VT" "Vermont" ["Vt."] 44.1 -72.7;
    "VA" "Virginia" ["Va."] 37.5 -78.9;
    "WA" "Washington" ["Wash.", "W.T."] 47.4 -120.5;
    "WV" "West Virginia" ["W. Va.", "W.Va."] 38.6 -80.6;
    "WI" "Wisconsin" ["Wis."] 44.6 -89.9;
    "WY" "Wyoming" ["Wyo."] 43.0 -107.6;
    "PR" "Puerto Rico" ["P.R."] 18.2 -66.5;
    "VI" "Virgin Islands" ["V.I."] 18.34 -64.9;
};

/// LoC's name for a state or territory's postal code.
pub fn state_name(code: &str) -> Option<&'static str> {
    STATES.iter().find(|s| s.code == code).map(|s| s.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_round_trip() {
        assert_eq!(language_code(" English "), "eng");
        assert_eq!(language_name("eng"), "English");
        assert_eq!(language_name("ger"), "German");
        assert_eq!(language_name("mus"), "Creek");
        // Unknown names stay LoC's, capitalized for display.
        assert_eq!(language_code("Pennsylvania German"), "pennsylvania german");
        assert_eq!(language_name("pennsylvania german"), "Pennsylvania German");
        for (name, code) in LANGUAGES {
            assert_eq!(language_code(name), *code);
        }
    }

    #[test]
    fn states_by_code_and_name() {
        assert_eq!(state_name("IL"), Some("Illinois"));
        assert_eq!(state_name("DC"), Some("District of Columbia"));
        assert_eq!(state_name("XX"), None);
        assert_eq!(
            state_by_name("washington, d.c.").map(|s| s.code),
            Some("DC")
        );
    }
}
