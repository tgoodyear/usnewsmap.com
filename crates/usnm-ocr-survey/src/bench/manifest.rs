//! The manifest a release carries, and the helpers the build and the client share.

use super::spec::{SpecSource, Transcription};
use crate::bundle::sha256_hex;
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    pub bytes: u64,
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    /// Damage annotation; v1 has none, so every sample is `{"status": "unannotated"}`.
    pub damage: Value,
    pub id: String,
    pub image: FileRef,
    pub image_height: u32,
    pub image_width: u32,
    pub languages: Vec<String>,
    /// Where the sample was first used: campaign path and sha256, job id and sha256.
    pub legacy: Value,
    pub mix: String,
    /// The campaign's provenance record, unchanged.
    pub provenance: Value,
    pub reference: FileRef,
    pub reference_status: String,
    pub scripts: Vec<String>,
    pub source: String,
    pub source_page_id: String,
    pub split: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription: Option<Transcription>,
    pub typeface: String,
    pub unit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Subset {
    pub count: usize,
    pub description: String,
    /// sha256 of one line per member, `id image-sha256 reference-sha256`, in id order.
    pub digest: String,
    pub members: Vec<String>,
    pub pages: usize,
    /// How it was made: `{"campaign": path}`, `{"from", "seed", "take"}` or `{"union": [...]}`.
    pub recipe: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Example {
    pub description: String,
    pub edits: u64,
    pub expect_cer: f64,
    pub hypotheses: String,
    pub name: String,
    pub reference_chars: u64,
    pub rule: String,
    pub subset: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub coverage: Value,
    pub examples: Vec<Example>,
    pub format: String,
    pub inputs_sha256: String,
    pub limitations: Vec<String>,
    pub release: String,
    pub rules: Vec<String>,
    pub samples: Vec<Sample>,
    pub schema: u32,
    pub sources: BTreeMap<String, SpecSource>,
    pub spec_sha256: String,
    pub subsets: BTreeMap<String, Subset>,
}

pub(super) fn pretty(v: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    Ok(b)
}

pub(super) fn safe_path(s: &str) -> bool {
    !s.is_empty()
        && Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

pub(super) fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_/.".contains(&b))
        && safe_path(s)
}

pub(super) fn valid_release(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<_> = rest.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

pub(super) fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The release's directory and archive name, `usnm-ocr-bench-<release>`.
pub fn release_name(release: &str) -> String {
    format!("usnm-ocr-bench-{release}")
}

pub(super) fn subset_digest(members: &[String], samples: &BTreeMap<String, Sample>) -> String {
    let mut s = String::new();
    for m in members {
        let x = &samples[m];
        s.push_str(&format!(
            "{} {} {}\n",
            x.id, x.image.sha256, x.reference.sha256
        ));
    }
    sha256_hex(s.as_bytes())
}

pub(super) fn page_count(members: &[String], samples: &BTreeMap<String, Sample>) -> usize {
    members
        .iter()
        .map(|m| samples[m].source_page_id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

/// Parses `sha256sum` output: `HASH  PATH` per line, paths unique.
pub fn read_sums(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let (hash, path) = line
            .split_once("  ")
            .with_context(|| format!("checksum line {}: want `HASH  PATH`", n + 1))?;
        ensure!(is_hex64(hash), "checksum line {}: bad sha256", n + 1);
        ensure!(
            safe_path(path),
            "checksum line {}: unsafe path {path:?}",
            n + 1
        );
        ensure!(
            out.insert(path.to_string(), hash.to_string()).is_none(),
            "checksum line {}: {path} listed twice",
            n + 1
        );
    }
    Ok(out)
}

pub(super) fn write_sums(sums: &BTreeMap<String, String>) -> String {
    sums.iter().map(|(p, h)| format!("{h}  {p}\n")).collect()
}

pub(super) fn list_files(dir: &Path) -> anyhow::Result<BTreeSet<String>> {
    fn walk(root: &Path, d: &Path, out: &mut BTreeSet<String>) -> anyhow::Result<()> {
        for e in std::fs::read_dir(d)? {
            let e = e?;
            let t = e.file_type()?;
            let p = e.path();
            if t.is_dir() {
                walk(root, &p, out)?;
            } else {
                ensure!(t.is_file(), "{}: not a regular file", p.display());
                let rel = p.strip_prefix(root)?.to_str().context("non-UTF-8 path")?;
                out.insert(rel.replace('\\', "/"));
            }
        }
        Ok(())
    }
    let mut out = BTreeSet::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}
