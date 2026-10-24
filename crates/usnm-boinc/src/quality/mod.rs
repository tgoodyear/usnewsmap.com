//! The OCR quality estimate (#522) as a BOINC app, `usnm_quality`: one page
//! per unit, every text we hold of it compared on a volunteer CPU.
//!
//! - [`sample`] draws the campaign's pages from the prefetch manifest.
//! - [`stage`] fetches each page's JPEG and ALTO from LoC (or reads them
//!   from the hand-off container), joins the texts exported for it, and
//!   writes one unit directory per page.
//! - [`submit`] hands staged units to stock `create_work`, two results each.
//! - [`work`] runs one unit on a volunteer host and always writes a result,
//!   its confusion pairs and its diagnostics.
//! - [`validate`] is what stock `script_validator` calls: `check` keeps a
//!   copy of every result, `compare` requires the canonical records equal
//!   and logs every pair.
//! - [`collect`] joins this app's and `usnm_qwen`'s canonical results per
//!   page into the campaign's Parquet tables.
//! - [`estimate`] runs the same computation offline on a directory of
//!   per-page inputs; [`calibrate`] runs it on gold crops with a
//!   transcription and reports the estimator's error.
//! - [`summary`] reads the validator's copies back into one table.
use std::path::{Path, PathBuf};
pub use usnm_ocr_quality::slot::{
    inputs, outputs, parse_alto, ALTO_FILE, CONFUSIONS_FILE, DIAG_FILE, JOB_FILE, PAGE_FILE,
    PLACES_FILE, RESULT_FILE, TEXTS_FILE,
};

pub mod calibrate;
pub mod cli;
pub mod collect;
pub mod estimate;
pub mod sample;
pub mod stage;
pub mod submit;
pub mod summary;
pub mod validate;
pub mod work;

pub const APP: &str = "usnm_quality";

/// One page to stage, as the Surya app names it: `group:lang:doc_id`.
pub use crate::surya::Page as PageSpec;

/// Where the units live under the staging root.
pub fn units(root: &Path) -> PathBuf {
    root.join("units")
}

pub fn check_no_hold(root: &Path) -> anyhow::Result<()> {
    crate::assess::check_no_hold(root)
}

/// A small ALTO v3 page in pixels, for tests and local checks: one block
/// per element of `blocks`, each a list of lines, each a list of
/// `(x0, y0, x1, y1, text, wc)` words.
#[allow(clippy::type_complexity)]
pub fn alto_xml(
    width: u32,
    height: u32,
    blocks: &[Vec<Vec<(u32, u32, u32, u32, &str, f64)>>],
) -> String {
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<alto xmlns=\"http://www.loc.gov/standards/alto/ns-v3#\"><Description><MeasurementUnit>pixel</MeasurementUnit></Description><Layout><Page ID=\"P1\" WIDTH=\"{width}\" HEIGHT=\"{height}\" PHYSICAL_IMG_NR=\"1\"><PrintSpace HPOS=\"0\" VPOS=\"0\" WIDTH=\"{width}\" HEIGHT=\"{height}\">"
    );
    for (bi, lines) in blocks.iter().enumerate() {
        let all: Vec<&(u32, u32, u32, u32, &str, f64)> = lines.iter().flatten().collect();
        let bx0 = all.iter().map(|w| w.0).min().unwrap_or(0);
        let by0 = all.iter().map(|w| w.1).min().unwrap_or(0);
        let bx1 = all.iter().map(|w| w.2).max().unwrap_or(0);
        let by1 = all.iter().map(|w| w.3).max().unwrap_or(0);
        s.push_str(&format!(
            "<TextBlock ID=\"B{bi}\" HPOS=\"{bx0}\" VPOS=\"{by0}\" WIDTH=\"{}\" HEIGHT=\"{}\">",
            bx1 - bx0,
            by1 - by0
        ));
        for (li, words) in lines.iter().enumerate() {
            let lx0 = words.iter().map(|w| w.0).min().unwrap_or(0);
            let ly0 = words.iter().map(|w| w.1).min().unwrap_or(0);
            let lx1 = words.iter().map(|w| w.2).max().unwrap_or(0);
            let ly1 = words.iter().map(|w| w.3).max().unwrap_or(0);
            s.push_str(&format!(
                "<TextLine ID=\"B{bi}L{li}\" HPOS=\"{lx0}\" VPOS=\"{ly0}\" WIDTH=\"{}\" HEIGHT=\"{}\">",
                lx1 - lx0,
                ly1 - ly0
            ));
            for (wi, (x0, y0, x1, y1, text, wc)) in words.iter().enumerate() {
                let text = text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('"', "&quot;");
                s.push_str(&format!(
                    "<String ID=\"B{bi}L{li}W{wi}\" HPOS=\"{x0}\" VPOS=\"{y0}\" WIDTH=\"{}\" HEIGHT=\"{}\" CONTENT=\"{text}\" WC=\"{wc:.2}\"/>",
                    x1 - x0,
                    y1 - y0
                ));
                if wi + 1 < words.len() {
                    s.push_str("<SP/>");
                }
            }
            s.push_str("</TextLine>");
        }
        s.push_str("</TextBlock>");
    }
    s.push_str("</PrintSpace></Page></Layout></alto>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alto_round_trip_and_rescale() {
        let xml = alto_xml(
            1000,
            2000,
            &[vec![vec![
                (10, 10, 100, 40, "one", 0.9),
                (110, 10, 200, 40, "two", 0.8),
            ]]],
        );
        let page = parse_alto(xml.as_bytes(), 2000.0, 4000.0).unwrap();
        assert_eq!(page.blocks.len(), 1);
        assert_eq!(page.blocks[0].lines[0].words[1].text, "two");
        assert_eq!(page.blocks[0].lines[0].words[1].rect.x0, 220.0);
        assert_eq!(usnm_alto::text(&page), "one two");
        // LoC's 1/1200 inch units read without a dpi and scale by the page size.
        let inch = xml.replace("pixel", "inch1200").replace(
            "WIDTH=\"1000\" HEIGHT=\"2000\"",
            "WIDTH=\"12000\" HEIGHT=\"24000\"",
        );
        let page = parse_alto(inch.as_bytes(), 1000.0, 2000.0).unwrap();
        assert!((page.blocks[0].lines[0].words[0].rect.x1 - 100.0 / 12.0).abs() < 1e-9);
        assert!(parse_alto(b"<x/>", 1.0, 1.0).is_err());
    }
}
