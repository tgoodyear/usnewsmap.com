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

mod build;
mod client;
mod manifest;
mod score;
mod spec;

pub use build::{build, BuildOptions, Built};
pub use client::{check_index, load, select, unpack, verify, Verified};
pub use manifest::{read_sums, release_name, Example, FileRef, Manifest, Sample, Subset};
pub use score::{normalize, read_outputs, score, Tally};
pub use spec::{
    Annotation, Damage, IllegibleSpan, ImportSample, Spec, SpecCampaign, SpecDerived, SpecExample,
    SpecImport, SpecSource, Transcription,
};

/// The manifest's `format`.
pub const FORMAT: &str = "usnm-ocr-bench";
/// The manifest schema version this code reads and writes.
pub const SCHEMA: u32 = 2;
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

#[cfg(test)]
pub(crate) mod tests {
    use super::client::{check_manifest, read_manifest};
    use super::manifest::{list_files, pretty, write_sums};
    use super::*;
    use crate::bundle::sha256_hex;
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

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

    pub(crate) struct Fixture {
        pub(crate) _tmp: tempfile::TempDir,
        pub(crate) opts: BuildOptions,
    }

    pub(crate) fn fixture(expect_cer: Option<f64>) -> Fixture {
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
    fn unpack_refuses_an_existing_release_dir_and_symlinked_parents() {
        let f = fixture(None);
        let a = build(&f.opts).unwrap();
        let work = tempfile::tempdir().unwrap();
        let top = work.path().join(release_name("v1.0.0"));
        std::fs::create_dir_all(&top).unwrap();
        let e = unpack(&a.archive, work.path()).unwrap_err().to_string();
        assert!(e.contains("already exists"), "{e}");
        // A symlinked parent inside dest can't redirect writes outside it.
        std::fs::remove_dir_all(&top).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(&top).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), top.join("images")).unwrap();
        let mut ar = tar::Archive::new(std::fs::File::open(&a.archive).unwrap());
        let mut tried = false;
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            if e.path().unwrap().to_string_lossy().contains("/images/") {
                assert!(!matches!(e.unpack_in(work.path()), Ok(true)));
                tried = true;
                break;
            }
        }
        assert!(tried);
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[test]
    fn an_example_outside_sha256sums_fails_verification() {
        let f = fixture(None);
        let a = build(&f.opts).unwrap();
        let mut m = read_manifest(&a.dir).unwrap();
        let outside = a.dir.with_file_name("outside.jsonl");
        std::fs::copy(a.dir.join("examples/pilot.jsonl"), &outside).unwrap();
        m.examples[0].hypotheses = "../outside.jsonl".into();
        let sums = read_sums(&std::fs::read_to_string(a.dir.join(SUMS)).unwrap()).unwrap();
        let e = check_manifest(&a.dir, &m, &sums).unwrap_err().to_string();
        assert!(e.contains("is not in SHA256SUMS"), "{e}");
    }

    #[test]
    fn bench_imports_build_verify_and_repeat_without_claiming_legacy_history() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("inputs");
        std::fs::create_dir_all(root.join("corpus/LICENSES")).unwrap();
        std::fs::write(
            root.join("corpus/LICENSES/SOURCE.json"),
            "synthetic fixture",
        )
        .unwrap();
        std::fs::write(root.join("corpus/image.jpg"), jpeg(3)).unwrap();
        std::fs::write(root.join("corpus/reference.txt"), "日?本").unwrap();
        let rows = json!([{"id":"example-1", "image":"image.jpg", "reference":"reference.txt",
            "source_page_id":"source-page", "provenance":{"for_scoring":true, "source_dataset":"test"}}]);
        std::fs::write(root.join("corpus/samples.json"), pretty(&rows).unwrap()).unwrap();
        let mut spec = json!({"campaigns":[], "imports":[{"path":"corpus", "source":"test", "subset":"test/all-1"}],
            "annotations":{"example-1":{"transcription":{"policy":"diplomatic-v1", "evidence":"synthetic review", "reviewed_by":"fixture",
                "illegible_spans":[{"start":1,"end":2,"reason":"unknown glyph"}]},
                "damage":{"status":"reviewed", "labels":["blur"], "severity":"moderate", "evidence":"synthetic fixture"}}},
            "derived":[], "examples":[], "format":"usnm-ocr-bench-spec", "limitations":["synthetic"], "release":"v1.1.0",
            "sources":{"test":{"annotation_status":"human-verified", "attribution":"synthetic test", "image_license":"CC0",
                "reference_license":"CC0", "languages":["jpn"], "scripts":["Jpan"], "unit":"crop"}}});
        let spec_path = tmp.path().join("spec.json");
        std::fs::write(&spec_path, pretty(&spec).unwrap()).unwrap();
        let inv = tmp.path().join("inputs.sha256");
        std::fs::write(&inv, inventory(&root)).unwrap();
        let mut opts = BuildOptions {
            spec: spec_path.clone(),
            inventory: inv,
            inputs: root.clone(),
            out: tmp.path().join("first"),
        };
        let first = build(&opts).unwrap();
        verify(&first.dir).unwrap();
        let m = load(&first.dir).unwrap();
        assert_eq!(m.schema, 2);
        assert!(m.samples[0].legacy.is_null());
        assert_eq!(m.samples[0].mix, "unannotated");
        assert_ne!(m.coverage[0]["damage"], "unannotated");
        opts.out = tmp.path().join("second");
        let second = build(&opts).unwrap();
        assert_eq!(
            std::fs::read(first.archive).unwrap(),
            std::fs::read(second.archive).unwrap()
        );
        spec["annotations"]["unknown"] = json!({});
        std::fs::write(&spec_path, pretty(&spec).unwrap()).unwrap();
        opts.out = tmp.path().join("unknown");
        assert!(build(&opts)
            .unwrap_err()
            .to_string()
            .contains("unknown sample"));
        spec["annotations"]
            .as_object_mut()
            .unwrap()
            .remove("unknown");
        spec["imports"][0]["subset"] = json!("../escape");
        std::fs::write(&spec_path, pretty(&spec).unwrap()).unwrap();
        opts.out = tmp.path().join("unsafe");
        assert!(build(&opts).is_err());
    }
}
