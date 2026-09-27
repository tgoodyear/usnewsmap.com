//! Page keys and document ids.
//!
//! A page key is `{lccn}/{yyyy-mm-dd}/ed-{n}/seq-{n}`, the same tuple LoC uses
//! for resource paths. The engine document id is the page key with `/`
//! replaced by `_`, which is collision-free and valid as an Azure AI Search key.

use std::fmt;
use std::str::FromStr;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PageKeyError {
    #[error("page key must have 4 '/'-separated parts: {0}")]
    Shape(String),
    #[error("invalid LCCN: {0}")]
    Lccn(String),
    #[error("invalid date: {0}")]
    Date(String),
    #[error("invalid edition or sequence: {0}")]
    Number(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PageKey {
    pub lccn: String,
    pub date: NaiveDate,
    pub edition: u16,
    pub seq: u16,
}

impl PageKey {
    pub fn new(lccn: &str, date: NaiveDate, edition: u16, seq: u16) -> Result<Self, PageKeyError> {
        validate_lccn(lccn)?;
        if edition == 0 || seq == 0 {
            return Err(PageKeyError::Number(format!("ed-{edition}/seq-{seq}")));
        }
        Ok(Self {
            lccn: lccn.to_owned(),
            date,
            edition,
            seq,
        })
    }

    /// Engine document id: the page key with `/` replaced by `_`.
    pub fn doc_id(&self) -> String {
        self.to_string().replace('/', "_")
    }

    /// Parse a document id produced by [`PageKey::doc_id`].
    pub fn from_doc_id(doc_id: &str) -> Result<Self, PageKeyError> {
        doc_id.replacen('_', "/", 3).parse()
    }

    /// Canonical loc.gov page viewer URL, with the search terms highlighted when given.
    pub fn viewer_url(&self, highlight: Option<&str>) -> String {
        let mut url = format!(
            "https://www.loc.gov/resource/{}/{}/ed-{}/?sp={}",
            self.lccn,
            self.date.format("%Y-%m-%d"),
            self.edition,
            self.seq
        );
        if let Some(q) = highlight.filter(|q| !q.is_empty()) {
            url.push_str("&q=");
            url.extend(form_urlencoded::byte_serialize(q.as_bytes()));
        }
        url
    }
}

fn validate_lccn(lccn: &str) -> Result<(), PageKeyError> {
    let ok = !lccn.is_empty()
        && lccn.len() <= 16
        && lccn
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(PageKeyError::Lccn(lccn.to_owned()))
    }
}

fn parse_numbered(part: &str, prefix: &str) -> Result<u16, PageKeyError> {
    part.strip_prefix(prefix)
        .and_then(|n| n.parse::<u16>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| PageKeyError::Number(part.to_owned()))
}

impl FromStr for PageKey {
    type Err = PageKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('/').collect();
        let [lccn, date, ed, seq] = parts.as_slice() else {
            return Err(PageKeyError::Shape(s.to_owned()));
        };
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|_| PageKeyError::Date((*date).to_owned()))?;
        PageKey::new(
            lccn,
            date,
            parse_numbered(ed, "ed-")?,
            parse_numbered(seq, "seq-")?,
        )
    }
}

impl fmt::Display for PageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/ed-{}/seq-{}",
            self.lccn,
            self.date.format("%Y-%m-%d"),
            self.edition,
            self.seq
        )
    }
}

impl TryFrom<String> for PageKey {
    type Error = PageKeyError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<PageKey> for String {
    fn from(value: PageKey) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_page_key_and_doc_id() {
        let key: PageKey = "sn84026749/1896-07-10/ed-1/seq-12".parse().unwrap();
        assert_eq!(key.lccn, "sn84026749");
        assert_eq!(key.seq, 12);
        assert_eq!(key.to_string(), "sn84026749/1896-07-10/ed-1/seq-12");
        let id = key.doc_id();
        assert_eq!(id, "sn84026749_1896-07-10_ed-1_seq-12");
        assert_eq!(PageKey::from_doc_id(&id).unwrap(), key);
    }

    #[test]
    fn rejects_malformed_keys() {
        assert!(matches!(
            "sn1/1896-07-10/ed-1".parse::<PageKey>(),
            Err(PageKeyError::Shape(_))
        ));
        assert!(matches!(
            "SN1/1896-07-10/ed-1/seq-1".parse::<PageKey>(),
            Err(PageKeyError::Lccn(_))
        ));
        assert!(matches!(
            "sn1/1896-13-10/ed-1/seq-1".parse::<PageKey>(),
            Err(PageKeyError::Date(_))
        ));
        assert!(matches!(
            "sn1/1896-07-10/ed-0/seq-1".parse::<PageKey>(),
            Err(PageKeyError::Number(_))
        ));
    }

    #[test]
    fn builds_viewer_url_with_highlight() {
        let key: PageKey = "sn84031492/1896-07-10/ed-1/seq-1".parse().unwrap();
        assert_eq!(
            key.viewer_url(Some("cross of gold")),
            "https://www.loc.gov/resource/sn84031492/1896-07-10/ed-1/?sp=1&q=cross+of+gold"
        );
        assert_eq!(
            key.viewer_url(None),
            "https://www.loc.gov/resource/sn84031492/1896-07-10/ed-1/?sp=1"
        );
    }
}
