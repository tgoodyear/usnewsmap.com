//! The audit trail of the manual overrides in `catalog/overrides/` (its
//! README): each entry says when it was decided (`added`), why (`reason`),
//! where its values come from (`source`) and where it was decided (`ref`,
//! a GitHub issue or pull request). An entry changed later has `updated`
//! and keeps its earlier versions in `history`, so the file itself is the
//! trail.

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::Value;

/// An earlier version of an override entry: the values it had then (only
/// fields of the entry), and that version's audit fields.
#[derive(Debug, Clone, Deserialize)]
pub struct Earlier {
    pub reason: String,
    #[serde(rename = "ref")]
    pub reference: u32,
    pub source: String,
    /// When this version was made: the entry's `added` for the first one.
    pub updated: String,
    /// The entry's values in this version.
    #[serde(flatten)]
    pub values: BTreeMap<String, Value>,
}

/// An entry's audit fields, borrowed from it.
#[derive(Debug, Clone, Copy)]
pub struct Audit<'a> {
    pub added: &'a str,
    pub history: &'a [Earlier],
    pub reason: &'a str,
    pub reference: u32,
    pub source: &'a str,
    pub updated: Option<&'a str>,
}

/// `YYYY-MM-DD`, a real date.
pub fn date(s: &str) -> anyhow::Result<NaiveDate> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("`{s}` isn't a date (YYYY-MM-DD)"))?;
    ensure!(
        d.format("%Y-%m-%d").to_string() == s,
        "`{s}` isn't a date (YYYY-MM-DD)"
    );
    Ok(d)
}

impl Audit<'_> {
    /// Check the trail; `fields` are the entry's fields an earlier version
    /// may hold.
    pub fn check(&self, fields: &[&str]) -> anyhow::Result<()> {
        let text = |name: &str, v: &str| {
            ensure!(!v.trim().is_empty(), "no `{name}`");
            Ok(())
        };
        let added = date(self.added).context("`added`")?;
        text("reason", self.reason)?;
        text("source", self.source)?;
        ensure!(self.reference > 0, "`ref` isn't an issue or PR number");
        match (self.updated, self.history) {
            (None, []) => return Ok(()),
            (Some(_), []) => bail!("`updated` without `history` (the values before the change)"),
            (None, _) => bail!("`history` without `updated` (when it changed)"),
            (Some(updated), history) => {
                let updated = date(updated).context("`updated`")?;
                let mut last = added;
                for (i, e) in history.iter().enumerate() {
                    let at = || format!("history[{i}]");
                    let when = date(&e.updated).with_context(at)?;
                    ensure!(
                        i > 0 || when == added,
                        "{}: the first version's `updated` isn't `added`",
                        at()
                    );
                    ensure!(when >= last, "{}: out of date order", at());
                    last = when;
                    text("reason", &e.reason).with_context(at)?;
                    text("source", &e.source).with_context(at)?;
                    ensure!(
                        e.reference > 0,
                        "{}: `ref` isn't an issue or PR number",
                        at()
                    );
                    if let Some(k) = e.values.keys().find(|k| !fields.contains(&k.as_str())) {
                        bail!("{}: `{k}` isn't a field it can change", at());
                    }
                }
                ensure!(updated >= last, "`updated` is before its history");
            }
        }
        Ok(())
    }
}

/// The keys of every JSON object in `json`, nested ones included, each in
/// the order the text has them (a parsed map would sort them).
#[cfg(test)]
pub(crate) fn keys_in_order(json: &str) -> Vec<Vec<String>> {
    use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
    struct Walk<'a>(&'a mut Vec<Vec<String>>);
    impl<'de> DeserializeSeed<'de> for Walk<'_> {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Walk<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("JSON")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<(), A::Error> {
            let at = self.0.len();
            self.0.push(Vec::new());
            while let Some(k) = m.next_key::<String>()? {
                self.0[at].push(k);
                m.next_value_seed(Walk(&mut *self.0))?;
            }
            Ok(())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<(), A::Error> {
            while s.next_element_seed(Walk(&mut *self.0))?.is_some() {}
            Ok(())
        }
        fn visit_str<E>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
        fn visit_bool<E>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E>(self) -> Result<(), E> {
            Ok(())
        }
    }
    let mut out = Vec::new();
    let mut de = serde_json::Deserializer::from_str(json);
    Walk(&mut out).deserialize(&mut de).unwrap();
    out
}

/// Every object in `json` has its keys in ascending order.
#[cfg(test)]
pub(crate) fn assert_keys_sorted(json: &str) {
    for keys in keys_in_order(json) {
        let mut s = keys.clone();
        s.sort();
        assert_eq!(keys, s, "keys out of order");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit<'a>(history: &'a [Earlier], updated: Option<&'a str>) -> Audit<'a> {
        Audit {
            added: "2026-10-06",
            history,
            reason: "Why.",
            reference: 190,
            source: "Where from.",
            updated,
        }
    }

    fn earlier(json: &str) -> Earlier {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn dates_are_iso() {
        assert!(date("2026-10-07").is_ok());
        for bad in [
            "2026-1-7",
            "2026-02-30",
            "07/10/2026",
            "2026-10-07T00:00:00Z",
            "",
        ] {
            assert!(date(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_field_is_checked() {
        assert!(audit(&[], None).check(&[]).is_ok());
        for bad in [
            Audit {
                added: "2026-10",
                ..audit(&[], None)
            },
            Audit {
                reason: " ",
                ..audit(&[], None)
            },
            Audit {
                source: "",
                ..audit(&[], None)
            },
            Audit {
                reference: 0,
                ..audit(&[], None)
            },
            audit(&[], Some("2026-10-07")),
        ] {
            assert!(bad.check(&[]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_change_keeps_the_earlier_values() {
        let first = earlier(
            r#"{"lat": 1.0, "lon": 2.0, "reason": "Old why.", "ref": 190, "source": "Old source.", "updated": "2026-10-06"}"#,
        );
        let h = [first.clone()];
        assert!(audit(&h, Some("2026-10-07")).check(&["lat", "lon"]).is_ok());
        // History needs `updated`, and the other way round.
        assert!(audit(&h, None).check(&["lat", "lon"]).is_err());
        // Only the entry's fields.
        assert!(audit(&h, Some("2026-10-07")).check(&["lat"]).is_err());
        // The first version is the one added, and dates run forward.
        assert!(audit(&h, Some("2026-10-05"))
            .check(&["lat", "lon"])
            .is_err());
        let late = Earlier {
            updated: "2026-10-08".into(),
            ..first.clone()
        };
        assert!(audit(&[late], Some("2026-10-09"))
            .check(&["lat", "lon"])
            .is_err());
        let second = Earlier {
            updated: "2026-10-08".into(),
            ..first.clone()
        };
        assert!(audit(&[first.clone(), second.clone()], Some("2026-10-09"))
            .check(&["lat", "lon"])
            .is_ok());
        assert!(audit(&[second, first], Some("2026-10-09"))
            .check(&["lat", "lon"])
            .is_err());
    }

    #[test]
    fn nested_keys_are_read_in_file_order() {
        let json = r#"[{"b": 1, "a": [{"d": "\"c\": no", "c": null}]}, {"e": {}}]"#;
        assert_eq!(
            keys_in_order(json),
            [vec!["b", "a"], vec!["d", "c"], vec!["e"], vec![]]
        );
    }
}
