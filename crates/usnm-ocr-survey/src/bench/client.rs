//! The client side: unpack a release tar, verify it, load it, and select a subset.

use super::manifest::{
    list_files, page_count, read_sums, release_name, safe_path, subset_digest, valid_id,
    valid_release, Manifest, Sample,
};
use super::score::{read_outputs, score, validate_annotations};
use super::{FORMAT, MANIFEST, MAX_MANIFEST, MAX_RELEASE, SCHEMA, SUMS};
use crate::bundle::sha256_hex;
use anyhow::{bail, ensure, Context};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Unpacks a release tar into `dest` (regular files only, no absolute paths or `..`, one top
/// directory named for a release) and returns the release directory.
pub fn unpack(tar: &Path, dest: &Path) -> anyhow::Result<PathBuf> {
    let mut a = tar::Archive::new(std::fs::File::open(tar)?);
    let mut total: u64 = 0;
    let mut top: Option<String> = None;
    for e in a.entries()? {
        let mut e = e?;
        ensure!(
            e.header().entry_type() == tar::EntryType::Regular,
            "release tar may hold only regular files"
        );
        total = total
            .checked_add(e.header().size()?)
            .filter(|t| *t <= MAX_RELEASE)
            .context("release tar unpacks to more than the size limit")?;
        let p = e.path()?.to_path_buf();
        let s = p.to_str().context("non-UTF-8 path")?.to_string();
        ensure!(safe_path(&s), "unsafe path in tar: {s}");
        let first = s.split('/').next().unwrap_or_default().to_string();
        ensure!(
            first.starts_with("usnm-ocr-bench-v") && s.len() > first.len() + 1,
            "unexpected tar entry {s}"
        );
        if top.is_none() {
            ensure!(
                !dest.join(&first).exists(),
                "{} already exists; unpack into a fresh directory",
                dest.join(&first).display()
            );
        }
        ensure!(
            top.get_or_insert_with(|| first.clone()) == &first,
            "release tar has more than one top directory"
        );
        // unpack_in refuses to write outside dest, even through a symlink already there.
        ensure!(e.unpack_in(dest)?, "unsafe path in tar: {s}");
    }
    Ok(dest.join(top.context("empty release tar")?))
}

#[derive(Debug, Serialize)]
pub struct Verified {
    pub content_digest: String,
    pub files: usize,
    pub manifest_sha256: String,
    pub release: String,
    pub samples: usize,
    pub subsets: usize,
}

/// Loads a release's manifest after checking SHA256SUMS against the files.
pub fn load(dir: &Path) -> anyhow::Result<Manifest> {
    verify(dir)?;
    read_manifest(dir)
}

pub(super) fn read_manifest(dir: &Path) -> anyhow::Result<Manifest> {
    let p = dir.join(MANIFEST);
    ensure!(
        std::fs::metadata(&p)?.len() <= MAX_MANIFEST,
        "manifest too large"
    );
    serde_json::from_slice(&std::fs::read(&p)?).context("parsing manifest.json")
}

/// Checks a release directory: every file is in SHA256SUMS with its sha256 and nothing else is
/// there; then the manifest's own rules (ids, checksums, subsets, splits, licences, examples).
pub fn verify(dir: &Path) -> anyhow::Result<Verified> {
    let sums_text = std::fs::read_to_string(dir.join(SUMS)).context("reading SHA256SUMS")?;
    let sums = read_sums(&sums_text)?;
    let mut on_disk = list_files(dir)?;
    ensure!(on_disk.remove(SUMS), "no SHA256SUMS");
    let listed: BTreeSet<String> = sums.keys().cloned().collect();
    if let Some(p) = on_disk.difference(&listed).next() {
        bail!("{p} is not in SHA256SUMS");
    }
    if let Some(p) = listed.difference(&on_disk).next() {
        bail!("{p} is in SHA256SUMS but missing");
    }
    for (p, want) in &sums {
        let got = sha256_hex(&std::fs::read(dir.join(p))?);
        ensure!(&got == want, "{p}: sha256 differs from SHA256SUMS");
    }
    let m = read_manifest(dir)?;
    check_manifest(dir, &m, &sums)?;
    Ok(Verified {
        content_digest: sha256_hex(sums_text.as_bytes()),
        files: sums.len(),
        manifest_sha256: sums[MANIFEST].clone(),
        release: m.release.clone(),
        samples: m.samples.len(),
        subsets: m.subsets.len(),
    })
}

pub(super) fn check_manifest(
    dir: &Path,
    m: &Manifest,
    sums: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    ensure!(m.format == FORMAT, "not a {FORMAT} manifest");
    ensure!(
        (1..=SCHEMA).contains(&m.schema),
        "manifest schema {} (this reads {SCHEMA})",
        m.schema
    );
    ensure!(valid_release(&m.release), "bad release {:?}", m.release);
    if m.schema == 1 {
        ensure!(
            m.samples
                .iter()
                .all(|s| s.transcription.is_none() && s.damage["status"] != "reviewed"),
            "reviewed annotations require manifest schema 2"
        );
    }
    ensure!(
        dir.file_name().and_then(|n| n.to_str()) == Some(release_name(&m.release).as_str()),
        "the directory is not named {}",
        release_name(&m.release)
    );
    let mut samples = BTreeMap::new();
    let mut images = BTreeSet::new();
    let mut page_split: BTreeMap<&str, &str> = BTreeMap::new();
    for s in &m.samples {
        ensure!(valid_id(&s.id), "bad sample id {:?}", s.id);
        ensure!(
            m.sources.contains_key(&s.source),
            "{}: unknown source",
            s.id
        );
        for f in [&s.image, &s.reference] {
            ensure!(
                sums.get(&f.path) == Some(&f.sha256),
                "{}: {} is not in SHA256SUMS with the manifest's sha256",
                s.id,
                f.path
            );
            ensure!(
                std::fs::metadata(dir.join(&f.path))?.len() == f.bytes,
                "{}: {} size",
                s.id,
                f.path
            );
        }
        ensure!(images.insert(&s.image.sha256), "{}: duplicate image", s.id);
        validate_annotations(s, &std::fs::read_to_string(dir.join(&s.reference.path))?)?;
        ensure!(
            !std::fs::read_to_string(dir.join(&s.reference.path))
                .with_context(|| format!("{}: reference is not UTF-8", s.id))?
                .trim()
                .is_empty(),
            "{}: empty reference",
            s.id
        );
        ensure!(
            ["public-eval", "dev", "test", "train"].contains(&s.split.as_str()),
            "{}: unknown split {}",
            s.id,
            s.split
        );
        // Samples from one source page never cross splits.
        if let Some(prev) = page_split.insert(&s.source_page_id, &s.split) {
            ensure!(
                prev == s.split,
                "page {} is in splits {prev} and {}",
                s.source_page_id,
                s.split
            );
        }
        ensure!(
            samples.insert(s.id.clone(), s.clone()).is_none(),
            "duplicate sample {}",
            s.id
        );
    }
    let mut in_subset = BTreeSet::new();
    for (k, s) in &m.subsets {
        ensure!(valid_id(k), "bad subset id {k:?}");
        let mut sorted = s.members.clone();
        sorted.sort();
        sorted.dedup();
        ensure!(sorted == s.members, "{k}: members not sorted and unique");
        for x in &s.members {
            ensure!(samples.contains_key(x), "{k}: unknown member {x}");
            in_subset.insert(x.clone());
        }
        ensure!(s.count == s.members.len(), "{k}: count");
        ensure!(
            s.pages == page_count(&s.members, &samples),
            "{k}: page count"
        );
        ensure!(
            s.digest == subset_digest(&s.members, &samples),
            "{k}: digest"
        );
        if let Some(from) = s.recipe["from"].as_str() {
            let parent = m
                .subsets
                .get(from)
                .with_context(|| format!("{k}: unknown parent {from}"))?;
            let p: BTreeSet<_> = parent.members.iter().collect();
            ensure!(
                s.members.iter().all(|x| p.contains(x)),
                "{k} is not inside {from}"
            );
        }
    }
    ensure!(
        in_subset.len() == samples.len(),
        "some samples are in no subset"
    );
    for src in m.sources.keys() {
        ensure!(
            sums.contains_key(&format!("LICENSES/{src}/NOTICE.txt")),
            "no licence notice for {src}"
        );
    }
    for e in &m.examples {
        ensure!(
            sums.contains_key(&e.hypotheses),
            "example {}: {} is not in SHA256SUMS",
            e.name,
            e.hypotheses
        );
        let outputs = read_outputs(&std::fs::read_to_string(dir.join(&e.hypotheses))?)?;
        let (t, _) = score(dir, m, &e.subset, &outputs, &e.rule)?;
        ensure!(
            t.edits == e.edits
                && t.reference_chars == e.reference_chars
                && t.cer() == Some(e.expect_cer),
            "example {}: scored {} / {}, the manifest records {} / {}",
            e.name,
            t.edits,
            t.reference_chars,
            e.edits,
            e.reference_chars
        );
    }
    Ok(())
}

/// Checks a verified release against its entry in `ocr/bench/releases.json`.
pub fn check_index(index: &Value, v: &Verified, m: &Manifest) -> anyhow::Result<()> {
    let e = index["releases"]
        .as_array()
        .context("releases.json has no releases")?
        .iter()
        .find(|e| e["release"].as_str() == Some(&v.release))
        .with_context(|| format!("releases.json has no {}", v.release))?;
    ensure!(
        e["content_digest"].as_str() == Some(&v.content_digest),
        "content digest differs from releases.json"
    );
    ensure!(
        e["manifest_sha256"].as_str() == Some(&v.manifest_sha256),
        "manifest sha256 differs from releases.json"
    );
    for (k, s) in &m.subsets {
        ensure!(
            e["subsets"][k]["digest"].as_str() == Some(&s.digest),
            "subset {k} differs from releases.json"
        );
    }
    ensure!(
        e["subsets"].as_object().map(|o| o.len()) == Some(m.subsets.len()),
        "releases.json lists other subsets"
    );
    Ok(())
}

/// The members of a subset, one JSON object each, for an engine runner. References are left out
/// unless asked for, so they can't reach a model's prompt by accident.
pub fn select(
    dir: &Path,
    m: &Manifest,
    subset: &str,
    with_references: bool,
) -> anyhow::Result<Vec<Value>> {
    let s = m.subsets.get(subset).with_context(|| {
        format!(
            "no subset {subset:?}; have {}",
            m.subsets.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
    let by_id: BTreeMap<&str, &Sample> = m.samples.iter().map(|x| (x.id.as_str(), x)).collect();
    s.members
        .iter()
        .map(|id| {
            let x = by_id[id.as_str()];
            let mut v = json!({
                "id": x.id,
                "image": dir.join(&x.image.path),
                "image_sha256": x.image.sha256,
                "languages": x.languages,
                "source": x.source,
                "unit": x.unit,
            });
            if with_references {
                v["reference"] = json!(dir.join(&x.reference.path));
                v["reference_sha256"] = json!(x.reference.sha256);
            }
            Ok(v)
        })
        .collect()
}
