//! What built an index version (#161): the code, the search engine, the
//! feature versions and the index templates, so versions can be compared
//! later without working out from merge times what each one had.
//!
//! A release records it in two places: the version's manifest
//! (`{version}/manifest.json` `build`, with the templates in full) and its
//! `index_runs` item (`build`, with the templates' checksums only, so the
//! item stays small). It describes the run that wrote the version's new
//! index. A delta's base was built by an earlier run: follow
//! `previous_version` to that version's record.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::source::hex;

/// The commit the image was built from (`ARG USNM_GIT_SHA` in
/// `Dockerfile.ingest`, set by CI). Absent outside CI builds.
const COMMIT_ENV: &str = "USNM_GIT_SHA";

/// The index templates a release can apply, by name.
fn templates(ja: bool) -> Vec<(&'static str, &'static str)> {
    let mut t = vec![("pages", crate::sink::INDEX_TEMPLATE)];
    if ja {
        t.push(("pages-ja", crate::ocr_ja::JA_TEMPLATE));
    }
    t
}

/// The full record, for the manifest: templates in full.
pub fn record(full: bool, ja: bool, quickwit_bin: Option<&Path>) -> Value {
    let mut v = summary(full, ja, quickwit_bin);
    for (name, yaml) in templates(ja) {
        v["templates"][name]["yaml"] = Value::from(yaml);
    }
    v
}

/// The record without the templates' text, for the `index_runs` item.
pub fn summary(full: bool, ja: bool, quickwit_bin: Option<&Path>) -> Value {
    let mut templates_v = json!({});
    for (name, yaml) in templates(ja) {
        templates_v[name] = json!({ "sha256": hex(&Sha256::digest(yaml.as_bytes())) });
    }
    json!({
        "commit": std::env::var(COMMIT_ENV).ok().filter(|s| !s.is_empty()),
        "ingest": env!("CARGO_PKG_VERSION"),
        "quickwit": quickwit_bin.and_then(quickwit_version),
        "full": full,
        "features": {
            "common_grams": usnm_core::common_grams::VERSION,
            "ja_fold": usnm_core::ja::FOLD_VERSION,
        },
        "templates": templates_v,
    })
}

/// `quickwit --version`'s first line ("Quickwit 0.9.1 (…)"), if it runs.
fn quickwit_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_features_and_templates() {
        let v = record(true, true, None);
        assert_eq!(v["full"], true);
        assert_eq!(
            v["features"]["common_grams"],
            usnm_core::common_grams::VERSION
        );
        assert_eq!(v["features"]["ja_fold"], usnm_core::ja::FOLD_VERSION);
        assert_eq!(v["quickwit"], Value::Null);
        let pages = &v["templates"]["pages"];
        assert_eq!(pages["yaml"], crate::sink::INDEX_TEMPLATE);
        assert_eq!(pages["sha256"].as_str().unwrap().len(), 64);
        assert!(v["templates"]["pages-ja"]["yaml"].is_string());
        // The run item's copy has the checksums only.
        let s = summary(true, false, None);
        assert_eq!(s["templates"]["pages"]["sha256"], pages["sha256"]);
        assert!(s["templates"]["pages"].get("yaml").is_none());
        assert!(s["templates"].get("pages-ja").is_none());
    }

    #[test]
    fn a_quickwit_that_doesnt_run_is_unknown() {
        assert_eq!(quickwit_version(Path::new("/nonexistent/quickwit")), None);
    }
}
