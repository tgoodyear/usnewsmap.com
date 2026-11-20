//! The stored ALTO profile (13 §13.8.2): ALTO 3.1 in pixels of the IIIF
//! full image, written from parsed values, never copied from an engine's
//! XML.

use std::collections::HashSet;
use std::fmt::Write as _;

use quick_xml::escape::escape;

use crate::model::{BlockKind, Granularity, Page, Rect, Word};

pub const NAMESPACE: &str = "http://www.loc.gov/standards/alto/ns-v3#";
pub const SCHEMA_LOCATION: &str = "http://www.loc.gov/standards/alto/v3/alto-3-1.xsd";
/// The ALTO v4 namespace and schema [`write_v4`] writes.
pub const NAMESPACE_V4: &str = "http://www.loc.gov/standards/alto/ns-v4#";
pub const SCHEMA_LOCATION_V4: &str = "http://www.loc.gov/standards/alto/v4/alto-4-4.xsd";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Software {
    pub creator: String,
    pub name: String,
    pub version: String,
}

/// One `ocrProcessingStep` or `postProcessingStep`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Step {
    /// `processingDateTime`, an `xsd:dateTime` such as
    /// `2026-10-20T14:04:31Z`.
    pub date_time: Option<String>,
    /// `processingStepSettings`, written as `key=value` lines in this order.
    pub settings: Vec<(String, String)>,
    pub software: Software,
}

/// The stored ALTO's `Description`, apart from its unit (always `pixel`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Description {
    /// `sourceImageInformation/fileName`: the URL of the image read.
    pub file_name: String,
    /// `fileIdentifier`s after it, as (`fileIdentifierLocation`, value):
    /// the sha256 of the bytes read and, for a batch's JP2, its bag md5.
    pub file_identifiers: Vec<(String, String)>,
    pub ocr: Step,
    pub post: Option<Step>,
}

/// Text safe in XML 1.0: control characters other than tab, newline and
/// carriage return, and U+FFFE and U+FFFF, can't appear even escaped.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            (c >= ' ' || matches!(c, '\t' | '\n' | '\r')) && c != '\u{FFFE}' && c != '\u{FFFF}'
        })
        .collect()
}

fn esc(s: &str) -> String {
    escape(clean(s).as_str()).into_owned()
}

/// A position in whole pixels.
fn px(x: f64) -> i64 {
    x.round() as i64
}

fn pos(r: &Rect) -> String {
    let (x0, y0) = (px(r.x0), px(r.y0));
    format!(
        "HPOS=\"{x0}\" VPOS=\"{y0}\" WIDTH=\"{}\" HEIGHT=\"{}\"",
        (px(r.x1) - x0).max(0),
        (px(r.y1) - y0).max(0)
    )
}

/// A confidence with at most four decimals and no trailing zeros.
fn conf(c: f64) -> String {
    let s = format!("{:.4}", c.clamp(0.0, 1.0));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.to_owned()
    }
}

fn step(out: &mut String, tag: &str, s: &Step) {
    let _ = writeln!(out, "      <{tag}>");
    if let Some(d) = &s.date_time {
        let _ = writeln!(
            out,
            "        <processingDateTime>{}</processingDateTime>",
            esc(d)
        );
    }
    if !s.settings.is_empty() {
        let lines: Vec<String> = s.settings.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = writeln!(
            out,
            "        <processingStepSettings>{}</processingStepSettings>",
            esc(&lines.join("\n"))
        );
    }
    let sw = &s.software;
    out.push_str("        <processingSoftware>\n");
    for (t, v) in [
        ("softwareCreator", &sw.creator),
        ("softwareName", &sw.name),
        ("softwareVersion", &sw.version),
    ] {
        if !v.is_empty() {
            let _ = writeln!(out, "          <{t}>{}</{t}>", esc(v));
        }
    }
    out.push_str("        </processingSoftware>\n");
    let _ = writeln!(out, "      </{tag}>");
}

/// The joined word for each word that is half of a hyphenated pair
/// (`SUBS_TYPE` and `SUBS_CONTENT`), indexed by line then word.
fn hyphen_parts(lines: &[&crate::model::Line]) -> Vec<Vec<Option<(&'static str, String)>>> {
    let mut parts: Vec<Vec<Option<(&'static str, String)>>> =
        lines.iter().map(|l| vec![None; l.words.len()]).collect();
    for i in 0..lines.len().saturating_sub(1) {
        let (a, b) = (&lines[i], &lines[i + 1]);
        let (Some(_), Some(last), Some(first)) = (&a.hyphen, a.words.last(), b.words.first())
        else {
            continue;
        };
        if last.text.is_empty() || first.text.is_empty() {
            continue;
        }
        let joined = format!("{}{}", last.text, first.text);
        let (la, lb) = (a.words.len() - 1, 0);
        parts[i + 1][lb] = Some(("HypPart2", joined.clone()));
        parts[i][la] = Some(("HypPart1", joined));
    }
    parts
}

/// The stored ALTO for `page`: LANG on each TextBlock is the block's
/// language or else `language` (ISO 639-3). Positions are whole pixels.
/// Geometry finer than `page.granularity` is the parent's box, as the
/// reading gave it; the settings say which granularity that is.
pub fn write(page: &Page, desc: &Description, language: &str) -> String {
    let mut out = String::with_capacity(4096 + page.blocks.len() * 512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<alto xmlns=\"{NAMESPACE}\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"{NAMESPACE} {SCHEMA_LOCATION}\">"
    );
    out.push_str("  <Description>\n    <MeasurementUnit>pixel</MeasurementUnit>\n");
    out.push_str("    <sourceImageInformation>\n");
    let _ = writeln!(out, "      <fileName>{}</fileName>", esc(&desc.file_name));
    for (loc, v) in &desc.file_identifiers {
        let _ = writeln!(
            out,
            "      <fileIdentifier fileIdentifierLocation=\"{}\">{}</fileIdentifier>",
            esc(loc),
            esc(v)
        );
    }
    out.push_str("    </sourceImageInformation>\n");
    out.push_str("    <OCRProcessing ID=\"OCR_0\">\n");
    step(&mut out, "ocrProcessingStep", &desc.ocr);
    if let Some(p) = &desc.post {
        step(&mut out, "postProcessingStep", p);
    }
    out.push_str("    </OCRProcessing>\n  </Description>\n");
    layout(&mut out, page, language, &[], HashSet::new());
    out
}

/// A processing step of a [`write_v4`] page: an ALTO v4 `Processing` element, which blocks
/// name in `PROCESSINGREFS`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Processing {
    /// The `ID` the blocks refer to (an XML name).
    pub id: String,
    /// `processingCategory`: `contentGeneration`, `contentModification`, `preOperation`,
    /// `postOperation` or `other`.
    pub category: String,
    pub step: Step,
}

/// A block's own `ID` and the `Processing` it came from, for [`write_v4`]. Notes are given by
/// position in `Page::blocks`; a block with none gets `TB<n>` and no `PROCESSINGREFS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockNote {
    pub id: String,
    pub processing: String,
}

/// ALTO v4 in the same pixel profile as [`write`], with per-block provenance: a `Processing`
/// element for each of `steps` in the `Description`, and a `PROCESSINGREFS` on every block that
/// has a note.
pub fn write_v4(
    page: &Page,
    file_name: &str,
    file_identifiers: &[(String, String)],
    steps: &[Processing],
    notes: &[Option<BlockNote>],
    language: &str,
) -> String {
    let mut out = String::with_capacity(4096 + page.blocks.len() * 512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<alto xmlns=\"{NAMESPACE_V4}\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"{NAMESPACE_V4} {SCHEMA_LOCATION_V4}\">"
    );
    out.push_str("  <Description>\n    <MeasurementUnit>pixel</MeasurementUnit>\n");
    out.push_str("    <sourceImageInformation>\n");
    let _ = writeln!(out, "      <fileName>{}</fileName>", esc(file_name));
    for (loc, v) in file_identifiers {
        let _ = writeln!(
            out,
            "      <fileIdentifier fileIdentifierLocation=\"{}\">{}</fileIdentifier>",
            esc(loc),
            esc(v)
        );
    }
    out.push_str("    </sourceImageInformation>\n");
    for p in steps {
        let _ = writeln!(out, "    <Processing ID=\"{}\">", esc(&p.id));
        let _ = writeln!(
            out,
            "      <processingCategory>{}</processingCategory>",
            esc(&p.category)
        );
        if let Some(d) = &p.step.date_time {
            let _ = writeln!(
                out,
                "      <processingDateTime>{}</processingDateTime>",
                esc(d)
            );
        }
        // At most one element (processingStepType), the settings as `key=value` lines.
        if !p.step.settings.is_empty() {
            let lines: Vec<String> = p
                .step
                .settings
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let _ = writeln!(
                out,
                "      <processingStepSettings>{}</processingStepSettings>",
                esc(&lines.join("\n"))
            );
        }
        let sw = &p.step.software;
        out.push_str("      <processingSoftware>\n");
        for (t, v) in [
            ("softwareCreator", &sw.creator),
            ("softwareName", &sw.name),
            ("softwareVersion", &sw.version),
        ] {
            if !v.is_empty() {
                let _ = writeln!(out, "        <{t}>{}</{t}>", esc(v));
            }
        }
        out.push_str("      </processingSoftware>\n    </Processing>\n");
    }
    out.push_str("  </Description>\n");
    // Supplied IDs (the blocks' and the steps') are kept as they are; every ID the writer
    // makes avoids them, so no ID is used twice in the document.
    let reserved: HashSet<String> = notes
        .iter()
        .flatten()
        .map(|n| n.id.clone())
        .chain(steps.iter().map(|p| p.id.clone()))
        .collect();
    layout(&mut out, page, language, notes, reserved);
    out
}

/// IDs in use in a document: a made one is the base when free, else the base with `_<n>`.
struct Ids(HashSet<String>);

impl Ids {
    fn take(&mut self, base: String) -> String {
        let mut id = base.clone();
        let mut n = 2;
        while !self.0.insert(id.clone()) {
            id = format!("{base}_{n}");
            n += 1;
        }
        id
    }
}

/// The `Layout`, to the end of the document.
fn layout(
    out: &mut String,
    page: &Page,
    language: &str,
    notes: &[Option<BlockNote>],
    reserved: HashSet<String>,
) {
    let mut ids = Ids(reserved);
    out.push_str("  <Layout>\n");
    let full = Rect::new(0.0, 0.0, page.width, page.height);
    let (w, h) = (px(page.width), px(page.height));
    let _ = writeln!(
        out,
        "    <Page ID=\"{}\" PHYSICAL_IMG_NR=\"1\" WIDTH=\"{w}\" HEIGHT=\"{h}\">",
        esc(&ids.take("P1".to_owned()))
    );
    let _ = writeln!(out, "      <PrintSpace {}>", pos(&full));
    let (mut nb, mut nl, mut ns) = (0usize, 0usize, 0usize);
    let lang = esc(language);
    for b in &page.blocks {
        nb += 1;
        match b.kind {
            BlockKind::Illustration => {
                let _ = writeln!(
                    out,
                    "        <Illustration ID=\"{}\" {}/>",
                    esc(&ids.take(format!("IL{nb}"))),
                    pos(&b.rect)
                );
                continue;
            }
            BlockKind::Separator => {
                let _ = writeln!(
                    out,
                    "        <GraphicalElement ID=\"{}\" {}/>",
                    esc(&ids.take(format!("GE{nb}"))),
                    pos(&b.rect)
                );
                continue;
            }
            _ => {}
        }
        let owned = crate::model::stored_lines(b);
        let lines: Vec<&crate::model::Line> = owned.iter().collect();
        if lines.is_empty() {
            continue;
        }
        let block_lang = b.lang.as_deref().map(esc).unwrap_or_else(|| lang.clone());
        let note = notes.get(nb - 1).and_then(Option::as_ref);
        // A supplied block ID is already reserved; one the writer makes is taken fresh.
        let id = note.map_or_else(|| esc(&ids.take(format!("TB{nb}"))), |n| esc(&n.id));
        let refs = note.map_or_else(String::new, |n| {
            format!(" PROCESSINGREFS=\"{}\"", esc(&n.processing))
        });
        let _ = writeln!(
            out,
            "        <TextBlock ID=\"{id}\" {}{refs} LANG=\"{block_lang}\">",
            pos(&b.rect)
        );
        let parts = hyphen_parts(&lines);
        for (li, l) in lines.iter().enumerate() {
            nl += 1;
            let _ = writeln!(
                out,
                "          <TextLine ID=\"{}\" {}>",
                esc(&ids.take(format!("TL{nl}"))),
                pos(&l.rect)
            );
            for (wi, wd) in l.words.iter().enumerate() {
                ns += 1;
                if wi > 0 {
                    out.push_str(&space(&l.words[wi - 1], wd, page.granularity));
                }
                let _ = write!(
                    out,
                    "            <String ID=\"{}\" {}",
                    esc(&ids.take(format!("ST{ns}"))),
                    pos(&wd.rect)
                );
                if let Some(c) = wd.conf {
                    let _ = write!(out, " WC=\"{}\"", conf(c));
                }
                if let Some((t, s)) = &parts[li][wi] {
                    let _ = write!(out, " SUBS_TYPE=\"{t}\" SUBS_CONTENT=\"{}\"", esc(s));
                }
                let _ = writeln!(out, " CONTENT=\"{}\"/>", esc(&wd.text));
            }
            if let Some(hy) = &l.hyphen {
                let _ = writeln!(out, "            <HYP CONTENT=\"{}\"/>", esc(hy));
            }
            out.push_str("          </TextLine>\n");
        }
        out.push_str("        </TextBlock>\n");
    }
    out.push_str("      </PrintSpace>\n    </Page>\n  </Layout>\n</alto>\n");
}

/// The `SP` between two words: with its position between them when words
/// have their own boxes and there is a gap.
fn space(prev: &Word, next: &Word, g: Granularity) -> String {
    let (a, b) = (px(prev.rect.x1), px(next.rect.x0));
    if g == Granularity::Word && b > a {
        format!(
            "            <SP HPOS=\"{a}\" VPOS=\"{}\" WIDTH=\"{}\"/>\n",
            px(prev.rect.y0),
            b - a
        )
    } else {
        "            <SP/>\n".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_names_each_block_s_processing_step() {
        use crate::model::{Block, Line};
        let word = |t: &str| Word {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            text: t.to_owned(),
            conf: None,
        };
        let block = |kind| Block {
            kind,
            rect: Rect::new(0.0, 0.0, 100.0, 20.0),
            lang: None,
            lines: vec![Line {
                rect: Rect::new(0.0, 0.0, 100.0, 20.0),
                words: vec![word("a"), word("b")],
                hyphen: None,
            }],
        };
        let page = Page {
            width: 200.0,
            height: 300.0,
            granularity: Granularity::Line,
            blocks: vec![block(BlockKind::Text), block(BlockKind::Text)],
        };
        let step = |name: &str| Processing {
            id: format!("PROC_{name}"),
            category: "contentGeneration".into(),
            step: Step {
                date_time: None,
                settings: vec![("k".into(), "v".into())],
                software: Software {
                    creator: "c".into(),
                    name: name.into(),
                    version: "1".into(),
                },
            },
        };
        let notes = vec![
            Some(BlockNote {
                id: "B1.1".into(),
                processing: "PROC_X".into(),
            }),
            None,
        ];
        let xml = write_v4(
            &page,
            "https://tile.loc.gov/x",
            &[("sha256".into(), "ab".into())],
            &[step("X"), step("L")],
            &notes,
            "eng",
        );
        assert!(xml.contains("ns-v4#"));
        assert!(xml.contains("<Processing ID=\"PROC_X\">"));
        assert!(xml.contains("<Processing ID=\"PROC_L\">"));
        assert!(xml.contains("ID=\"B1.1\" HPOS=\"0\" VPOS=\"0\" WIDTH=\"100\" HEIGHT=\"20\" PROCESSINGREFS=\"PROC_X\""));
        // A block with no note keeps the stored profile's id and has no reference.
        assert!(xml.contains(
            "<TextBlock ID=\"TB2\" HPOS=\"0\" VPOS=\"0\" WIDTH=\"100\" HEIGHT=\"20\" LANG=\"eng\">"
        ));
        assert_eq!(xml.matches("PROCESSINGREFS").count(), 1);
        // The bounded reader and the parser both take it.
        let checked = crate::check(xml.as_bytes(), &crate::Limits::default()).unwrap();
        assert_eq!(checked.version, crate::AltoVersion::V4);
        let parsed = crate::parse(xml.as_bytes(), &crate::ParseOptions::default()).unwrap();
        assert_eq!(parsed.blocks.len(), 2);
        // `write` is unchanged: same blocks, no references.
        let v3 = write(&page, &Description::default(), "eng");
        assert!(!v3.contains("PROCESSINGREFS") && v3.contains("ID=\"TB1\""));
    }

    #[test]
    fn ids_the_writer_makes_avoid_every_supplied_id() {
        use crate::model::{Block, Line};
        let line = || Line {
            rect: Rect::new(0.0, 0.0, 100.0, 20.0),
            words: vec![Word {
                rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                text: "a".into(),
                conf: None,
            }],
            hyphen: None,
        };
        let block = |kind| Block {
            kind,
            rect: Rect::new(0.0, 0.0, 100.0, 20.0),
            lang: None,
            lines: vec![line()],
        };
        let page = Page {
            width: 200.0,
            height: 300.0,
            granularity: Granularity::Line,
            blocks: vec![
                block(BlockKind::Text),
                block(BlockKind::Text),
                block(BlockKind::Illustration),
            ],
        };
        // Supplied block ids that the writer would otherwise make for a line, a word, the
        // page and an illustration, and a step called like a block.
        let notes = vec![
            Some(BlockNote {
                id: "TL1".into(),
                processing: "TL2".into(),
            }),
            Some(BlockNote {
                id: "ST1".into(),
                processing: "P1".into(),
            }),
            Some(BlockNote {
                id: "IL3".into(),
                processing: "P1".into(),
            }),
        ];
        let step = Processing {
            id: "TL2".into(),
            category: "contentGeneration".into(),
            step: Step {
                date_time: None,
                settings: vec![],
                software: Software {
                    creator: "c".into(),
                    name: "n".into(),
                    version: "1".into(),
                },
            },
        };
        let xml = write_v4(&page, "f", &[], &[step], &notes, "eng");
        let ids: Vec<&str> = xml
            .split(" ID=\"")
            .skip(1)
            .filter_map(|t| t.split('"').next())
            .collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "{ids:?}");
        // Supplied ids stay as given.
        assert!(xml.contains("<TextBlock ID=\"ST1\""));
    }

    #[test]
    fn numbers() {
        assert_eq!(conf(0.96), "0.96");
        assert_eq!(conf(1.0), "1");
        assert_eq!(conf(0.0), "0");
        assert_eq!(conf(0.123456), "0.1235");
        assert_eq!(
            pos(&Rect::new(1.4, 2.6, 10.5, 3.0)),
            "HPOS=\"1\" VPOS=\"3\" WIDTH=\"10\" HEIGHT=\"0\""
        );
        assert_eq!(clean("a\u{1}b\tc\u{FFFF}"), "ab\tc");
    }
}
