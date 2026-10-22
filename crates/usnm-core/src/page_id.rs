//! Source-qualified page identifiers (ADR-0026).
//!
//! A page identifier is `<source>:<local id>`: the source's own identifier, unchanged, under a
//! registered source. A bare id (no `:`) is a LoC `doc_id`, so every stored LoC id stays valid
//! as it is. Only `loc` ids give a [`PageKey`].
//!
//! | Source      | Local id                                                       |
//! | ----------- | -------------------------------------------------------------- |
//! | `loc`       | [`PageKey::doc_id`], e.g. `sn85055101_1917-04-03_ed-1_seq-3`   |
//! | `serialset` | `SERIALSET-{package}_p{seq}` (GovInfo)                         |
//! | `unt`       | `unt-{metadc}_p{seq}`                                          |
//! | `lcg`       | `lcg-{series}-{volume}_p{seq}` (Congress.gov)                  |
//! | `sample`    | `{dataset}/{sample id}`: a dataset sample with no known parent |
//!
//! An identity names a page, never a version or a container: a benchmark release or a gold
//! bundle is the version of an artifact that holds pages, not part of their id.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{PageKey, PageKeyError};

/// The longest local id any source may use.
const MAX_LOCAL: usize = 160;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PageIdError {
    #[error("unknown page source {0:?}")]
    Source(String),
    #[error("invalid {kind} page id {local:?}: {reason}")]
    Local {
        kind: Source,
        local: String,
        reason: String,
    },
}

/// A registered page source. Adding one means a row in ADR-0026 and its rule here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Loc,
    Serialset,
    Unt,
    Lcg,
    Sample,
}

impl Source {
    pub const ALL: [Source; 5] = [
        Source::Loc,
        Source::Serialset,
        Source::Unt,
        Source::Lcg,
        Source::Sample,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Loc => "loc",
            Source::Serialset => "serialset",
            Source::Unt => "unt",
            Source::Lcg => "lcg",
            Source::Sample => "sample",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Source {
    type Err = PageIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Source::ALL
            .into_iter()
            .find(|source| source.as_str() == s)
            .ok_or_else(|| PageIdError::Source(s.to_owned()))
    }
}

/// A page identifier: a source and that source's own id, checked by the source's rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PageId {
    source: Source,
    local: String,
}

impl PageId {
    pub fn new(source: Source, local: &str) -> Result<Self, PageIdError> {
        check_local(source, local).map_err(|reason| PageIdError::Local {
            kind: source,
            local: local.to_owned(),
            reason,
        })?;
        Ok(Self {
            source,
            local: local.to_owned(),
        })
    }

    /// The `loc` id of a LoC page. `PageKey`'s fields are public, so the key is checked
    /// like any other local id.
    pub fn loc(key: &PageKey) -> Result<Self, PageIdError> {
        PageId::new(Source::Loc, &key.doc_id())
    }

    pub fn source(&self) -> Source {
        self.source
    }

    /// The source's own id, unchanged.
    pub fn local(&self) -> &str {
        &self.local
    }

    /// The LoC page key, for a `loc` id only.
    pub fn page_key(&self) -> Option<PageKey> {
        (self.source == Source::Loc)
            .then(|| PageKey::from_doc_id(&self.local).ok())
            .flatten()
    }

    /// The id as stored where only LoC pages live: bare for `loc`, qualified otherwise.
    pub fn stored(&self) -> String {
        match self.source {
            Source::Loc => self.local.clone(),
            _ => self.to_string(),
        }
    }

    /// A relative path for this page's files where sources mix: `<source>/<local id>`.
    /// Every local id is made of safe path segments, so this passes `usnm_store::validate_path`.
    pub fn path(&self) -> String {
        format!("{}/{}", self.source, self.local)
    }
}

impl fmt::Display for PageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.local)
    }
}

impl FromStr for PageId {
    type Err = PageIdError;

    /// `<source>:<local id>`, or a bare LoC `doc_id`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once(':') {
            Some((source, local)) => PageId::new(source.parse()?, local),
            None => PageId::new(Source::Loc, s),
        }
    }
}

impl TryFrom<String> for PageId {
    type Error = PageIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<PageId> for String {
    fn from(value: PageId) -> Self {
        value.to_string()
    }
}

/// Letters, digits, `_` and `-`, as every stored id so far uses.
fn plain(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

/// `{stem}_p{seq}` with a positive page number, the Congress pages' shape.
fn paged<'a>(local: &'a str, prefix: &str) -> Result<&'a str, String> {
    let stem = local
        .strip_prefix(prefix)
        .ok_or_else(|| format!("must start with {prefix}"))?;
    let (stem, seq) = stem
        .rsplit_once("_p")
        .ok_or_else(|| "must end with _p<page>".to_owned())?;
    (!seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit()))
        .then(|| seq.parse::<u32>().ok())
        .flatten()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("bad page number {seq:?}"))?;
    if plain(stem) {
        Ok(stem)
    } else {
        Err("only letters, digits, _ and - before the page".to_owned())
    }
}

/// One path segment as `usnm_store::is_safe_segment` allows it:
/// `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`. Kept in step with that function.
fn safe_segment(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn check_local(source: Source, local: &str) -> Result<(), String> {
    if local.len() > MAX_LOCAL {
        return Err(format!("longer than {MAX_LOCAL} bytes"));
    }
    // Every id must work as `<source>/<local>` in the shared stores.
    if !local.split('/').all(safe_segment) {
        return Err("each /-separated part must be a safe path segment".to_owned());
    }
    match source {
        Source::Loc => {
            let key = PageKey::from_doc_id(local).map_err(|e: PageKeyError| e.to_string())?;
            if key.doc_id() == local {
                Ok(())
            } else {
                Err(format!("not canonical (is {})", key.doc_id()))
            }
        }
        Source::Serialset => paged(local, "SERIALSET-").map(|_| ()),
        Source::Unt => paged(local, "unt-").map(|_| ()),
        Source::Lcg => paged(local, "lcg-").map(|_| ()),
        Source::Sample => {
            let (dataset, sample) = local
                .split_once('/')
                .ok_or_else(|| "must be <dataset>/<sample id>".to_owned())?;
            let dataset_ok = dataset.len() <= 40
                && dataset.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && dataset
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !dataset_ok {
                return Err(format!("bad dataset {dataset:?}"));
            }
            // The segment check above already holds the sample id to the store's rule.
            if sample.contains('/') {
                Err(format!("bad sample id {sample:?}"))
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOC: &str = "sn85055101_1917-04-03_ed-1_seq-3";

    #[test]
    fn a_bare_id_is_loc_and_stays_bare_where_stored() {
        let id: PageId = LOC.parse().unwrap();
        assert_eq!(id.source(), Source::Loc);
        assert_eq!(id.to_string(), format!("loc:{LOC}"));
        assert_eq!(id.stored(), LOC);
        assert_eq!(id.page_key().unwrap().doc_id(), LOC);
        assert_eq!(format!("loc:{LOC}").parse::<PageId>().unwrap(), id);
    }

    #[test]
    fn every_source_round_trips() {
        for s in [
            format!("loc:{LOC}"),
            "serialset:SERIALSET-00033_00_00-009-0042-0000_p1".to_owned(),
            "unt:unt-metadc123456_p12".to_owned(),
            "lcg:lcg-crecb-1925_p3".to_owned(),
            "sample:bln600/pub-bln600-063".to_owned(),
            "sample:overproof/overproof-1".to_owned(),
        ] {
            let id: PageId = s.parse().unwrap_or_else(|e| panic!("{s}: {e}"));
            assert_eq!(id.to_string(), s);
            let again = string_round_trip(&id);
            assert_eq!(again, id);
        }
    }

    fn string_round_trip(id: &PageId) -> PageId {
        let s: String = id.clone().into();
        PageId::try_from(s).unwrap()
    }

    #[test]
    fn only_loc_gives_a_page_key() {
        let id: PageId = "unt:unt-metadc123456_p12".parse().unwrap();
        assert!(id.page_key().is_none());
        assert_eq!(id.stored(), "unt:unt-metadc123456_p12");
        assert_eq!(id.path(), "unt/unt-metadc123456_p12");
    }

    #[test]
    fn non_loc_bare_ids_are_refused_rather_than_misread() {
        // Today's bare Congress and benchmark ids must be qualified first (ADR-0026 §2).
        for bare in [
            "SERIALSET-00033_00_00-009-0042-0000_p1",
            "unt-metadc123456_p12",
            "overproof-1",
            "pub-bln600-063",
        ] {
            assert!(bare.parse::<PageId>().is_err(), "{bare}");
        }
    }

    #[test]
    fn each_source_checks_its_own_shape() {
        for bad in [
            "nope:x",
            "loc:sn85055101_1917-04-03_ed-01_seq-3",
            "loc:SN85055101_1917-04-03_ed-1_seq-3",
            "serialset:unt-metadc1_p1",
            "serialset:SERIALSET-x_p0",
            "unt:unt-x",
            "lcg:lcg-x_pa",
            "sample:pub-bln600-063",
            "sample:BLN600/x",
            "sample:bln600/../x",
            "sample:bln600/a/b",
            "sample:bln600/.hidden",
            "sample:bln600/_x",
            "unt:unt-metadc123456_p+1",
            "loc:",
        ] {
            assert!(bad.parse::<PageId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_sample_id_longer_than_a_segment_is_refused() {
        let long = format!("sample:bln600/{}", "a".repeat(129));
        assert!(long.parse::<PageId>().is_err());
    }

    #[test]
    fn loc_from_a_tampered_key_is_refused() {
        let mut key = PageKey::from_doc_id(LOC).unwrap();
        assert_eq!(PageId::loc(&key).unwrap().stored(), LOC);
        key.seq = 0;
        assert!(PageId::loc(&key).is_err());
    }

    #[test]
    fn paths_never_escape() {
        for s in ["sample:bln600/pub-bln600-063", "lcg:lcg-crecb-1925_p3"] {
            let path = s.parse::<PageId>().unwrap().path();
            assert!(!path.starts_with('/') && !path.contains("..") && !path.contains('\\'));
        }
    }
}
