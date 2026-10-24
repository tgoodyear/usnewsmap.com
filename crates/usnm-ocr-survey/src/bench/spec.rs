//! The recipe a release is built from: the committed `spec.json` and its annotation records.

use super::manifest::{is_hex64, safe_path, valid_id, valid_release};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    #[serde(default)]
    pub annotations: BTreeMap<String, Annotation>,
    #[serde(default)]
    pub imports: Vec<SpecImport>,
    pub inputs_url: Option<String>,
    pub campaigns: Vec<SpecCampaign>,
    pub derived: Vec<SpecDerived>,
    pub examples: Vec<SpecExample>,
    pub format: String,
    pub limitations: Vec<String>,
    pub release: String,
    pub sources: BTreeMap<String, SpecSource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecCampaign {
    pub campaign_sha256: String,
    pub path: String,
    pub source: String,
    pub subset: String,
}

/// Inventory-pinned, already-curated image/reference pairs, not historical campaigns.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecImport {
    pub path: String,
    pub source: String,
    pub subset: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportSample {
    pub id: String,
    pub image: String,
    pub reference: String,
    pub source_page_id: String,
    pub provenance: Value,
}

/// A subset made from others: `take` samples of `from`, ranked by sha256 of `seed`, a newline and
/// the sample id (so it nests inside `from`), or the `union` of several.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecDerived {
    pub description: String,
    pub id: String,
    pub from: Option<String>,
    pub seed: Option<String>,
    pub take: Option<usize>,
    pub union: Option<Vec<String>>,
}

/// Recorded engine outputs shipped for scorer parity: the build scores them and must get
/// `expect_cer` exactly.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecExample {
    pub campaign: String,
    pub description: String,
    pub expect_cer: f64,
    pub name: String,
    pub report: String,
    pub subset: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpecSource {
    pub annotation_status: String,
    pub attribution: String,
    pub image_license: String,
    pub languages: Vec<String>,
    pub reference_license: String,
    pub scripts: Vec<String>,
    pub unit: String,
}

/// Reviewed sample annotations; absent annotations preserve historical release bytes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub damage: Option<Damage>,
    pub transcription: Option<Transcription>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Damage {
    pub status: String,
    pub labels: Vec<String>,
    pub severity: String,
    pub evidence: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Transcription {
    pub policy: String,
    pub evidence: String,
    pub reviewed_by: String,
    pub illegible_spans: Vec<IllegibleSpan>,
}

/// Half-open Unicode code-point offsets into the unchanged reference file.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IllegibleSpan {
    pub start: usize,
    pub end: usize,
    pub reason: String,
}

pub(super) fn spec_check(spec: &Spec) -> anyhow::Result<()> {
    ensure!(
        spec.format == "usnm-ocr-bench-spec",
        "spec format must be usnm-ocr-bench-spec"
    );
    ensure!(
        valid_release(&spec.release),
        "release must look like v1.2.3"
    );
    for c in &spec.campaigns {
        ensure!(is_hex64(&c.campaign_sha256), "{}: campaign_sha256", c.path);
        ensure!(safe_path(&c.path), "{}: unsafe path", c.path);
        ensure!(valid_id(&c.subset), "{}: bad subset id", c.subset);
        ensure!(
            spec.sources.contains_key(&c.source),
            "{}: unknown source {}",
            c.path,
            c.source
        );
    }
    if let Some(url) = &spec.inputs_url {
        ensure!(url.starts_with("https://"), "inputs_url must be HTTPS");
    }
    for i in &spec.imports {
        ensure!(
            safe_path(&i.path) && valid_id(&i.subset),
            "invalid import path/subset"
        );
        ensure!(
            spec.sources.contains_key(&i.source),
            "unknown import source {}",
            i.source
        );
    }
    for s in spec.sources.keys() {
        ensure!(valid_id(s) && !s.contains('/'), "bad source name {s:?}");
    }
    Ok(())
}

pub(super) fn str_field<'a>(v: &'a Value, k: &str, what: &str) -> anyhow::Result<&'a str> {
    v[k].as_str()
        .with_context(|| format!("{what}: no string {k}"))
}
