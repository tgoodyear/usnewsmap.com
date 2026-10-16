//! The `ja-ocr/alto.py` profile: line geometry, NDLOCR reading order and
//! ALTO 3.1 in inch1200 (one String per line, not fabricated character boxes).
use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{bail, ensure, Context};
use quick_xml::{events::Event, Reader, XmlVersion};

#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub dpi: u32,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
impl Default for Geometry {
    fn default() -> Self {
        Self {
            dpi: 300,
            width: None,
            height: None,
        }
    }
}

#[derive(Debug)]
struct Line {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    order: f64,
    conf: f64,
    text: String,
}

fn line(attrs: &BTreeMap<String, String>) -> Option<Line> {
    let number = |key: &str, default: Option<&str>| {
        let n = attrs
            .get(key)
            .map(String::as_str)
            .or(default)?
            .parse::<f64>()
            .ok()?;
        n.is_finite().then_some(n)
    };
    let text = attrs.get("STRING")?.trim().to_owned();
    let line = Line {
        x: number("X", None)?,
        y: number("Y", None)?,
        w: number("WIDTH", None)?,
        h: number("HEIGHT", None)?,
        order: number("ORDER", Some("0"))?,
        conf: number("CONF", Some("0"))?,
        text,
    };
    (!line.text.is_empty() && line.w > 0.0 && line.h > 0.0).then_some(line)
}

struct Parsed {
    size: (u32, u32),
    blocks: Vec<Vec<Line>>,
}

fn blocks(xml: &[u8]) -> anyhow::Result<Parsed> {
    ensure!(xml.len() <= 64 << 20, "NDLOCR XML over limit");
    let mut reader = Reader::from_str(std::str::from_utf8(xml)?);
    let mut stack: Vec<String> = Vec::new();
    let mut size = None;
    let mut page_depth = None;
    let mut current = Vec::new();
    let mut blocks = Vec::new();
    let mut elements = 0;
    let mut root_seen = false;
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                elements += 1;
                ensure!(
                    elements <= 2_000_000 && stack.len() < 32,
                    "NDLOCR XML complexity limit"
                );
                let mut attrs = BTreeMap::new();
                for attr in e.attributes() {
                    let a = attr?;
                    ensure!(attrs.len() < 32, "NDLOCR attribute limit");
                    attrs.insert(
                        a.key.as_ref().to_owned(),
                        a.normalized_value(XmlVersion::Implicit1_0)?.into_owned(),
                    );
                }
                let name = e.name();
                if stack.is_empty() {
                    ensure!(
                        !root_seen && name.as_ref() == "OCRDATASET",
                        "expected one OCRDATASET root"
                    );
                    root_seen = true;
                }
                if name.as_ref() == "PAGE" {
                    ensure!(
                        size.is_none() && stack.as_slice() == ["OCRDATASET".to_owned()],
                        "expected one NDLOCR PAGE"
                    );
                    let width = attrs
                        .get("WIDTH")
                        .context("no page width")?
                        .parse::<u32>()?;
                    let height = attrs
                        .get("HEIGHT")
                        .context("no page height")?
                        .parse::<u32>()?;
                    ensure!(width > 0 && height > 0, "empty page geometry");
                    size = Some((width, height));
                    page_depth = Some(stack.len());
                }
                if name.as_ref() == "LINE" && page_depth.is_some() {
                    if let Some(ln) = line(&attrs) {
                        current.push(ln);
                    }
                }
                if matches!(event, Event::Empty(_)) {
                    if page_depth.is_some_and(|d| stack.len() == d + 1) && !current.is_empty() {
                        current.sort_by(|a, b| a.order.total_cmp(&b.order));
                        blocks.push(std::mem::take(&mut current));
                    }
                    if page_depth == Some(stack.len()) {
                        page_depth = None;
                    }
                } else {
                    stack.push(name.as_ref().to_owned());
                }
            }
            Event::End(_) => {
                stack.pop().context("unbalanced NDLOCR XML")?;
                if page_depth.is_some_and(|d| stack.len() == d + 1) && !current.is_empty() {
                    current.sort_by(|a, b| a.order.total_cmp(&b.order));
                    blocks.push(std::mem::take(&mut current));
                }
                if page_depth == Some(stack.len()) {
                    page_depth = None;
                }
            }
            Event::DocType(_) | Event::PI(_) => bail!("NDLOCR XML declarations are not allowed"),
            Event::Text(ref t) if stack.is_empty() => {
                ensure!(
                    t.xml_content(XmlVersion::Implicit1_0).trim().is_empty(),
                    "text outside root"
                )
            }
            Event::Eof => break,
            _ => (),
        }
    }
    ensure!(stack.is_empty(), "unclosed NDLOCR XML");
    blocks.sort_by(|a, b| a[0].order.total_cmp(&b[0].order));
    Ok(Parsed {
        size: size.context("NDLOCR XML has no PAGE")?,
        blocks,
    })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn quote(s: &str) -> String {
    let s = escape(s)
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;");
    if s.contains('"') && !s.contains('\'') {
        format!("'{s}'")
    } else {
        format!("\"{}\"", s.replace('"', "&quot;"))
    }
}

/// Same ALTO bytes as `alto.ndlocr_to_alto`, for the same geometry and metadata.
/// Timestamp is an ISO-8601 value supplied by the caller (no clock in conversion).
pub fn ndlocr_to_alto(
    xml: &[u8],
    geometry: Geometry,
    image_url: &str,
    engine: &str,
    seq: u16,
    ocred_at: &str,
) -> anyhow::Result<Vec<u8>> {
    ensure!(geometry.dpi > 0, "zero dpi");
    let Parsed {
        size: (pw, ph),
        blocks,
    } = blocks(xml)?;
    let k = 1200.0 / f64::from(geometry.dpi);
    let width = geometry
        .width
        .filter(|v| *v > 0)
        .map(f64::from)
        .unwrap_or((f64::from(pw) * k).round_ties_even());
    let height = geometry
        .height
        .filter(|v| *v > 0)
        .map(f64::from)
        .unwrap_or((f64::from(ph) * k).round_ties_even());
    let bbox = |x: f64, y: f64, w: f64, h: f64| {
        let x0 = (x * k).round_ties_even().min(width);
        let y0 = (y * k).round_ties_even().min(height);
        let x1 = ((x + w) * k).round_ties_even().min(width);
        let y1 = ((y + h) * k).round_ties_even().min(height);
        format!(
            "HPOS=\"{x0:.0}\" VPOS=\"{y0:.0}\" WIDTH=\"{:.0}\" HEIGHT=\"{:.0}\"",
            x1 - x0,
            y1 - y0
        )
    };
    let mut out = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<alto xmlns=\"http://www.loc.gov/standards/alto/ns-v3#\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"http://www.loc.gov/standards/alto/ns-v3# http://www.loc.gov/standards/alto/v3/alto-3-1.xsd\">\n<Description>\n<MeasurementUnit>inch1200</MeasurementUnit>\n<sourceImageInformation><fileName>{}</fileName></sourceImageInformation>\n<OCRProcessing ID=\"OCR_0\"><ocrProcessingStep>\n<processingDateTime>{}</processingDateTime>\n<processingStepSettings>dpi:{}\nlines:NDLOCR-Lite reading order\nstrings:one per line</processingStepSettings>\n<processingSoftware><softwareCreator>National Diet Library</softwareCreator><softwareName>NDLOCR-Lite</softwareName><softwareVersion>{}</softwareVersion></processingSoftware>\n</ocrProcessingStep></OCRProcessing>\n</Description>\n<Layout>\n<Page ID=\"P{seq}\" PHYSICAL_IMG_NR=\"{seq}\" WIDTH=\"{width:.0}\" HEIGHT=\"{height:.0}\">\n<PrintSpace HPOS=\"0\" VPOS=\"0\" WIDTH=\"{width:.0}\" HEIGHT=\"{height:.0}\">\n", escape(image_url), escape(ocred_at), geometry.dpi, escape(engine));
    let mut n = 0;
    for (b, lines) in blocks.iter().enumerate() {
        let x0 = lines.iter().map(|l| l.x).fold(f64::INFINITY, f64::min);
        let y0 = lines.iter().map(|l| l.y).fold(f64::INFINITY, f64::min);
        let x1 = lines
            .iter()
            .map(|l| l.x + l.w)
            .fold(f64::NEG_INFINITY, f64::max);
        let y1 = lines
            .iter()
            .map(|l| l.y + l.h)
            .fold(f64::NEG_INFINITY, f64::max);
        writeln!(
            out,
            "<TextBlock ID=\"P{seq}_TB{:05}\" {} LANG=\"jpn\">",
            b + 1,
            bbox(x0, y0, x1 - x0, y1 - y0)
        )?;
        for ln in lines {
            n += 1;
            let attrs = bbox(ln.x, ln.y, ln.w, ln.h);
            writeln!(out,"<TextLine ID=\"P{seq}_TL{n:05}\" {attrs}><String ID=\"P{seq}_ST{n:05}\" {attrs} CONTENT={} WC=\"{:.3}\"/></TextLine>", quote(&ln.text), ln.conf.clamp(0.0,1.0))?;
        }
        out.push_str("</TextBlock>\n");
    }
    out.push_str("</PrintSpace>\n</Page>\n</Layout>\n</alto>\n");
    Ok(out.into_bytes())
}
