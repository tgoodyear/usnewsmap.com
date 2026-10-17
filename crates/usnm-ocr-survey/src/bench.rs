//! The OCR benchmark release (#738): one versioned, checksummed archive of images and reference
//! transcriptions that any engine can be scored on, with named subsets that resolve to the same
//! bytes. docs/ocr-bench.md has the contract.
//!
//! A release is built from a committed recipe, `ocr/bench/<release>/spec.json`, and a committed
//! inventory of its inputs, `ocr/bench/<release>/inputs.sha256` (`sha256sum` format). Every input
//! file is checked against the inventory before anything is read from it, and the build is
//! reproducible: the same inputs give the same bytes, so a second build can be compared with the
//! published archive. v1 takes its inputs from the immutable Iris campaign roots (#703), so the
//! samples are the exact image bytes and references those campaigns were scored on.
//!
//! The release directory holds `images/`, `references/`, `examples/`, `LICENSES/<source>/`,
//! `RELEASE-NOTES.md`, `manifest.json` and `SHA256SUMS`. `SHA256SUMS` lists every other file,
//! the manifest included; its sha256 is the release's content digest, the one value to cite.

use crate::bundle::sha256_hex;
use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// The manifest's `format`.
pub const FORMAT: &str = "usnm-ocr-bench";
/// The manifest schema version this code reads and writes.
pub const SCHEMA: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
pub const SUMS: &str = "SHA256SUMS";
/// The largest release a client unpacks.
pub const MAX_RELEASE: u64 = 4 << 30;
/// The largest manifest a client reads.
pub const MAX_MANIFEST: u64 = 64 << 20;

/// The score rules. `legacy-ws-v1` is the Iris pilot's: whitespace runs collapsed to one space
/// and trimmed, as Python's `" ".join(s.split())`, then character (code point) edit distance.
/// `raw-v1` compares the texts unchanged.
pub const RULES: [&str; 2] = ["legacy-ws-v1", "raw-v1"];

// ------------------------------------------------------------------ spec

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
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

// ------------------------------------------------------------------ manifest

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

fn pretty(v: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    Ok(b)
}

fn safe_path(s: &str) -> bool {
    !s.is_empty()
        && Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_/.".contains(&b))
        && safe_path(s)
}

fn valid_release(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<_> = rest.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The release's directory and archive name, `usnm-ocr-bench-<release>`.
pub fn release_name(release: &str) -> String {
    format!("usnm-ocr-bench-{release}")
}

fn subset_digest(members: &[String], samples: &BTreeMap<String, Sample>) -> String {
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

fn page_count(members: &[String], samples: &BTreeMap<String, Sample>) -> usize {
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

fn write_sums(sums: &BTreeMap<String, String>) -> String {
    sums.iter().map(|(p, h)| format!("{h}  {p}\n")).collect()
}

// ------------------------------------------------------------------ scoring

/// Python's `str.isspace`: Unicode White_Space plus the four ASCII information separators.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Applies a score rule to a text.
pub fn normalize(rule: &str, text: &str) -> anyhow::Result<String> {
    match rule {
        "legacy-ws-v1" => Ok(text
            .split(py_space)
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")),
        "raw-v1" => Ok(text.to_string()),
        _ => bail!("unknown score rule {rule:?}; known: {}", RULES.join(", ")),
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Tally {
    /// Character edits over the samples with an output.
    pub edits: u64,
    /// Samples with no output, or an output that isn't text.
    pub missing: usize,
    /// Reference characters over the samples with an output.
    pub reference_chars: u64,
    /// Reference characters of the missing samples (scored as empty in `cer_missing_as_empty`).
    pub missing_chars: u64,
    pub samples: usize,
    pub scored: usize,
}

impl Tally {
    fn cer(&self) -> Option<f64> {
        (self.reference_chars > 0).then(|| self.edits as f64 / self.reference_chars as f64)
    }
    fn cer_missing_as_empty(&self) -> Option<f64> {
        let chars = self.reference_chars + self.missing_chars;
        (chars > 0).then(|| (self.edits + self.missing_chars) as f64 / chars as f64)
    }
    fn json(&self) -> Value {
        json!({
            "cer": self.cer(),
            "cer_missing_as_empty": self.cer_missing_as_empty(),
            "edits": self.edits,
            "missing": self.missing,
            "missing_chars": self.missing_chars,
            "reference_chars": self.reference_chars,
            "samples": self.samples,
            "scored": self.scored,
        })
    }
}

/// Scores outputs `{id: text}` on a subset. Missing outputs are counted, never silently left out:
/// `cer` is over the samples with an output and `cer_missing_as_empty` counts the rest as empty.
/// Outputs for ids outside the subset are an error.
pub fn score(
    dir: &Path,
    m: &Manifest,
    subset: &str,
    outputs: &BTreeMap<String, String>,
    rule: &str,
) -> anyhow::Result<(Tally, Value)> {
    let s = m
        .subsets
        .get(subset)
        .with_context(|| format!("no subset {subset:?} in release {}", m.release))?;
    let members: BTreeSet<&str> = s.members.iter().map(String::as_str).collect();
    if let Some(stray) = outputs.keys().find(|k| !members.contains(k.as_str())) {
        bail!("output for {stray:?}, which is not in {subset}");
    }
    let by_id: BTreeMap<&str, &Sample> = m.samples.iter().map(|x| (x.id.as_str(), x)).collect();
    let mut total = Tally::default();
    let mut by_source: BTreeMap<String, Tally> = BTreeMap::new();
    let mut rows = Vec::new();
    for id in &s.members {
        let x = by_id[id.as_str()];
        let reference = std::fs::read_to_string(dir.join(&x.reference.path))?;
        let truth: Vec<char> = normalize(rule, &reference)?.chars().collect();
        let t = by_source.entry(x.source.clone()).or_default();
        for t in [&mut total, t] {
            t.samples += 1;
        }
        match outputs.get(id) {
            Some(text) => {
                let hyp: Vec<char> = normalize(rule, text)?.chars().collect();
                let e = crate::distance::levenshtein(&truth, &hyp) as u64;
                for t in [&mut total, by_source.get_mut(&x.source).unwrap()] {
                    t.scored += 1;
                    t.edits += e;
                    t.reference_chars += truth.len() as u64;
                }
                rows.push(json!({"edits": e, "id": id, "reference_chars": truth.len()}));
            }
            None => {
                for t in [&mut total, by_source.get_mut(&x.source).unwrap()] {
                    t.missing += 1;
                    t.missing_chars += truth.len() as u64;
                }
                rows.push(json!({"id": id, "missing": true, "reference_chars": truth.len()}));
            }
        }
    }
    let report = json!({
        "by_source": by_source.iter().map(|(k, v)| (k.clone(), v.json())).collect::<serde_json::Map<_, _>>(),
        "release": m.release,
        "rows": rows,
        "rule": rule,
        "subset": subset,
        "subset_digest": s.digest,
        "total": total.json(),
    });
    Ok((total, report))
}

/// Reads outputs as JSONL rows `{"id", "text"}`; a duplicate id is an error.
pub fn read_outputs(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(line).with_context(|| format!("outputs line {}", n + 1))?;
        let id = v["id"]
            .as_str()
            .with_context(|| format!("outputs line {}: no string id", n + 1))?;
        let text = v["text"]
            .as_str()
            .with_context(|| format!("outputs line {}: no string text", n + 1))?;
        ensure!(
            out.insert(id.to_string(), text.to_string()).is_none(),
            "outputs line {}: {id} given twice",
            n + 1
        );
    }
    Ok(out)
}

// ------------------------------------------------------------------ build

pub struct BuildOptions {
    pub spec: PathBuf,
    pub inventory: PathBuf,
    pub inputs: PathBuf,
    /// The directory the release directory and archive are written into.
    pub out: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Built {
    pub archive: PathBuf,
    pub dir: PathBuf,
    /// The entry for `ocr/bench/releases.json`.
    pub index: Value,
}

/// Reads an input through the inventory: the file must be listed and match its sha256.
struct Inputs {
    root: PathBuf,
    sums: BTreeMap<String, String>,
    used: BTreeSet<String>,
}

impl Inputs {
    fn read(&mut self, path: &str) -> anyhow::Result<Vec<u8>> {
        let want = self
            .sums
            .get(path)
            .with_context(|| format!("{path} is not in the input inventory"))?;
        let data =
            std::fs::read(self.root.join(path)).with_context(|| format!("reading input {path}"))?;
        ensure!(
            &sha256_hex(&data) == want,
            "{path}: sha256 differs from the input inventory"
        );
        self.used.insert(path.to_string());
        Ok(data)
    }
}

fn spec_check(spec: &Spec) -> anyhow::Result<()> {
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
    for s in spec.sources.keys() {
        ensure!(valid_id(s) && !s.contains('/'), "bad source name {s:?}");
    }
    Ok(())
}

fn str_field<'a>(v: &'a Value, k: &str, what: &str) -> anyhow::Result<&'a str> {
    v[k].as_str()
        .with_context(|| format!("{what}: no string {k}"))
}

/// Builds a release from the spec, the inventory and the input root.
pub fn build(o: &BuildOptions) -> anyhow::Result<Built> {
    let spec_bytes = std::fs::read(&o.spec).context("reading the spec")?;
    let spec: Spec = serde_json::from_slice(&spec_bytes).context("parsing the spec")?;
    spec_check(&spec)?;
    let inventory = std::fs::read_to_string(&o.inventory).context("reading the inventory")?;
    let mut inputs = Inputs {
        root: o.inputs.clone(),
        sums: read_sums(&inventory)?,
        used: BTreeSet::new(),
    };

    let name = release_name(&spec.release);
    let dir = o.out.join(&name);
    ensure!(
        !dir.exists(),
        "{} exists; build into an empty directory",
        dir.display()
    );
    let staging = o.out.join(format!(".{name}.part"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let mut samples: BTreeMap<String, Sample> = BTreeMap::new();
    let mut subsets: BTreeMap<String, Subset> = BTreeMap::new();
    let mut images_seen: BTreeMap<String, String> = BTreeMap::new();
    let mut campaign_jobs: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for c in &spec.campaigns {
        let src = &spec.sources[&c.source];
        let cpath = format!("{}/campaign.json", c.path);
        let cbytes = inputs.read(&cpath)?;
        ensure!(
            sha256_hex(&cbytes) == c.campaign_sha256,
            "{cpath}: not the campaign_sha256 the spec names"
        );
        let campaign: Value = serde_json::from_slice(&cbytes).context("parsing campaign.json")?;
        ensure!(
            campaign["evaluation_only"] == json!(true),
            "{cpath}: not evaluation_only"
        );
        let jobs = campaign["jobs"]
            .as_array()
            .with_context(|| format!("{cpath}: no jobs"))?;
        let mut members = Vec::new();
        for j in jobs {
            let id = str_field(j, "crop_id", &cpath)?.to_string();
            ensure!(valid_id(&id), "{cpath}: bad crop id {id:?}");
            let job_id = str_field(j, "id", &cpath)?;
            ensure!(is_hex64(job_id), "{id}: bad job id");
            let unit = format!("{}/units/{job_id}", c.path);
            let job_bytes = inputs.read(&format!("{unit}/iris.json"))?;
            ensure!(
                sha256_hex(&job_bytes) == str_field(j, "job_sha256", &id)?,
                "{id}: iris.json differs from the campaign's job_sha256"
            );
            let job: Value = serde_json::from_slice(&job_bytes)?;
            let page_sha = str_field(j, "page_sha256", &id)?;
            ensure!(
                job["page_sha256"].as_str() == Some(page_sha)
                    && job["crop_id"].as_str() == Some(&id),
                "{id}: iris.json names another image or crop"
            );
            let image = inputs.read(&format!("{unit}/page.jpg"))?;
            ensure!(
                sha256_hex(&image) == page_sha,
                "{id}: page.jpg differs from the campaign's page_sha256"
            );
            if let Some(prev) = images_seen.insert(page_sha.to_string(), id.clone()) {
                bail!("{id} and {prev} have the same image bytes");
            }
            let (width, height) = image::ImageReader::new(std::io::Cursor::new(&image))
                .with_guessed_format()?
                .into_dimensions()
                .with_context(|| format!("{id}: reading the image size"))?;
            let reference = str_field(j, "reference", &id)?;
            ensure!(!reference.trim().is_empty(), "{id}: empty reference");
            let provenance = j["provenance"].clone();
            ensure!(provenance.is_object(), "{id}: no provenance");
            ensure!(
                provenance["for_scoring"] == json!(true),
                "{id}: provenance is not for_scoring"
            );
            ensure!(
                provenance["source_dataset"].as_str() == Some(&c.source),
                "{id}: provenance names another source than the spec's {}",
                c.source
            );
            let page_id = str_field(&provenance, "page_id", &id)?.to_string();
            let languages = match provenance["lang"].as_str() {
                Some(l) => {
                    ensure!(
                        src.languages.iter().any(|x| x == l),
                        "{id}: language {l} is not the source's"
                    );
                    vec![l.to_string()]
                }
                None => src.languages.clone(),
            };
            let image_path = format!("images/{id}.jpg");
            let ref_path = format!("references/{id}.txt");
            let s = Sample {
                damage: json!({"status": "unannotated"}),
                id: id.clone(),
                image: FileRef {
                    bytes: image.len() as u64,
                    path: image_path.clone(),
                    sha256: page_sha.to_string(),
                },
                image_height: height,
                image_width: width,
                languages,
                legacy: json!({
                    "campaign": c.path,
                    "campaign_sha256": c.campaign_sha256,
                    "job_id": job_id,
                    "job_sha256": j["job_sha256"],
                }),
                mix: "monolingual".into(),
                provenance,
                reference: FileRef {
                    bytes: reference.len() as u64,
                    path: ref_path.clone(),
                    sha256: sha256_hex(reference.as_bytes()),
                },
                reference_status: src.annotation_status.clone(),
                scripts: src.scripts.clone(),
                source: c.source.clone(),
                source_page_id: page_id,
                split: "public-eval".into(),
                typeface: "unlabelled".into(),
                unit: src.unit.clone(),
            };
            ensure!(
                samples.insert(id.clone(), s).is_none(),
                "{id} is in more than one campaign"
            );
            files.insert(image_path, image);
            files.insert(ref_path, reference.as_bytes().to_vec());
            members.push(id);
        }
        members.sort();
        ensure!(!members.is_empty(), "{cpath}: no jobs");
        ensure!(
            !subsets.contains_key(&c.subset),
            "subset {} named twice",
            c.subset
        );
        subsets.insert(
            c.subset.clone(),
            Subset {
                count: members.len(),
                description: format!(
                    "The exact samples of campaign {} ({}), unchanged.",
                    c.path, c.source
                ),
                digest: String::new(),
                members,
                pages: 0,
                recipe: json!({"campaign": c.path, "campaign_sha256": c.campaign_sha256}),
            },
        );
        campaign_jobs.insert(c.path.clone(), jobs.clone());

        // The campaign's licence files, under the source; every campaign of a source must carry
        // the same bytes for a name.
        let prefix = format!("{}/LICENSES/", c.path);
        let names: Vec<String> = inputs
            .sums
            .keys()
            .filter_map(|p| p.strip_prefix(&prefix).map(str::to_string))
            .collect();
        ensure!(!names.is_empty(), "{}: no LICENSES files", c.path);
        for n in names {
            ensure!(!n.contains('/'), "{prefix}{n}: nested licence file");
            let data = inputs.read(&format!("{prefix}{n}"))?;
            let key = format!("LICENSES/{}/{n}", c.source);
            if let Some(prev) = files.get(&key) {
                ensure!(
                    prev == &data,
                    "{key}: campaigns of {} carry different bytes",
                    c.source
                );
            }
            files.insert(key, data);
        }
    }

    for d in &spec.derived {
        ensure!(valid_id(&d.id), "bad subset id {:?}", d.id);
        ensure!(!subsets.contains_key(&d.id), "subset {} named twice", d.id);
        let (members, recipe) = match (&d.from, &d.seed, d.take, &d.union) {
            (Some(from), Some(seed), Some(take), None) => {
                let parent = subsets
                    .get(from)
                    .with_context(|| format!("{}: unknown subset {from}", d.id))?;
                ensure!(
                    take > 0 && take < parent.count,
                    "{}: take {take} of {from}'s {}",
                    d.id,
                    parent.count
                );
                let mut ranked: Vec<(String, String)> = parent
                    .members
                    .iter()
                    .map(|m| (sha256_hex(format!("{seed}\n{m}").as_bytes()), m.clone()))
                    .collect();
                ranked.sort();
                let mut members: Vec<String> =
                    ranked.into_iter().take(take).map(|(_, m)| m).collect();
                members.sort();
                (members, json!({"from": from, "seed": seed, "take": take}))
            }
            (None, None, None, Some(union)) => {
                ensure!(union.len() >= 2, "{}: a union of fewer than two", d.id);
                let mut all = BTreeSet::new();
                for u in union {
                    let s = subsets
                        .get(u)
                        .with_context(|| format!("{}: unknown subset {u}", d.id))?;
                    all.extend(s.members.iter().cloned());
                }
                (all.into_iter().collect(), json!({"union": union}))
            }
            _ => bail!("{}: give either from, seed and take, or union", d.id),
        };
        subsets.insert(
            d.id.clone(),
            Subset {
                count: members.len(),
                description: d.description.clone(),
                digest: String::new(),
                members,
                pages: 0,
                recipe,
            },
        );
    }
    for s in subsets.values_mut() {
        s.digest = subset_digest(&s.members, &samples);
        s.pages = page_count(&s.members, &samples);
    }

    // Recorded outputs, scored below for parity.
    let mut examples = Vec::new();
    for e in &spec.examples {
        ensure!(
            valid_id(&e.name) && !e.name.contains('/'),
            "bad example name"
        );
        let report: Value = serde_json::from_slice(&inputs.read(&e.report)?)
            .with_context(|| format!("parsing {}", e.report))?;
        let jobs = campaign_jobs
            .get(&e.campaign)
            .with_context(|| format!("example {}: unknown campaign", e.name))?;
        let crop_of: BTreeMap<&str, &str> = jobs
            .iter()
            .filter_map(|j| Some((j["id"].as_str()?, j["crop_id"].as_str()?)))
            .collect();
        let mut rows = BTreeMap::new();
        for r in report["rows"].as_array().context("report has no rows")? {
            if r["status"] != json!("ok") {
                continue;
            }
            let job = str_field(r, "id", &e.report)?;
            let crop = *crop_of
                .get(job)
                .with_context(|| format!("{}: row for an unknown job", e.report))?;
            ensure!(
                r["crop_id"].as_str() == Some(crop),
                "{}: row crop id",
                e.report
            );
            rows.insert(crop.to_string(), str_field(r, "text", crop)?.to_string());
        }
        let path = format!("examples/{}.jsonl", e.name);
        let mut text = String::new();
        for (id, t) in &rows {
            text.push_str(&serde_json::to_string(&json!({"id": id, "text": t}))?);
            text.push('\n');
        }
        files.insert(path.clone(), text.into_bytes());
        examples.push((e, path, rows));
    }

    let unused: Vec<_> = inputs
        .sums
        .keys()
        .filter(|p| !inputs.used.contains(*p))
        .collect();
    ensure!(
        unused.is_empty(),
        "{} inventory files were not used, e.g. {}",
        unused.len(),
        unused[0]
    );

    for (name, src) in &spec.sources {
        let notice = format!(
            "Source: {name}\n\nAttribution: {}\n\nImages: {}\n\nReference transcriptions: {}\n\n\
             Changes: images are the exact crops and line pairs the usnewsmap Iris campaigns (#703) \
             were scored on, cut from the source pages; references are the published transcriptions \
             of those regions, unchanged.\n",
            src.attribution, src.image_license, src.reference_license
        );
        files.insert(format!("LICENSES/{name}/NOTICE.txt"), notice.into_bytes());
    }

    let mut coverage = BTreeMap::new();
    for s in samples.values() {
        let key = format!(
            "{}|{}|{}|{}|{}",
            s.source,
            s.languages.join("+"),
            s.scripts.join("+"),
            s.typeface,
            s.unit
        );
        let e = coverage.entry(key).or_insert((0usize, BTreeSet::new()));
        e.0 += 1;
        e.1.insert(s.source_page_id.clone());
    }
    let coverage = json!(coverage
        .into_iter()
        .map(|(k, (n, pages))| {
            let p: Vec<&str> = k.split('|').collect();
            json!({
                "damage": "unannotated", "languages": p[1], "pages": pages.len(), "samples": n,
                "scripts": p[2], "source": p[0], "typeface": p[3], "unit": p[4],
            })
        })
        .collect::<Vec<_>>());

    let mut manifest = Manifest {
        coverage,
        examples: Vec::new(),
        format: FORMAT.into(),
        inputs_sha256: sha256_hex(inventory.as_bytes()),
        limitations: spec.limitations.clone(),
        release: spec.release.clone(),
        rules: RULES.iter().map(|r| r.to_string()).collect(),
        samples: samples.values().cloned().collect(),
        schema: SCHEMA,
        sources: spec.sources.clone(),
        spec_sha256: sha256_hex(&spec_bytes),
        subsets,
    };
    // Write the data files first so the examples can be scored from disk, as a client would.
    for (p, data) in &files {
        let out = staging.join(p);
        std::fs::create_dir_all(out.parent().context("path")?)?;
        std::fs::write(out, data)?;
    }
    for (e, path, rows) in examples {
        let (t, _) = score(&staging, &manifest, &e.subset, &rows, "legacy-ws-v1")?;
        let cer = t.cer().context("example scored no characters")?;
        ensure!(
            cer == e.expect_cer,
            "example {}: CER {cer} ({} / {}), the spec expects {}",
            e.name,
            t.edits,
            t.reference_chars,
            e.expect_cer
        );
        manifest.examples.push(Example {
            description: e.description.clone(),
            edits: t.edits,
            expect_cer: e.expect_cer,
            hypotheses: path,
            name: e.name.clone(),
            reference_chars: t.reference_chars,
            rule: "legacy-ws-v1".into(),
            subset: e.subset.clone(),
        });
    }
    let notes = release_notes(&manifest);
    files.insert("RELEASE-NOTES.md".into(), notes.clone().into_bytes());
    std::fs::write(staging.join("RELEASE-NOTES.md"), notes)?;
    let mbytes = pretty(&manifest)?;
    std::fs::write(staging.join(MANIFEST), &mbytes)?;
    files.insert(MANIFEST.into(), mbytes.clone());
    let sums: BTreeMap<String, String> = files
        .iter()
        .map(|(p, d)| (p.clone(), sha256_hex(d)))
        .collect();
    let sums_text = write_sums(&sums);
    std::fs::write(staging.join(SUMS), &sums_text)?;
    std::fs::rename(&staging, &dir)?;

    let archive = o.out.join(format!("{name}.tar"));
    write_tar(&dir, &archive)?;
    let v = verify(&dir)?;
    let tar_bytes = std::fs::read(&archive)?;
    let index = json!({
        "archive": {
            "bytes": tar_bytes.len(),
            "name": format!("{name}.tar"),
            "sha256": sha256_hex(&tar_bytes),
        },
        "content_digest": v.content_digest,
        "inputs_sha256": manifest.inputs_sha256,
        "manifest_sha256": sha256_hex(&mbytes),
        "release": manifest.release,
        "samples": manifest.samples.len(),
        "spec_sha256": manifest.spec_sha256,
        "subsets": manifest.subsets.iter().map(|(k, s)| (k.clone(), json!({
            "count": s.count, "digest": s.digest, "pages": s.pages,
        }))).collect::<serde_json::Map<_, _>>(),
        "tag": format!("ocr-bench-{}", manifest.release),
    });
    Ok(Built {
        archive,
        dir,
        index,
    })
}

fn release_notes(m: &Manifest) -> String {
    let mut s = format!(
        "# usnm OCR benchmark {}\n\n{} samples from {} sources. The samples, subsets, sources and \
         licences are listed in manifest.json; SHA256SUMS lists every file, and its sha256 is the \
         release's content digest. docs/ocr-bench.md in the usnewsmap.com repository has the \
         contract.\n\n## Subsets\n\n| Subset | Samples | Source pages |\n| --- | ---: | ---: |\n",
        m.release,
        m.samples.len(),
        m.sources.len()
    );
    for (k, v) in &m.subsets {
        s.push_str(&format!("| `{k}` | {} | {} |\n", v.count, v.pages));
    }
    s.push_str("\n## Known limitations\n\n");
    for l in &m.limitations {
        s.push_str(&format!("- {l}\n"));
    }
    s.push_str("\n## Licences\n\n");
    for (k, v) in &m.sources {
        s.push_str(&format!(
            "- `{k}`: {} Images: {}. References: {}.\n",
            v.attribution, v.image_license, v.reference_license
        ));
    }
    s
}

fn list_files(dir: &Path) -> anyhow::Result<BTreeSet<String>> {
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

fn write_tar(dir: &Path, tar: &Path) -> anyhow::Result<()> {
    let top = dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("release dir name")?;
    let part = tar.with_extension("tar.part");
    let mut b = tar::Builder::new(std::fs::File::create(&part)?);
    for p in list_files(dir)? {
        let data = std::fs::read(dir.join(&p))?;
        let mut h = tar::Header::new_ustar();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, format!("{top}/{p}"), data.as_slice())?;
    }
    b.into_inner()?.sync_all()?;
    std::fs::rename(part, tar)?;
    Ok(())
}

// ------------------------------------------------------------------ client

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
        ensure!(
            top.get_or_insert_with(|| first.clone()) == &first,
            "release tar has more than one top directory"
        );
        let out = dest.join(&p);
        std::fs::create_dir_all(out.parent().context("path")?)?;
        e.unpack(&out)?;
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

fn read_manifest(dir: &Path) -> anyhow::Result<Manifest> {
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

fn check_manifest(dir: &Path, m: &Manifest, sums: &BTreeMap<String, String>) -> anyhow::Result<()> {
    ensure!(m.format == FORMAT, "not a {FORMAT} manifest");
    ensure!(
        m.schema == SCHEMA,
        "manifest schema {} (this reads {SCHEMA})",
        m.schema
    );
    ensure!(valid_release(&m.release), "bad release {:?}", m.release);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(seed: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(8 + seed as u32, 6, |x, y| {
            image::Rgb([seed, (x as u8).wrapping_mul(20), (y as u8).wrapping_mul(30)])
        });
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out)
            .encode_image(&img)
            .unwrap();
        out
    }

    /// Writes a campaign root like iris-campaign.py's and returns its inventory lines.
    fn campaign(
        root: &Path,
        path: &str,
        source: &str,
        ids: &[(&str, &str, &str)],
    ) -> (String, String) {
        let dir = root.join(path);
        std::fs::create_dir_all(dir.join("LICENSES")).unwrap();
        std::fs::write(
            dir.join("LICENSES/SOURCE.json"),
            format!("{{\"dataset\":\"{source}\"}}\n"),
        )
        .unwrap();
        let mut jobs = Vec::new();
        for (crop, page, reference) in ids {
            // A different width per image, so no two crops share bytes.
            static NEXT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
            let img = jpeg(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 200);
            let page_sha = sha256_hex(&img);
            let job_id = sha256_hex(crop.as_bytes());
            let job =
                serde_json::to_vec(&json!({"crop_id": crop, "page_sha256": page_sha})).unwrap();
            let unit = dir.join("units").join(&job_id);
            std::fs::create_dir_all(&unit).unwrap();
            std::fs::write(unit.join("iris.json"), &job).unwrap();
            std::fs::write(unit.join("page.jpg"), &img).unwrap();
            jobs.push(json!({
                "crop_id": crop, "id": job_id, "job_sha256": sha256_hex(&job), "page_sha256": page_sha,
                "reference": reference,
                "provenance": {"for_scoring": true, "page_id": page, "source_dataset": source},
            }));
        }
        let c = serde_json::to_vec(&json!({"evaluation_only": true, "jobs": jobs})).unwrap();
        std::fs::write(dir.join("campaign.json"), &c).unwrap();
        (sha256_hex(&c), job_ids_report(&jobs))
    }

    fn job_ids_report(jobs: &[Value]) -> String {
        let rows: Vec<Value> = jobs
            .iter()
            .map(|j| {
                let r = j["reference"].as_str().unwrap();
                // One wrong character per row, plus whitespace the legacy rule ignores.
                let text = format!("  {}x\n", &r[..r.len() - 1]);
                json!({"crop_id": j["crop_id"], "id": j["id"], "status": "ok", "text": text})
            })
            .collect();
        serde_json::to_string(&json!({"rows": rows})).unwrap()
    }

    fn inventory(root: &Path) -> String {
        let mut sums = BTreeMap::new();
        for p in list_files(root).unwrap() {
            sums.insert(
                p.clone(),
                sha256_hex(&std::fs::read(root.join(&p)).unwrap()),
            );
        }
        write_sums(&sums)
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        opts: BuildOptions,
    }

    fn fixture(expect_cer: Option<f64>) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = tmp.path().join("inputs");
        let (a_sha, a_report) = campaign(
            &inputs,
            "pilot",
            "en-src",
            &[
                ("pub-a-001", "p1", "One  line\nof text."),
                ("pub-a-002", "p1", "Second crop."),
                ("pub-a-003", "p2", "Third crop here"),
            ],
        );
        std::fs::write(inputs.join("pilot/report.json"), &a_report).unwrap();
        let (b_sha, _) = campaign(
            &inputs,
            "de/c0",
            "de-src",
            &[
                ("pub-b-001", "q1", "Zwei Zeilen\nhier"),
                ("pub-b-002", "q2", "Noch ſo eine"),
            ],
        );
        let inv = tmp.path().join("inputs.sha256");
        std::fs::write(&inv, inventory(&inputs)).unwrap();
        // pub-a-001: "One line of text." is 17 characters; -002 12; -003 15: 3 edits / 44.
        let cer = expect_cer.unwrap_or(3.0 / 44.0);
        let source = |lang: &str, unit: &str| {
            json!({"annotation_status": "human-verified", "attribution": "A", "image_license": "PD",
                   "languages": [lang], "reference_license": "CC BY", "scripts": ["Latn"], "unit": unit})
        };
        let spec = json!({
            "campaigns": [
                {"campaign_sha256": a_sha, "path": "pilot", "source": "en-src", "subset": "legacy/en-3"},
                {"campaign_sha256": b_sha, "path": "de/c0", "source": "de-src", "subset": "legacy/de-2"},
            ],
            "derived": [
                {"description": "two", "from": "legacy/en-3", "id": "en/smoke-2", "seed": "s", "take": 2},
                {"description": "all", "id": "all-5", "union": ["legacy/en-3", "legacy/de-2"]},
            ],
            "examples": [{"campaign": "pilot", "description": "d", "expect_cer": cer, "name": "pilot",
                          "report": "pilot/report.json", "subset": "legacy/en-3"}],
            "format": "usnm-ocr-bench-spec",
            "limitations": ["small"],
            "release": "v1.0.0",
            "sources": {"de-src": source("deu", "line-pair"), "en-src": source("eng", "crop")},
        });
        let spec_path = tmp.path().join("spec.json");
        std::fs::write(&spec_path, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();
        let out = tmp.path().join("out");
        Fixture {
            opts: BuildOptions {
                spec: spec_path,
                inventory: inv,
                inputs,
                out,
            },
            _tmp: tmp,
        }
    }

    #[test]
    fn builds_verifies_and_is_reproducible() {
        let f = fixture(None);
        let a = build(&f.opts).unwrap();
        assert_eq!(a.index["samples"], 5);
        assert_eq!(a.index["subsets"]["legacy/en-3"]["pages"], 2);
        assert_eq!(a.index["subsets"]["en/smoke-2"]["count"], 2);
        let m = load(&a.dir).unwrap();
        assert_eq!(m.examples[0].edits, 3);
        assert_eq!(m.examples[0].reference_chars, 44);
        // The same inputs give the same bytes.
        let again = BuildOptions {
            out: f.opts.out.with_file_name("out2"),
            spec: f.opts.spec.clone(),
            inventory: f.opts.inventory.clone(),
            inputs: f.opts.inputs.clone(),
        };
        let b = build(&again).unwrap();
        assert_eq!(a.index, b.index);
        assert_eq!(
            std::fs::read(&a.archive).unwrap(),
            std::fs::read(&b.archive).unwrap()
        );
        // And the tar unpacks to a release that verifies with the same digest.
        let tmp = tempfile::tempdir().unwrap();
        let dir = unpack(&a.archive, tmp.path()).unwrap();
        assert_eq!(
            verify(&dir).unwrap().content_digest,
            a.index["content_digest"]
        );
        let index = json!({"releases": [a.index]});
        check_index(&index, &verify(&dir).unwrap(), &load(&dir).unwrap()).unwrap();
    }

    #[test]
    fn parity_mismatch_stops_the_build() {
        let f = fixture(Some(0.5));
        let e = build(&f.opts).unwrap_err().to_string();
        assert!(e.contains("the spec expects 0.5"), "{e}");
    }

    #[test]
    fn a_changed_input_stops_the_build() {
        let f = fixture(None);
        let img = list_files(&f.opts.inputs)
            .unwrap()
            .into_iter()
            .find(|p| p.ends_with("page.jpg"))
            .unwrap();
        std::fs::write(f.opts.inputs.join(&img), b"not the crop").unwrap();
        let e = build(&f.opts).unwrap_err().to_string();
        assert!(e.contains("differs from the input inventory"), "{e}");
    }

    #[test]
    fn tampering_and_extra_files_fail_verification() {
        let f = fixture(None);
        let a = build(&f.opts).unwrap();
        let r = a.dir.join("references/pub-a-002.txt");
        std::fs::write(&r, "Second crop!").unwrap();
        assert!(verify(&a.dir)
            .unwrap_err()
            .to_string()
            .contains("differs from SHA256SUMS"));
        std::fs::write(&r, "Second crop.").unwrap();
        std::fs::write(a.dir.join("images/extra.jpg"), b"x").unwrap();
        assert!(verify(&a.dir)
            .unwrap_err()
            .to_string()
            .contains("not in SHA256SUMS"));
    }

    #[test]
    fn smoke_subset_nests_and_scores_disclose_missing() {
        let f = fixture(None);
        let a = build(&f.opts).unwrap();
        let m = load(&a.dir).unwrap();
        let parent: BTreeSet<_> = m.subsets["legacy/en-3"].members.iter().collect();
        assert!(m.subsets["en/smoke-2"]
            .members
            .iter()
            .all(|x| parent.contains(x)));
        let outputs =
            read_outputs("{\"id\":\"pub-a-001\",\"text\":\"One line of text.\"}\n").unwrap();
        let (t, _) = score(&a.dir, &m, "legacy/en-3", &outputs, "legacy-ws-v1").unwrap();
        assert_eq!((t.scored, t.missing, t.edits), (1, 2, 0));
        assert_eq!(t.cer(), Some(0.0));
        assert_eq!(t.cer_missing_as_empty(), Some(27.0 / 44.0));
        let stray = read_outputs("{\"id\":\"pub-b-001\",\"text\":\"x\"}").unwrap();
        assert!(score(&a.dir, &m, "legacy/en-3", &stray, "raw-v1").is_err());
        let rows = select(&a.dir, &m, "all-5", false).unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows[0].get("reference").is_none());
    }

    #[test]
    fn legacy_rule_matches_python_split() {
        assert_eq!(
            normalize("legacy-ws-v1", " a\u{1f}b\u{3000}c\n\n").unwrap(),
            "a b c"
        );
        assert_eq!(
            normalize("legacy-ws-v1", "a\u{200b}b").unwrap(),
            "a\u{200b}b"
        );
        assert_eq!(normalize("raw-v1", " a ").unwrap(), " a ");
        assert!(normalize("nfkc", "a").is_err());
    }
}
