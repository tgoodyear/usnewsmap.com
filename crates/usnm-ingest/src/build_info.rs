//! What built an index version (#161): the code, the search engine, the
//! feature versions and the index templates, so versions can be compared
//! later without working out from merge times what each one had.
//!
//! A release records it in two places: the version's manifest
//! (`{version}/manifest.json` `build`, with the templates in full) and its
//! `index_runs` item (`build`, with the templates' checksums only, so the
//! item stays small). It describes what this run built: the templates it
//! applied are those of the indexes it wrote. A delta's base, and the main
//! indexes an overlay-only release keeps, were built by earlier versions:
//! follow `previous_version` to their records.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::source::hex;

/// The commit the image was built from (`ARG USNM_GIT_SHA` in
/// `Dockerfile.ingest`, set by CI). Absent outside CI builds.
const COMMIT_ENV: &str = "USNM_GIT_SHA";

/// What a run builds: which indexes it writes, on which engine.
#[derive(Debug, Clone, Default)]
pub struct Built {
    pub full: bool,
    /// It writes a main index (`pages-…`): every release but an overlay-only one.
    pub main_index: bool,
    /// It writes a Japanese index (`pages-ja-…`).
    pub ja_index: bool,
    /// The engine, from the sink (`IndexSink::engine`).
    pub engine: Option<String>,
}

/// The index templates the run applies, by name.
fn templates(b: &Built) -> Vec<(&'static str, &'static str)> {
    let mut t = Vec::new();
    if b.main_index {
        t.push(("pages", crate::sink::INDEX_TEMPLATE));
    }
    if b.ja_index {
        t.push(("pages-ja", crate::ocr_ja::JA_TEMPLATE));
    }
    t
}

/// The full record, for the manifest: templates in full.
pub fn record(b: &Built) -> Value {
    let mut v = summary(b);
    for (name, yaml) in templates(b) {
        v["templates"][name]["yaml"] = Value::from(yaml);
    }
    v
}

/// The record without the templates' text, for the `index_runs` item.
pub fn summary(b: &Built) -> Value {
    let mut templates_v = json!({});
    for (name, yaml) in templates(b) {
        templates_v[name] = json!({ "sha256": hex(&Sha256::digest(yaml.as_bytes())) });
    }
    json!({
        "commit": std::env::var(COMMIT_ENV).ok().filter(|s| !s.is_empty()),
        "ingest": env!("CARGO_PKG_VERSION"),
        "engine": b.engine,
        "full": b.full,
        "features": {
            "common_grams": usnm_core::common_grams::VERSION,
            "ja_fold": usnm_core::ja::FOLD_VERSION,
        },
        "templates": templates_v,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(main_index: bool, ja_index: bool) -> Built {
        Built {
            full: true,
            main_index,
            ja_index,
            engine: Some("Quickwit 0.9.1".into()),
        }
    }

    #[test]
    fn records_features_and_the_templates_applied() {
        let v = record(&built(true, true));
        assert_eq!(
            (v["full"].clone(), v["engine"].clone()),
            (json!(true), json!("Quickwit 0.9.1"))
        );
        assert_eq!(
            v["features"]["common_grams"],
            usnm_core::common_grams::VERSION
        );
        assert_eq!(v["features"]["ja_fold"], usnm_core::ja::FOLD_VERSION);
        let pages = &v["templates"]["pages"];
        assert_eq!(pages["yaml"], crate::sink::INDEX_TEMPLATE);
        assert_eq!(pages["sha256"].as_str().unwrap().len(), 64);
        assert!(v["templates"]["pages-ja"]["yaml"].is_string());
        // The run item's copy has the checksums only.
        let s = summary(&built(true, false));
        assert_eq!(s["templates"]["pages"]["sha256"], pages["sha256"]);
        assert!(s["templates"]["pages"].get("yaml").is_none());
        assert!(s["templates"].get("pages-ja").is_none());
    }

    #[test]
    fn an_overlay_only_release_applies_only_the_japanese_template() {
        let v = record(&built(false, true));
        assert!(v["templates"].get("pages").is_none());
        assert!(v["templates"]["pages-ja"]["sha256"].is_string());
    }
}
