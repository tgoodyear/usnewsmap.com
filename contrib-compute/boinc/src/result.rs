//! The result file a workunit uploads, and the validator's checks on it.

use crate::similarity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// `result.json`. Fields (and map keys) are in alphabetical order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultFile {
    pub engine: Engine,
    pub pages: Vec<PageResult>,
    pub schema: u32,
    pub wu: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Engine {
    pub name: String,
    /// Traineddata file name (without extension) to its SHA-256 in hex.
    pub traineddata: BTreeMap<String, String>,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alto: Option<String>,
    #[serde(default)]
    pub blank: bool,
    pub input_md5: String,
    pub page_id: String,
    /// Present only for fetch-and-ocr results: what the client downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceInfo>,
    pub text: String,
    pub text_sha256: String,
}

/// What a fetch-and-ocr client downloaded and read. `input_md5` of the page is the MD5 of
/// these bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub bytes: u64,
    /// `jp2` or `jpeg`.
    pub format: String,
    pub sha256: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the validator insists on. Loaded from a JSON file (BOINC's script validator cannot
/// pass flags, so the validator reads `USNM_BOINC_VALIDATOR_CONFIG`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub engine_name: String,
    pub engine_version: String,
    pub max_pages: usize,
    pub max_text_bytes: usize,
    pub min_dice: f64,
    pub min_length_ratio: f64,
    pub traineddata: BTreeMap<String, String>,
}

impl Config {
    /// Defaults from the German pilot: Tesseract 5.5.0 with frak2021, a 0.98 word-overlap
    /// threshold and a 5% length tolerance. The thresholds are not tuned; see design.md 5.2.
    pub fn pilot() -> Config {
        let mut t = BTreeMap::new();
        t.insert(
            "frak2021".to_string(),
            "1da2384254fa8462c776faf3b43307fe19ce51be0931c623b2fdd560f96e299a".to_string(),
        );
        Config {
            engine_name: "tesseract".into(),
            engine_version: "5.5.0".into(),
            max_pages: 16,
            max_text_bytes: 2 * 1024 * 1024,
            min_dice: 0.98,
            min_length_ratio: 0.95,
            traineddata: t,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Invalid {
    Parse(String),
    Schema(u32),
    Engine(String),
    NoPages,
    TooManyPages(usize),
    DuplicatePage(String),
    BadMd5(String),
    TextHash(String),
    EmptyText(String),
    TextTooBig(String),
    BadSource(String),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

pub fn parse(bytes: &[u8]) -> Result<ResultFile, Invalid> {
    serde_json::from_slice(bytes).map_err(|e| Invalid::Parse(e.to_string()))
}

/// Check one result on its own. This is BOINC's "init_result" step.
pub fn check(r: &ResultFile, cfg: &Config) -> Result<(), Invalid> {
    if r.schema != 1 {
        return Err(Invalid::Schema(r.schema));
    }
    if r.engine.name != cfg.engine_name || r.engine.version != cfg.engine_version {
        return Err(Invalid::Engine(format!(
            "{} {}",
            r.engine.name, r.engine.version
        )));
    }
    // Every file the app version pins must be present with the pinned hash. Extra files are
    // not allowed: the result would then come from a different model set.
    if r.engine.traineddata != cfg.traineddata {
        return Err(Invalid::Engine(
            "traineddata does not match the pins".into(),
        ));
    }
    if r.pages.is_empty() {
        return Err(Invalid::NoPages);
    }
    if r.pages.len() > cfg.max_pages {
        return Err(Invalid::TooManyPages(r.pages.len()));
    }
    let mut seen = BTreeSet::new();
    for p in &r.pages {
        if !seen.insert(p.page_id.as_str()) {
            return Err(Invalid::DuplicatePage(p.page_id.clone()));
        }
        if p.input_md5.len() != 32 || !p.input_md5.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Invalid::BadMd5(p.page_id.clone()));
        }
        if p.text.len() > cfg.max_text_bytes {
            return Err(Invalid::TextTooBig(p.page_id.clone()));
        }
        if p.text_sha256 != sha256_hex(p.text.as_bytes()) {
            return Err(Invalid::TextHash(p.page_id.clone()));
        }
        if !p.blank && p.text.trim().is_empty() {
            return Err(Invalid::EmptyText(p.page_id.clone()));
        }
        if let Some(s) = &p.source {
            let ok_format = matches!(s.format.as_str(), "jp2" | "jpeg");
            if !ok_format
                || s.bytes == 0
                || s.sha256.len() != 64
                || !s
                    .sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(Invalid::BadSource(p.page_id.clone()));
            }
        }
    }
    Ok(())
}

/// Outcome of comparing two results that each passed [`check`].
#[derive(Debug, PartialEq)]
pub enum Comparison {
    Match,
    Mismatch(String),
}

/// Compare two results page by page. They must cover the same page ids and the same input
/// MD5s, and every page must meet both thresholds. This is BOINC's "compare_results" step.
pub fn compare(a: &ResultFile, b: &ResultFile, cfg: &Config) -> Comparison {
    if a.pages.len() != b.pages.len() {
        return Comparison::Mismatch("different page counts".into());
    }
    let by_id: BTreeMap<&str, &PageResult> =
        b.pages.iter().map(|p| (p.page_id.as_str(), p)).collect();
    for pa in &a.pages {
        let Some(pb) = by_id.get(pa.page_id.as_str()) else {
            return Comparison::Mismatch(format!("page {} missing from second result", pa.page_id));
        };
        // Every replica of a workunit requests the same pinned URL, so they must have read the
        // same format and the same bytes. A client that verified the source's checksum has
        // already failed the task on a mismatch; this is the check for sources without one.
        if let (Some(x), Some(y)) = (&pa.source, &pb.source) {
            if x.format != y.format {
                return Comparison::Mismatch(format!("page {} read different formats", pa.page_id));
            }
        }
        if pa.input_md5 != pb.input_md5 {
            return Comparison::Mismatch(format!("page {} read different inputs", pa.page_id));
        }
        if pa.blank != pb.blank {
            return Comparison::Mismatch(format!("page {} blank in one result only", pa.page_id));
        }
        let d = similarity::dice(&pa.text, &pb.text);
        if d < cfg.min_dice {
            return Comparison::Mismatch(format!("page {} dice {d:.4}", pa.page_id));
        }
        let l = similarity::length_ratio(&pa.text, &pb.text);
        if l < cfg.min_length_ratio {
            return Comparison::Mismatch(format!("page {} length ratio {l:.4}", pa.page_id));
        }
    }
    Comparison::Match
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn good(text_a: &str, text_b: &str) -> ResultFile {
        let cfg = Config::pilot();
        let mk = |id: &str, t: &str| PageResult {
            alto: None,
            blank: false,
            input_md5: "0123456789abcdef0123456789abcdef".into(),
            page_id: id.into(),
            source: None,
            text: t.into(),
            text_sha256: sha256_hex(t.as_bytes()),
        };
        ResultFile {
            engine: Engine {
                name: cfg.engine_name,
                traineddata: cfg.traineddata,
                version: cfg.engine_version,
            },
            pages: vec![mk("p0", text_a), mk("p1", text_b)],
            schema: 1,
            wu: "usnm_ocr_0000000000000000".into(),
        }
    }

    const T0: &str = "Der Kaiser ist heute in Berlin angekommen und wurde empfangen.";
    const T1: &str = "Die Zeitung meldet neue Preise fuer Weizen und Roggen am Markt.";

    #[test]
    fn a_good_result_passes() {
        assert_eq!(check(&good(T0, T1), &Config::pilot()), Ok(()));
    }

    #[test]
    fn parse_round_trips_and_rejects_junk() {
        let r = good(T0, T1);
        let bytes = serde_json::to_vec(&r).unwrap();
        assert_eq!(parse(&bytes).unwrap(), r);
        assert!(matches!(parse(b"nope"), Err(Invalid::Parse(_))));
    }

    #[test]
    fn rejects_wrong_engine_or_models() {
        let cfg = Config::pilot();
        let mut r = good(T0, T1);
        r.engine.version = "5.4.0".into();
        assert!(matches!(check(&r, &cfg), Err(Invalid::Engine(_))));
        let mut r = good(T0, T1);
        r.engine.traineddata.insert("frak2021".into(), "00".into());
        assert!(matches!(check(&r, &cfg), Err(Invalid::Engine(_))));
        let mut r = good(T0, T1);
        r.engine.traineddata.insert("extra".into(), "00".into());
        assert!(matches!(check(&r, &cfg), Err(Invalid::Engine(_))));
    }

    #[test]
    fn rejects_structure_problems() {
        let cfg = Config::pilot();
        let mut r = good(T0, T1);
        r.schema = 2;
        assert_eq!(check(&r, &cfg), Err(Invalid::Schema(2)));
        let mut r = good(T0, T1);
        r.pages.clear();
        assert_eq!(check(&r, &cfg), Err(Invalid::NoPages));
        let mut r = good(T0, T1);
        r.pages[1].page_id = "p0".into();
        assert_eq!(check(&r, &cfg), Err(Invalid::DuplicatePage("p0".into())));
        let mut r = good(T0, T1);
        r.pages[0].input_md5 = "zz".into();
        assert_eq!(check(&r, &cfg), Err(Invalid::BadMd5("p0".into())));
        let mut r = good(T0, T1);
        r.pages[0].text.push('x');
        assert_eq!(check(&r, &cfg), Err(Invalid::TextHash("p0".into())));
    }

    #[test]
    fn empty_text_is_invalid_unless_marked_blank() {
        let cfg = Config::pilot();
        let mut r = good("  ", T1);
        assert_eq!(check(&r, &cfg), Err(Invalid::EmptyText("p0".into())));
        r.pages[0].blank = true;
        assert_eq!(check(&r, &cfg), Ok(()));
    }

    #[test]
    fn identical_results_match() {
        let cfg = Config::pilot();
        assert_eq!(
            compare(&good(T0, T1), &good(T0, T1), &cfg),
            Comparison::Match
        );
    }

    #[test]
    fn small_differences_match_within_threshold() {
        let cfg = Config {
            min_dice: 0.9,
            ..Config::pilot()
        };
        let a = good(T0, T1);
        let b = good(&T0.replace("Berlin", "Berlln"), T1);
        // One word of ten differs: dice is 18/20 = 0.9, exactly the threshold.
        assert_eq!(compare(&a, &b, &cfg), Comparison::Match);
        // The default 0.98 is stricter.
        assert!(matches!(
            compare(&a, &b, &Config::pilot()),
            Comparison::Mismatch(_)
        ));
    }

    #[test]
    fn different_text_inputs_or_pages_mismatch() {
        let cfg = Config::pilot();
        let a = good(T0, T1);
        assert!(matches!(
            compare(&a, &good(T1, T0), &cfg),
            Comparison::Mismatch(_)
        ));
        let mut b = good(T0, T1);
        b.pages[0].input_md5 = "ffffffffffffffffffffffffffffffff".into();
        assert!(matches!(compare(&a, &b, &cfg), Comparison::Mismatch(m) if m.contains("inputs")));
        let mut c = good(T0, T1);
        c.pages[1].page_id = "other".into();
        assert!(matches!(compare(&a, &c, &cfg), Comparison::Mismatch(m) if m.contains("missing")));
        let mut d = good(T0, T1);
        d.pages.pop();
        assert!(matches!(compare(&a, &d, &cfg), Comparison::Mismatch(m) if m.contains("counts")));
    }

    #[test]
    fn a_truncated_page_fails_on_length() {
        let cfg = Config {
            min_dice: 0.5,
            ..Config::pilot()
        };
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon";
        let short =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi";
        let a = good(long, T1);
        let b = good(short, T1);
        assert!(matches!(compare(&a, &b, &cfg), Comparison::Mismatch(m) if m.contains("length")));
    }

    fn with_source(mut r: ResultFile, i: usize, format: &str, md5: &str) -> ResultFile {
        r.pages[i].input_md5 = md5.into();
        r.pages[i].source = Some(SourceInfo {
            bytes: 4_000_000,
            format: format.into(),
            sha256: "ab".repeat(32),
        });
        r
    }

    #[test]
    fn source_info_is_checked() {
        let cfg = Config::pilot();
        let ok = with_source(good(T0, T1), 0, "jp2", "0123456789abcdef0123456789abcdef");
        assert_eq!(check(&ok, &cfg), Ok(()));
        let bad = with_source(good(T0, T1), 0, "tiff", "0123456789abcdef0123456789abcdef");
        assert_eq!(check(&bad, &cfg), Err(Invalid::BadSource("p0".into())));
    }

    #[test]
    fn same_format_replicas_must_agree_on_bytes() {
        let cfg = Config::pilot();
        let a = with_source(good(T0, T1), 0, "jp2", "11111111111111111111111111111111");
        let b = with_source(good(T0, T1), 0, "jp2", "22222222222222222222222222222222");
        assert!(matches!(compare(&a, &b, &cfg), Comparison::Mismatch(_)));
    }

    #[test]
    fn replicas_that_read_different_formats_mismatch() {
        let cfg = Config::pilot();
        let a = with_source(good(T0, T1), 0, "jp2", "11111111111111111111111111111111");
        let b = with_source(good(T0, T1), 0, "jpeg", "11111111111111111111111111111111");
        assert!(matches!(compare(&a, &b, &cfg), Comparison::Mismatch(m) if m.contains("formats")));
    }
}
