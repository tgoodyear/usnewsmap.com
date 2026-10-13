//! Optional BOINC `script_validator` checks for untrusted volunteers.
//!
//! Not used by default: the pilot's trusted-friends setup keeps
//! `sample_trivial_validator`, and the central collector re-checks every
//! result against its job. This module is for the case where two replicas of
//! one task must agree before the server accepts either.
use crate::read;
use anyhow::{bail, ensure, Context};
use std::{collections::HashMap, path::Path};
use usnm_ocr_contract::boinc::{self, ResultManifest};

/// Two replicas agree when the word-bigram Dice score of their text, and of
/// the words in their ALTO, is at least this. Bigrams make the measure
/// order-sensitive: shuffled words share all their words but few pairs.
/// Tesseract is deterministic for one image and version, so honest replicas
/// score 1.0; the slack covers float and thread differences between CPUs
/// (one changed word in fifty still passes), not a different engine.
pub const SIMILARITY_THRESHOLD: f64 = 0.95;

/// One result's three output files, in the order of the result template.
pub struct Outputs {
    pub text: Vec<u8>,
    pub alto: Vec<u8>,
    pub manifest: ResultManifest,
}

/// Read and check one result without a job: the manifest parses, the digests
/// match the files, the text is UTF-8 and the ALTO passes the shared checks.
/// The job itself is compared later, centrally, by `ResultManifest::matches`.
pub fn load(text: &Path, alto: &Path, manifest: &Path) -> anyhow::Result<Outputs> {
    let text = read(text, boinc::MAX_TEXT)?;
    let alto = read(alto, boinc::MAX_ALTO)?;
    let manifest: ResultManifest = serde_json::from_slice(&read(manifest, boinc::MAX_JSON)?)
        .context("result.json is not a result manifest")?;
    ensure!(manifest.v == boinc::VERSION, "unknown result version");
    chrono::DateTime::parse_from_rfc3339(&manifest.completed_at)
        .context("completed_at is not an RFC 3339 timestamp")?;
    ensure!(
        manifest.text_sha256 == boinc::sha256(&text)
            && manifest.alto_sha256 == boinc::sha256(&alto),
        "output checksum mismatch"
    );
    ensure!(std::str::from_utf8(&text).is_ok(), "text is not UTF-8");
    usnm_alto::check(&alto, &usnm_alto::Limits::default())
        .map_err(|e| anyhow::anyhow!(e.code()))?;
    Ok(Outputs {
        text,
        alto,
        manifest,
    })
}

/// Word-bigram Dice score of two texts, ignoring case and whitespace. Word
/// order matters: the score counts adjacent word pairs. A text of one word
/// has no pairs, so it is compared as a single unigram. Linear in the text
/// size. Two empty texts score 1.0.
pub fn similarity(a: &str, b: &str) -> f64 {
    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_lowercase).collect()
    }
    similarity_words(&words(a), &words(b))
}

fn similarity_words(a: &[String], b: &[String]) -> f64 {
    fn grams(w: &[String]) -> (HashMap<String, u32>, u32) {
        let mut m = HashMap::new();
        if w.len() == 1 {
            m.insert(w[0].clone(), 1);
            return (m, 1);
        }
        let mut n = 0;
        for p in w.windows(2) {
            *m.entry(format!("{}\u{1}{}", p[0], p[1])).or_insert(0) += 1;
            n += 1;
        }
        (m, n)
    }
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (ca, na) = grams(a);
    let (cb, nb) = grams(b);
    if na + nb == 0 {
        return 0.0;
    }
    let shared: u32 = ca
        .iter()
        .map(|(g, n)| (*n).min(cb.get(g).copied().unwrap_or(0)))
        .sum();
    2.0 * f64::from(shared) / f64::from(na + nb)
}

/// The `CONTENT` values of the `String` elements in an ALTO file, in
/// document order, parsed as XML so comments, other elements and any quoting
/// style are handled. Attribute values are unescaped and lowercased.
fn alto_words(alto: &[u8]) -> Vec<String> {
    use quick_xml::events::Event;
    let mut out = Vec::new();
    let Ok(text) = std::str::from_utf8(alto) else {
        return out;
    };
    let mut reader = quick_xml::NsReader::from_str(text);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == "String" => {
                for attr in e.attributes().flatten() {
                    if attr.key.local_name().as_ref() == "CONTENT" {
                        if let Ok(v) = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
                            out.push(v.to_lowercase());
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Do two replicas of one task agree? Same job, scan and engine, and both
/// the text and the ALTO words at or above [`SIMILARITY_THRESHOLD`]. Returns
/// the lower of the two scores.
pub fn compare(a: &Outputs, b: &Outputs) -> anyhow::Result<f64> {
    let (x, y) = (&a.manifest, &b.manifest);
    ensure!(
        x.job_id == y.job_id && x.scan_sha256 == y.scan_sha256 && x.engine == y.engine,
        "replicas answer different jobs or engines"
    );
    let score = similarity(std::str::from_utf8(&a.text)?, std::str::from_utf8(&b.text)?);
    if score < SIMILARITY_THRESHOLD {
        bail!("text similarity below threshold");
    }
    let alto = similarity_words(&alto_words(&a.alto), &alto_words(&b.alto));
    if alto < SIMILARITY_THRESHOLD {
        bail!("ALTO similarity below threshold");
    }
    Ok(score.min(alto))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_scores_one() {
        assert_eq!(similarity("The  Daily News", "the daily\nnews"), 1.0);
        assert_eq!(similarity("", " \n"), 1.0);
    }

    #[test]
    fn one_word_in_fifty_passes_and_one_in_ten_fails() {
        let base: Vec<String> = (0..50).map(|i| format!("w{i}")).collect();
        let mut one = base.clone();
        one[7] = "x".into();
        assert!(similarity(&base.join(" "), &one.join(" ")) >= SIMILARITY_THRESHOLD);
        let base: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
        let mut two = base.clone();
        two[1] = "x".into();
        two[2] = "y".into();
        assert!(similarity(&base.join(" "), &two.join(" ")) < SIMILARITY_THRESHOLD);
    }

    #[test]
    fn empty_against_text_scores_zero() {
        assert_eq!(similarity("", "some words"), 0.0);
    }

    #[test]
    fn repeats_count_once_per_occurrence() {
        assert!(similarity("a a a a", "a") < 0.5);
    }

    #[test]
    fn shuffled_words_fail() {
        let base: Vec<String> = (0..60).map(|i| format!("w{i}")).collect();
        let mut rev = base.clone();
        rev.reverse();
        assert!(similarity(&base.join(" "), &rev.join(" ")) < SIMILARITY_THRESHOLD);
        let mut swap = base.clone();
        swap.rotate_left(30);
        assert!(similarity(&base.join(" "), &swap.join(" ")) >= 0.9);
    }

    fn alto_for(text: &str) -> String {
        let strings: String = text
            .split_whitespace()
            .map(|w| format!(r#"<String CONTENT="{w}"/>"#))
            .collect();
        format!(
            r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout><Page WIDTH="10" HEIGHT="10"><PrintSpace><TextBlock><TextLine>{strings}</TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#
        )
    }

    fn write(dir: &Path, job: &str, engine: &str, text: &str) -> [std::path::PathBuf; 3] {
        write_with(
            dir,
            job,
            engine,
            text,
            &alto_for(text),
            "2026-01-01T00:00:00Z",
        )
    }

    fn write_with(
        dir: &Path,
        job: &str,
        engine: &str,
        text: &str,
        alto: &str,
        completed_at: &str,
    ) -> [std::path::PathBuf; 3] {
        let manifest = serde_json::json!({
            "v": 1, "job_id": job, "scan_sha256": "0".repeat(64),
            "engine": {"image": "r/i@sha256:".to_owned() + &"1".repeat(64), "version": engine,
                "profile": "eng", "models": "m", "psm": 1, "dpi": 300, "timeout_secs": 60},
            "text_sha256": boinc::sha256(text.as_bytes()),
            "alto_sha256": boinc::sha256(alto.as_bytes()),
            "elapsed_ms": 1, "completed_at": completed_at
        });
        let p = [dir.join("t"), dir.join("a"), dir.join("r")];
        std::fs::write(&p[0], text).unwrap();
        std::fs::write(&p[1], alto).unwrap();
        std::fs::write(&p[2], manifest.to_string()).unwrap();
        p
    }

    fn open(p: &[std::path::PathBuf; 3]) -> anyhow::Result<Outputs> {
        load(&p[0], &p[1], &p[2])
    }

    #[test]
    fn replicas_compare_on_files() {
        let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let words = (0..60)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let a = open(&write(d1.path(), "j", "5", &words)).unwrap();
        let b = open(&write(d2.path(), "j", "5", &words.replace("w3 ", "z "))).unwrap();
        assert!(compare(&a, &b).unwrap() >= SIMILARITY_THRESHOLD);
        let other = open(&write(d2.path(), "j", "5", "unrelated text entirely")).unwrap();
        assert!(compare(&a, &other).is_err());
        let engine = open(&write(d2.path(), "j", "6", &words)).unwrap();
        assert!(compare(&a, &engine).is_err());
        let job = open(&write(d2.path(), "k", "5", &words)).unwrap();
        assert!(compare(&a, &job).is_err());
    }

    #[test]
    fn divergent_alto_fails() {
        let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let words = (0..60)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let a = open(&write(d1.path(), "j", "5", &words)).unwrap();
        let other = alto_for("entirely different words in the layout of this page here");
        let b = open(&write_with(
            d2.path(),
            "j",
            "5",
            &words,
            &other,
            "2026-01-01T00:00:00Z",
        ))
        .unwrap();
        assert!(compare(&a, &b).is_err());
    }

    #[test]
    fn alto_words_come_from_string_elements_only() {
        let x = br#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><!-- <String CONTENT="fake"/> x CONTENT="evil" --><Layout><Page WIDTH="1" HEIGHT="1"><PrintSpace><TextBlock><TextLine><String  ID='a'   CONTENT='One'/><String CONTENT = "t&amp;o"></String><Other CONTENT="no"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#;
        assert_eq!(alto_words(x), ["one", "t&o"]);
    }

    #[test]
    fn comment_injection_cannot_mask_divergent_alto() {
        let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let words = (0..60)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let a = open(&write(d1.path(), "j", "5", &words)).unwrap();
        let honest = alto_for(&words);
        let injected = honest.replace(
            "<Layout>",
            &format!(
                "<!--{}--><Layout>",
                alto_for(&words).replace('<', " ").replace("/>", "")
            ),
        );
        let forged = injected.replace(r#"CONTENT="w"#, r#"CONTENT="x"#);
        let b = open(&write_with(
            d2.path(),
            "j",
            "5",
            &words,
            &forged,
            "2026-01-01T00:00:00Z",
        ))
        .unwrap();
        assert!(compare(&a, &b).is_err());
    }

    #[test]
    fn bad_timestamp_is_refused() {
        let d = tempfile::tempdir().unwrap();
        for ts in ["yesterday", "", "2026-13-40T00:00:00Z"] {
            let p = write_with(d.path(), "j", "5", "hello", &alto_for("hello"), ts);
            assert!(open(&p).is_err(), "{ts}");
        }
    }

    #[test]
    fn tampered_text_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "j", "5", "hello");
        std::fs::write(&p[0], "hellO").unwrap();
        assert!(open(&p).is_err());
    }
}
