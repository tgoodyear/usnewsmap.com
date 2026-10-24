//! Building a release from a spec, an input inventory and the input root.

use super::client::verify;
use super::manifest::{
    is_hex64, list_files, page_count, pretty, read_sums, release_name, safe_path, subset_digest,
    valid_id, write_sums, Example, FileRef, Manifest, Sample, Subset,
};
use super::score::{score, validate_annotations};
use super::spec::{spec_check, str_field, ImportSample, Spec};
use super::{FORMAT, MANIFEST, RULES, SCHEMA, SUMS};
use crate::bundle::sha256_hex;
use anyhow::{bail, ensure, Context};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
                damage: match spec.annotations.get(&id).and_then(|a| a.damage.as_ref()) {
                    Some(d) => serde_json::to_value(d)?,
                    None => json!({"status": "unannotated"}),
                },
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
                transcription: spec
                    .annotations
                    .get(&id)
                    .and_then(|a| a.transcription.clone()),
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

    for import in &spec.imports {
        let src = &spec.sources[&import.source];
        let bytes = inputs.read(&format!("{}/samples.json", import.path))?;
        let imported: Vec<ImportSample> = serde_json::from_slice(&bytes)?;
        let mut members = Vec::new();
        for row in imported {
            ensure!(
                valid_id(&row.id) && !row.source_page_id.trim().is_empty(),
                "invalid imported sample id/page"
            );
            ensure!(
                safe_path(&row.image) && safe_path(&row.reference),
                "unsafe imported path"
            );
            ensure!(
                row.provenance["for_scoring"] == json!(true)
                    && row.provenance["source_dataset"] == json!(import.source),
                "{}: invalid source provenance",
                row.id
            );
            let image = inputs.read(&format!("{}/{}", import.path, row.image))?;
            let reference = inputs.read(&format!("{}/{}", import.path, row.reference))?;
            let text = std::str::from_utf8(&reference)?;
            ensure!(!text.trim().is_empty(), "{}: empty reference", row.id);
            let hash = sha256_hex(&image);
            ensure!(
                images_seen.insert(hash.clone(), row.id.clone()).is_none(),
                "{}: duplicate image",
                row.id
            );
            let (width, height) = image::ImageReader::new(std::io::Cursor::new(&image))
                .with_guessed_format()?
                .into_dimensions()?;
            let image_path = format!("images/{}.jpg", row.id);
            let ref_path = format!("references/{}.txt", row.id);
            let annotation = spec.annotations.get(&row.id);
            let sample = Sample {
                damage: match annotation.and_then(|a| a.damage.as_ref()) {
                    Some(d) => serde_json::to_value(d)?,
                    None => json!({"status":"unannotated"}),
                },
                id: row.id.clone(),
                image: FileRef {
                    bytes: image.len() as u64,
                    path: image_path.clone(),
                    sha256: hash,
                },
                image_height: height,
                image_width: width,
                languages: src.languages.clone(),
                legacy: Value::Null,
                mix: "unannotated".into(),
                provenance: row.provenance,
                reference: FileRef {
                    bytes: reference.len() as u64,
                    path: ref_path.clone(),
                    sha256: sha256_hex(&reference),
                },
                reference_status: src.annotation_status.clone(),
                scripts: src.scripts.clone(),
                source: import.source.clone(),
                source_page_id: row.source_page_id,
                split: "public-eval".into(),
                transcription: annotation.and_then(|a| a.transcription.clone()),
                typeface: "unlabelled".into(),
                unit: src.unit.clone(),
            };
            validate_annotations(&sample, text)?;
            ensure!(
                samples.insert(row.id.clone(), sample).is_none(),
                "duplicate imported id {}",
                row.id
            );
            members.push(row.id);
            files.insert(image_path, image);
            files.insert(ref_path, reference);
        }
        members.sort();
        ensure!(!members.is_empty(), "empty import {}", import.path);
        let subset = Subset {
            count: members.len(),
            description: format!("Published ground truth from {}.", import.source),
            digest: String::new(),
            members,
            pages: 0,
            recipe: json!({"import":import.path}),
        };
        ensure!(
            subsets.insert(import.subset.clone(), subset).is_none(),
            "duplicate import subset"
        );
        let prefix = format!("{}/LICENSES/", import.path);
        let names: Vec<_> = inputs
            .sums
            .keys()
            .filter(|p| p.starts_with(&prefix))
            .cloned()
            .collect();
        ensure!(!names.is_empty(), "{}: no licence materials", import.path);
        for path in names {
            let name = path.strip_prefix(&prefix).unwrap();
            ensure!(!name.contains('/'), "nested import licence");
            let data = inputs.read(&path)?;
            let key = format!("LICENSES/{}/{name}", import.source);
            if let Some(prev) = files.get(&key) {
                ensure!(prev == &data, "different import licence bytes");
            }
            files.insert(key, data);
        }
    }

    for id in spec.annotations.keys() {
        ensure!(
            samples.contains_key(id),
            "annotation for unknown sample {id}"
        );
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
        let changes = if spec.imports.iter().any(|i| &i.source == name) {
            "Images are crops of the credited source pages; references preserve the published ground truth of those regions. Import provenance records the conversion and source archive hashes."
        } else {
            "images are the exact crops and line pairs the usnewsmap Iris campaigns (#703) were scored on, cut from the source pages; references are the published transcriptions of those regions, unchanged."
        };
        let notice = format!(
            "Source: {name}\n\nAttribution: {}\n\nImages: {}\n\nReference transcriptions: {}\n\n\
             Changes: {changes}\n",
            src.attribution, src.image_license, src.reference_license
        );
        files.insert(format!("LICENSES/{name}/NOTICE.txt"), notice.into_bytes());
    }

    let mut coverage = BTreeMap::new();
    for s in samples.values() {
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            s.source,
            s.languages.join("+"),
            s.scripts.join("+"),
            s.typeface,
            s.unit,
            if s.damage["status"] == "unannotated" {
                "unannotated".to_string()
            } else {
                serde_json::to_string(
                    &json!({"status": s.damage["status"], "labels": s.damage["labels"], "severity": s.damage["severity"]}),
                )?
            }
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
                "damage": p[5], "languages": p[1], "pages": pages.len(), "samples": n,
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
        rules: RULES
            .iter()
            .copied()
            .chain(
                (!spec.imports.is_empty() || !spec.annotations.is_empty())
                    .then_some("raw-masked-v1"),
            )
            .map(str::to_string)
            .collect(),
        samples: samples.values().cloned().collect(),
        schema: if spec.imports.is_empty() && spec.annotations.is_empty() {
            1
        } else {
            SCHEMA
        },
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
