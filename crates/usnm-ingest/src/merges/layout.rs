//! An index's published splits, and the checks on the layout a merge leaves.

use std::collections::BTreeMap;

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::Value;

use super::max_partitioned_splits;

/// One published split.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub id: String,
    pub docs: u64,
    pub bytes: u64,
    /// The size of its footer (the hotcache and the file list), which a
    /// searcher reads first and keeps in `split_footer_cache_capacity`;
    /// `None` if the metastore doesn't say.
    pub footer_bytes: Option<u64>,
    /// Quickwit's partition: 0 unless the index has a `partition_key`
    /// (05 §5.5.5).
    pub partition_id: u64,
    /// The decades its pages are in, from its `decade:` tags; empty for an
    /// index without them.
    pub decades: Vec<u16>,
    /// Quickwit still merges it (`maturity.type` is `immature`): what is
    /// left to merge, which the merge wait's lines follow ([`Burndown`](super::Burndown)).
    pub immature: bool,
}

/// An index's published splits, as the release log and manifest report them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexLayout {
    pub index_id: String,
    pub splits: u64,
    pub docs: u64,
    /// The split files' total size.
    pub bytes: u64,
    pub smallest_split_docs: u64,
    pub largest_split_docs: u64,
    /// The splits' footers in all: what a searcher's split footer cache needs
    /// to hold them all (#125). Absent when a split doesn't give its footer,
    /// and in manifests written before releases recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer_bytes: Option<u64>,
    /// The largest single footer; absent like `footer_bytes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub largest_footer_bytes: Option<u64>,
    /// Splits per decade, for an index partitioned by decade (05 §5.5.5),
    /// where every split holds one; absent otherwise.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub splits_by_decade: BTreeMap<u16, u64>,
}

impl IndexLayout {
    pub fn of(index_id: &str, splits: &[Split]) -> Self {
        let footers: Option<Vec<u64>> = splits.iter().map(|s| s.footer_bytes).collect();
        Self {
            index_id: index_id.to_owned(),
            splits: splits.len() as u64,
            docs: splits.iter().map(|s| s.docs).sum(),
            bytes: splits.iter().map(|s| s.bytes).sum(),
            smallest_split_docs: splits.iter().map(|s| s.docs).min().unwrap_or(0),
            largest_split_docs: splits.iter().map(|s| s.docs).max().unwrap_or(0),
            footer_bytes: footers.as_ref().map(|f| f.iter().sum()),
            largest_footer_bytes: footers
                .as_ref()
                .map(|f| f.iter().copied().max().unwrap_or(0)),
            // Only a partitioned index (a partition key, so nonzero
            // partition ids): a tagged delta's split can also hold one decade.
            splits_by_decade: if splits
                .iter()
                .all(|s| s.partition_id != 0 && s.decades.len() == 1)
            {
                let mut by = BTreeMap::new();
                for s in splits {
                    *by.entry(s.decades[0]).or_default() += 1;
                }
                by
            } else {
                BTreeMap::new()
            },
        }
    }

    /// One "index layout" line.
    pub fn log(&self) {
        tracing::info!(
            index = %self.index_id,
            splits = self.splits,
            docs = self.docs,
            mb = (self.bytes as f64 / (1024.0 * 1024.0)).round() as u64,
            smallest_split_docs = self.smallest_split_docs,
            largest_split_docs = self.largest_split_docs,
            footer_bytes = self.footer_bytes,
            largest_footer_bytes = self.largest_footer_bytes,
            splits_by_decade = ?self.splits_by_decade,
            "index layout"
        );
    }
}

/// The splits in one page of `GET /api/v1/indexes/{id}/splits`.
pub(super) fn parse_splits(v: &Value) -> anyhow::Result<Vec<Split>> {
    v["splits"]
        .as_array()
        .context("the split list has no `splits`")?
        .iter()
        .map(|s| {
            Ok(Split {
                id: s["split_id"]
                    .as_str()
                    .context("a split has no id")?
                    .to_owned(),
                docs: s["num_docs"].as_u64().context("a split has no num_docs")?,
                // The footer ends the split file.
                bytes: s["footer_offsets"]["end"].as_u64().unwrap_or(0),
                footer_bytes: footer_bytes(&s["footer_offsets"]),
                partition_id: s["partition_id"].as_u64().unwrap_or(0),
                decades: decade_tags(&s["tags"]),
                immature: s["maturity"]["type"] == "immature",
            })
        })
        .collect()
}

/// The decades in a split's tags (`decade:1890`), sorted.
fn decade_tags(tags: &Value) -> Vec<u16> {
    let mut decades: Vec<u16> = tags
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str()?.strip_prefix("decade:")?.parse().ok())
        .collect();
    decades.sort_unstable();
    decades.dedup();
    decades
}

/// A split's footer size from its `footer_offsets` (`{"start", "end"}`,
/// byte offsets in the split file).
fn footer_bytes(offsets: &Value) -> Option<u64> {
    offsets["end"]
        .as_u64()?
        .checked_sub(offsets["start"].as_u64()?)
}

/// The merged index holds every document, in no more splits than the
/// merge policy leaves at its split target.
pub(super) fn check(
    index_id: &str,
    expected: u64,
    split_target: u64,
    splits: &[Split],
) -> anyhow::Result<IndexLayout> {
    let layout = IndexLayout::of(index_id, splits);
    if layout.docs != expected {
        bail!(
            "after merging, `{index_id}` holds {} documents in {} splits, expected {expected}",
            layout.docs,
            layout.splits
        );
    }
    let mut partitions: BTreeMap<u64, u64> = BTreeMap::new();
    for s in splits {
        *partitions.entry(s.partition_id).or_default() += s.docs;
    }
    let ceiling = max_partitioned_splits(partitions.values().copied(), split_target);
    if layout.splits > ceiling {
        let across = match partitions.len() {
            0 | 1 => String::new(),
            n => format!(" in {n} partitions"),
        };
        bail!(
            "merging left `{index_id}` in {} splits; its merge policy leaves at most {ceiling} \
             for {expected} documents{across}. Not publishing a half-merged index",
            layout.splits
        );
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merges::max_splits;

    /// The template's split target.
    const T: u64 = 30_000;

    #[test]
    fn layout_and_checks() {
        let splits = vec![
            Split {
                id: "a".into(),
                docs: 1_000_000,
                bytes: 23_000_000_000,
                footer_bytes: Some(3_500_000),
                partition_id: 0,
                decades: vec![],
                immature: false,
            },
            Split {
                id: "b".into(),
                docs: 40_000,
                bytes: 920_000_000,
                footer_bytes: Some(92_377),
                partition_id: 0,
                decades: vec![],
                immature: false,
            },
        ];
        let l = check("idx", 1_040_000, T, &splits).unwrap();
        assert_eq!(
            l,
            IndexLayout {
                index_id: "idx".into(),
                splits: 2,
                docs: 1_040_000,
                bytes: 23_920_000_000,
                smallest_split_docs: 40_000,
                largest_split_docs: 1_000_000,
                footer_bytes: Some(3_592_377),
                largest_footer_bytes: Some(3_500_000),
                splits_by_decade: BTreeMap::new(),
            }
        );
        // In the log line and the manifest.
        assert_eq!(
            serde_json::to_value(&l).unwrap(),
            serde_json::json!({
                "index_id": "idx", "splits": 2, "docs": 1_040_000, "bytes": 23_920_000_000_u64,
                "smallest_split_docs": 40_000, "largest_split_docs": 1_000_000,
                "footer_bytes": 3_592_377, "largest_footer_bytes": 3_500_000,
            })
        );
        // A split that doesn't give its footer: no footer total, rather than
        // one too small. The manifest leaves the fields out.
        let mut unknown = splits.clone();
        unknown[1].footer_bytes = None;
        let l = IndexLayout::of("idx", &unknown);
        assert_eq!((l.footer_bytes, l.largest_footer_bytes), (None, None));
        let v = serde_json::to_value(&l).unwrap();
        assert!(v.get("footer_bytes").is_none() && v.get("largest_footer_bytes").is_none());
        let empty = IndexLayout::of("empty", &[]);
        assert_eq!(
            (empty.footer_bytes, empty.largest_footer_bytes),
            (Some(0), Some(0))
        );
        let err = check("idx", 1_040_001, T, &splits).unwrap_err().to_string();
        assert!(err.contains("expected 1040001"), "{err}");

        let many: Vec<Split> = (0..20)
            .map(|i| Split {
                id: format!("s{i:02}"),
                docs: 7_000,
                bytes: 1,
                footer_bytes: None,
                partition_id: 0,
                decades: vec![],
                immature: false,
            })
            .collect();
        let err = check("idx", 140_000, T, &many).unwrap_err().to_string();
        assert!(err.contains("at most 11"), "{err}");
        assert_eq!(IndexLayout::of("empty", &[]).splits, 0);
        // An index is held to the target it was created with (`--split-docs`):
        // 12 splits of 35,000 pages are mature at 30,000, so they pass, but at
        // 120,000 they should still have merged, so they fail.
        let mid: Vec<Split> = (0..12)
            .map(|i| Split {
                docs: 35_000,
                ..many[i].clone()
            })
            .collect();
        assert!(check("idx", 420_000, T, &mid).is_ok());
        let err = check("idx", 420_000, 120_000, &mid)
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most 10"), "{err}");

        // Partitioned by decade (05 §5.5.5): each partition merges on its
        // own, so each may keep its own small splits.
        let decades: Vec<Split> = (0..20)
            .map(|i| Split {
                partition_id: 100 + i / 5,
                decades: vec![1860 + 10 * (i / 5) as u16],
                ..many[i as usize].clone()
            })
            .collect();
        let l = check("idx", 140_000, T, &decades).unwrap();
        assert_eq!(
            l.splits_by_decade,
            BTreeMap::from([(1860, 5), (1870, 5), (1880, 5), (1890, 5)])
        );
        assert_eq!(
            serde_json::to_value(&l).unwrap()["splits_by_decade"],
            serde_json::json!({"1860": 5, "1870": 5, "1880": 5, "1890": 5})
        );
        // 4 partitions of 35,000 pages: at most 4 × (1 + 5 + 2).
        assert_eq!(max_partitioned_splits([35_000; 4], T), 32);
        assert_eq!(max_partitioned_splits([140_000], T), max_splits(140_000, T));
        let mut crowded = decades.clone();
        for s in &mut crowded {
            s.partition_id = 7;
        }
        let err = check("idx", 140_000, T, &crowded).unwrap_err().to_string();
        assert!(
            err.contains("at most 11") && !err.contains("partitions"),
            "{err}"
        );
        let tiny: Vec<Split> = (0..40)
            .map(|i| Split {
                docs: 1,
                partition_id: i / 20,
                ..decades[0].clone()
            })
            .collect();
        let err = check("idx", 40, T, &tiny).unwrap_err().to_string();
        assert!(
            err.contains("at most 14") && err.contains("in 2 partitions"),
            "{err}"
        );
        // A split with several decades (a tagged delta) or none: no count.
        let mut mixed = decades.clone();
        mixed[0].decades = vec![1860, 1870];
        assert!(IndexLayout::of("idx", &mixed).splits_by_decade.is_empty());
        // A tagged delta whose splits each hold one decade isn't partitioned.
        let tagged: Vec<Split> = decades
            .iter()
            .map(|s| Split {
                partition_id: 0,
                ..s.clone()
            })
            .collect();
        assert!(IndexLayout::of("idx", &tagged).splits_by_decade.is_empty());
    }

    #[test]
    fn parses_a_split_list() {
        let v = serde_json::json!({"offset": 0, "size": 1, "splits": [{
            "split_state": "Published", "split_id": "01M3XPHH", "num_docs": 40000,
            "footer_offsets": {"start": 919982920, "end": 920075297}, "num_merge_ops": 1,
        }]});
        let tagged = serde_json::json!({"splits": [{
            "split_id": "t", "num_docs": 1, "partition_id": 3264326757911759461_u64,
            "tags": ["decade!", "decade:1900", "decade:1890", "lccn:x"],
        }]});
        let t = &parse_splits(&tagged).unwrap()[0];
        assert_eq!(
            (t.partition_id, t.decades.clone()),
            (3264326757911759461, vec![1890, 1900])
        );
        assert_eq!(
            parse_splits(&v).unwrap(),
            vec![Split {
                id: "01M3XPHH".into(),
                docs: 40000,
                bytes: 920_075_297,
                footer_bytes: Some(92_377),
                partition_id: 0,
                decades: vec![],
                immature: false,
            }]
        );
        assert!(parse_splits(&serde_json::json!({"message": "index `x` not found"})).is_err());
        // Without offsets, or with ones that make no sense: no footer size.
        let v = serde_json::json!({"splits": [
            {"split_id": "a", "num_docs": 1},
            {"split_id": "b", "num_docs": 1, "footer_offsets": {"end": 10}},
            {"split_id": "c", "num_docs": 1, "footer_offsets": {"start": 11, "end": 10}},
        ]});
        assert!(parse_splits(&v)
            .unwrap()
            .iter()
            .all(|s| s.footer_bytes.is_none()));
    }
}
