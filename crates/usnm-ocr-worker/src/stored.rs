//! The page's stored outputs from an engine's reading (ADR-0016 decision
//! 5, 13 §13.4.7 steps 2 to 4): the engine's ALTO is parsed, the stored
//! ALTO 3.1 is written from the parsed values (`usnm_alto::write`), and the
//! page text is derived from that page in reading order, then normalized.
//! The text and the ALTO always hold the same words, and no engine XML is
//! stored.

use chrono::{DateTime, SecondsFormat, Utc};
use std::collections::BTreeMap;
use usnm_alto::{Description, Page, ParseOptions, Software, Step};

use usnm_core::ocr_engine::{EngineTag, OcrEngineTag};
use usnm_core::text::{normalize_ocr_for, Hyphenation};

use crate::engine::ReadInfo;

/// This worker as the stored ALTO names it: the crate version, plus the
/// commit CI built it from (`USNM_GIT_SHA`), e.g. `0.1.0+1dcc5c3`.
pub fn worker_version() -> String {
    let v = env!("CARGO_PKG_VERSION");
    match option_env!("USNM_GIT_SHA").filter(|s| !s.is_empty()) {
        Some(sha) => format!("{v}+{}", &sha[..sha.len().min(7)]),
        None => v.to_owned(),
    }
}

/// The file the engine read.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    /// The scan's URL: the IIIF full image, or a batch's JP2.
    pub url: &'a str,
    /// The sha256 of the bytes read.
    pub sha256: &'a str,
    /// A batch JP2's md5 from its bag manifest, which the bytes matched.
    pub md5: Option<&'a str>,
    /// The page's `run_id` (13 §13.4.3), when the worker minted one.
    pub run_id: Option<&'a str>,
    /// The queue message's `score`, the page's damage before.
    pub damage_before: Option<f64>,
}

/// What the worker uploads for a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    /// The stored ALTO 3.1.
    pub alto: String,
    /// The page text, normalized (`normalize_ocr`).
    pub text: String,
    pub page: Page,
}

/// Why an engine's output couldn't be stored: the worker's `convert`.
#[derive(Debug, thiserror::Error)]
#[error("the engine's ALTO doesn't convert: {0}")]
pub struct ConvertError(#[from] pub usnm_alto::ParseError);

fn hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The stored outputs for a page read as `ocr_engine` (its full tag) in
/// `language` (ISO 639-3) from engine ALTO in pixels of the image read,
/// at `dpi`. `worker_version` names this worker in the ALTO.
pub fn from_engine_alto(
    engine_alto: &[u8],
    ocr_engine: &str,
    language: &str,
    dpi: u32,
    source: &Source<'_>,
    worker_version: &str,
    now: DateTime<Utc>,
) -> Result<Stored, ConvertError> {
    let page = usnm_alto::parse(engine_alto, &ParseOptions::default())?;
    Ok(store(
        page,
        ocr_engine,
        language,
        dpi,
        source,
        worker_version,
        now,
    ))
}

/// The stored outputs for an engine's reading: its page when it wrote
/// boxes, else its ALTO parsed.
/// The page is taken from `out`, so a large reading isn't held twice.
pub fn from_output(
    out: &mut crate::engine::EngineOutput,
    language: &str,
    dpi: u32,
    source: &Source<'_>,
    worker_version: &str,
    now: DateTime<Utc>,
) -> Result<Stored, ConvertError> {
    let page = match out.page.take() {
        Some(page) => page,
        None => usnm_alto::parse(&out.alto, &ParseOptions::default())?,
    };
    Ok(store_from(
        page,
        out.layout_from,
        &out.engine,
        Some(&out.read),
        language,
        dpi,
        source,
        worker_version,
        now,
    ))
}

/// The stored outputs for a page parsed from engine ALTO.
pub fn store(
    page: Page,
    ocr_engine: &str,
    language: &str,
    dpi: u32,
    source: &Source<'_>,
    worker_version: &str,
    now: DateTime<Utc>,
) -> Stored {
    store_from(
        page,
        "ALTO",
        ocr_engine,
        None,
        language,
        dpi,
        source,
        worker_version,
        now,
    )
}

/// The stored outputs for a page parsed from what the engine wrote
/// (`from`: `ALTO` or `boxes.json`), with the engine's side of its
/// provenance when known.
#[allow(clippy::too_many_arguments)]
pub fn store_from(
    page: Page,
    from: &str,
    ocr_engine: &str,
    read: Option<&ReadInfo>,
    language: &str,
    dpi: u32,
    source: &Source<'_>,
    worker_version: &str,
    now: DateTime<Utc>,
) -> Stored {
    let mut ids = Vec::new();
    if hex(source.sha256) {
        ids.push(("sha256".to_owned(), source.sha256.to_owned()));
        if let Some(m) = source.md5.filter(|m| hex(m)) {
            ids.push(("md5".to_owned(), m.to_owned()));
        }
    }
    // The engine's name and version from its tag; the whole tag goes in
    // the settings, with what it names (13 §13.8.2).
    let tag = match EngineTag::parse(ocr_engine) {
        Ok(EngineTag::Tag(t)) => Some(t),
        _ => None,
    };
    let (family, version) = match &tag {
        Some(t) => (t.family().to_owned(), t.version().to_owned()),
        None => (
            ocr_engine.split('/').next().unwrap_or("").to_owned(),
            String::new(),
        ),
    };
    let creator = match family.as_str() {
        "tesseract" => "tesseract-ocr".to_owned(),
        other => other.to_owned(),
    };
    let desc = Description {
        file_name: source.url.to_owned(),
        file_identifiers: ids,
        ocr: Step {
            date_time: Some(now.to_rfc3339_opts(SecondsFormat::Secs, true)),
            settings: settings(
                ocr_engine,
                tag.as_ref(),
                read,
                page.granularity.as_str(),
                source,
                dpi,
            ),
            software: Software {
                creator,
                name: family,
                version,
            },
        },
        post: Some(Step {
            date_time: None,
            settings: vec![
                ("source".into(), format!("engine {from}, parsed")),
                (
                    "text".into(),
                    "usnm-alto reading order, normalize_ocr".into(),
                ),
            ],
            software: Software {
                creator: "usnewsmap.com".into(),
                name: "usnm-ocr-worker".into(),
                version: worker_version.to_owned(),
            },
        }),
    };
    let alto = usnm_alto::write(&page, &desc, language);
    // A German page, or a Fraktur model's, can end a line in an em dash or a
    // `⸗` for a broken word's hyphen (#1065).
    let text = normalize_ocr_for(
        &usnm_alto::text(&page),
        Hyphenation::for_page(ocr_engine, language),
    );
    Stored { alto, text, page }
}

/// The stored ALTO's `processingStepSettings`, in 13 §13.8.2's order:
/// the full tag first, then what it abbreviates and the page's run.
fn settings(
    ocr_engine: &str,
    tag: Option<&OcrEngineTag>,
    read: Option<&ReadInfo>,
    granularity: &str,
    source: &Source<'_>,
    dpi: u32,
) -> Vec<(String, String)> {
    let mut s = vec![("ocr_engine".to_owned(), ocr_engine.to_owned())];
    if let Some(r) = read {
        s.push(("image".into(), r.image_ref.clone()));
        let models: Vec<String> = serde_json::from_str::<BTreeMap<String, String>>(&r.models_json)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, sha)| {
                let sha = sha.strip_prefix("sha256:").unwrap_or(&sha).to_owned();
                format!("{name}:sha256:{sha}")
            })
            .collect();
        if !models.is_empty() {
            s.push(("models".into(), models.join(",")));
        }
    }
    if let Some(t) = tag {
        s.push(("profile".into(), t.profile().to_owned()));
    }
    if let Some(r) = read {
        let opts: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&r.options_json).unwrap_or_default();
        let opts: Vec<String> = opts
            .into_iter()
            .map(|(k, v)| match v {
                serde_json::Value::String(v) => format!("{k}={v}"),
                v => format!("{k}={}", v),
            })
            .collect();
        s.push(("options".into(), opts.join(",")));
    }
    if let Some(t) = tag {
        s.push(("route".into(), t.route().to_owned()));
        s.push(("routing".into(), format!("r{}", t.route_version())));
        s.push(("contract".into(), t.contract().to_string()));
    }
    if let Some(r) = read {
        s.push(("adapter".into(), r.adapter.clone()));
    }
    s.push(("granularity".into(), granularity.to_owned()));
    if let Some(id) = source.run_id {
        s.push(("run_id".into(), id.to_owned()));
    }
    if hex(source.sha256) {
        s.push(("input_sha256".into(), source.sha256.to_owned()));
    }
    s.push(("dpi".into(), format!("{dpi} (assumed)")));
    if let Some(d) = source.damage_before {
        s.push(("damage_before".into(), format!("{d}")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::set_alto_source;

    const TAG: &str = "tesseract/5.5.1 profile=eng image=sha256:3f9a0c1d2e4b models=sha256:8c2d71aa09fe route=minimal@r1 contract=0";
    const URL: &str = "https://tile.loc.gov/image-services/iiif/service:ndnp:az:batch_az_agave_ver01:data:sn84024828:00211100618:1894062301:0285/full/full/0/default.jpg";
    const SHA: &str = "0207a56216a746f2d137d019ff44c49c83d4f403efde9b1eddba67ab6ebe326b";

    /// Tesseract 5.5's own ALTO and text for two pages: a crop of a real
    /// 1894 page (Mohave County Miner, from LoC's IIIF service, read with
    /// `--psm 3 --dpi 300`) and a rendered two-column page.
    fn fixtures() -> Vec<(&'static str, Vec<u8>, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        [
            (
                "testdata/tesseract-agave-1894-06-23",
                dir.join("testdata/tesseract-agave-1894-06-23"),
            ),
            (
                "usnm-alto tesseract-5.5",
                dir.join("../usnm-alto/tests/data/tesseract-5.5"),
            ),
        ]
        .into_iter()
        .map(|(name, base)| {
            (
                name,
                std::fs::read(format!("{}.xml", base.display())).unwrap(),
                std::fs::read_to_string(format!("{}.txt", base.display())).unwrap(),
            )
        })
        .collect()
    }

    fn page_of(alto: &str) -> Page {
        usnm_alto::parse(alto.as_bytes(), &ParseOptions::default()).unwrap()
    }

    /// The text between `<tag>` and `</tag>`s, in order.
    fn elements(xml: &str, tag: &str) -> Vec<String> {
        let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
        xml.match_indices(&open)
            .filter_map(|(i, _)| {
                let rest = &xml[i..];
                let start = rest.find('>')? + 1;
                let end = rest.find(&close)?;
                Some(rest[start..end].to_owned())
            })
            .collect()
    }

    /// Regression against main's output for Tesseract pages: main stored
    /// Tesseract's own ALTO with `fileName` replaced and a `fileIdentifier`
    /// added, and `normalize_ocr` of Tesseract's `page.txt`.
    #[test]
    fn tesseract_pages_store_what_main_stored() {
        let now: DateTime<Utc> = "2026-10-20T14:04:31Z".parse().unwrap();
        for md5 in [None, Some("cd34")] {
            for (name, xml, txt) in fixtures() {
                let source = Source {
                    url: URL,
                    sha256: SHA,
                    md5,

                    run_id: None,

                    damage_before: None,
                };
                let new = from_engine_alto(&xml, TAG, "eng", 300, &source, "0.1.0", now).unwrap();
                let old_alto = set_alto_source(&String::from_utf8_lossy(&xml), URL, SHA, md5);
                let old_text = usnm_core::text::normalize_ocr(&txt);
                // The same text.
                assert_eq!(new.text, old_text, "{name}: text");
                // The same page: size, blocks, lines, words, boxes,
                // confidences and hyphens.
                assert_eq!(page_of(&new.alto), page_of(&old_alto), "{name}: layout");
                // The same scan named, with the same identifiers.
                for tag in ["fileName", "fileIdentifier"] {
                    assert_eq!(
                        elements(&new.alto, tag),
                        elements(&old_alto, tag),
                        "{name}: {tag}"
                    );
                }
                // The pickup takes it, as it took main's.
                usnm_ingest::ocr_local::checks::check_alto(new.alto.as_bytes()).unwrap();
                assert!(new
                    .alto
                    .contains("<softwareVersion>5.5.1</softwareVersion>"));
                assert!(new.alto.contains(&format!("ocr_engine={TAG}")));
            }
        }
    }

    /// The stored ALTO's settings name the engine's provenance and the
    /// page's run, in 13 §13.8.2's order.
    #[test]
    fn settings_carry_the_provenance() {
        let (_, xml, _) = fixtures().into_iter().next().unwrap();
        let page = usnm_alto::parse(&xml, &ParseOptions::default()).unwrap();
        let read = ReadInfo {
            image_ref: "ghcr.io/x/usnm-ocr/tesseract@sha256:3f9a".into(),
            models_json: r#"{"tessdata/eng.traineddata":"7af2"}"#.into(),
            options_json: r#"{"dpi":300,"psm":3}"#.into(),
            ..ReadInfo::local("tesseract-5.5")
        };
        let source = Source {
            url: URL,
            sha256: SHA,
            md5: None,
            run_id: Some("nuc1.20261020T140431123456Z"),
            damage_before: Some(0.25),
        };
        let now: DateTime<Utc> = "2026-10-20T14:04:31Z".parse().unwrap();
        let stored = store_from(
            page,
            "ALTO",
            TAG,
            Some(&read),
            "eng",
            300,
            &source,
            "0.1.0",
            now,
        );
        let settings = stored
            .alto
            .split_once("<processingStepSettings>")
            .and_then(|(_, s)| s.split_once("</processingStepSettings>"))
            .unwrap()
            .0;
        let keys: Vec<&str> = settings.split('\n').collect();
        assert_eq!(
            keys,
            [
                format!("ocr_engine={TAG}"),
                "image=ghcr.io/x/usnm-ocr/tesseract@sha256:3f9a".into(),
                "models=tessdata/eng.traineddata:sha256:7af2".into(),
                "profile=eng".into(),
                "options=dpi=300,psm=3".into(),
                "route=minimal".into(),
                "routing=r1".into(),
                "contract=0".into(),
                format!("adapter={}", read.adapter),
                "granularity=word".into(),
                "run_id=nuc1.20261020T140431123456Z".into(),
                format!("input_sha256={SHA}"),
                "dpi=300 (assumed)".into(),
                "damage_before=0.25".into(),
            ]
        );
        usnm_ingest::ocr_local::checks::check_alto(stored.alto.as_bytes()).unwrap();
    }

    /// An engine's ALTO with a word broken at a line end four ways: the
    /// dash in the word (`Haus—`), a dash printed as the line's `HYP`
    /// (`wei`, `—`), a `⸗` in the word and a `⸗` as a `HYP`, and an em dash
    /// before a capital, which is a column break.
    const BROKEN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">
  <Description><MeasurementUnit>pixel</MeasurementUnit></Description>
  <Layout><Page ID="p1" WIDTH="1000" HEIGHT="1000"><PrintSpace>
    <TextBlock ID="b1" HPOS="0" VPOS="0" WIDTH="500" HEIGHT="500">
      <TextLine ID="l1"><String CONTENT="engen"/><String CONTENT="Haus—"/></TextLine>
      <TextLine ID="l2"><String CONTENT="gang"/><String CONTENT="schritt"/></TextLine>
      <TextLine ID="l3"><String CONTENT="Abend"/><String CONTENT="wei"/><HYP CONTENT="—"/></TextLine>
      <TextLine ID="l4"><String CONTENT="ter"/><String CONTENT="aus"/></TextLine>
    </TextBlock>
    <TextBlock ID="b2" HPOS="0" VPOS="500" WIDTH="500" HEIGHT="500">
      <TextLine ID="l5"><String CONTENT="der"/><String CONTENT="Draht⸗"/></TextLine>
      <TextLine ID="l6"><String CONTENT="zaun"/><String CONTENT="und"/></TextLine>
      <TextLine ID="l7"><String CONTENT="Stahl"/><HYP CONTENT="⸗"/></TextLine>
      <TextLine ID="l8"><String CONTENT="werk"/><String CONTENT="Haus—"/></TextLine>
      <TextLine ID="l9"><String CONTENT="Cunow"/></TextLine>
    </TextBlock>
  </PrintSpace></Page></Layout>
</alto>"#;

    /// German pages and Fraktur models rejoin a word broken with a dash at a
    /// line end; other pages rejoin the `⸗` and leave the dash (#1065).
    #[test]
    fn an_em_dash_line_end_is_rejoined_for_german_and_fraktur_pages() {
        let source = Source {
            url: URL,
            sha256: SHA,
            md5: None,
            run_id: None,
            damage_before: None,
        };
        let now: DateTime<Utc> = "2026-10-20T14:04:31Z".parse().unwrap();
        let store = |tag: &str, language: &str| {
            from_engine_alto(BROKEN.as_bytes(), tag, language, 300, &source, "0.1.0", now)
                .unwrap()
                .text
        };
        let german = "engen Hausgang schritt\nAbend weiter aus\n\
            der Drahtzaun und\nStahlwerk Haus—\nCunow";
        let english = "engen Haus—\ngang schritt\nAbend wei—\nter aus\n\
            der Drahtzaun und\nStahlwerk Haus—\nCunow";
        let frak = TAG.replace("profile=eng", "profile=frak2021");
        // The Fraktur model, whatever the language it was routed for.
        assert_eq!(store(&frak, "deu"), german);
        assert_eq!(store(&frak, "eng"), german);
        // The German language, whatever the model.
        assert_eq!(store(TAG, "deu"), german);
        // Neither: only the `⸗` is a hyphen.
        assert_eq!(store(TAG, "eng"), english);
        // The older tag form names the model after the `/`.
        assert_eq!(store("tesseract-5.5.1/frak2021", "eng"), german);
        assert_eq!(store("tesseract-5.5.1/eng", "eng"), english);
    }

    #[test]
    fn engine_alto_that_doesnt_parse_is_a_convert_failure() {
        let source = Source {
            url: URL,
            sha256: SHA,
            md5: None,
            run_id: None,
            damage_before: None,
        };
        let now = Utc::now();
        assert!(from_engine_alto(b"<alto/>", TAG, "eng", 300, &source, "0.1.0", now).is_err());
        assert!(from_engine_alto(b"not xml", TAG, "eng", 300, &source, "0.1.0", now).is_err());
    }
}
