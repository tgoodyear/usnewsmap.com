//! Adds `text_cg` (05 §5.5.3) to index documents: JSON lines on stdin, each
//! with a `text`, to stdout with `text_cg` computed as the release does, and
//! `text_as_cg` for those with American Stories' `text_as` (05 §5.5.4).
//! scripts/quickwit-fixtures.sh loads the fixture indexes through it.

use std::io::{self, BufRead, Write};

fn main() -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut doc: serde_json::Value = serde_json::from_str(&line)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(text) = doc["text"].as_str() {
            doc["text_cg"] = usnm_core::common_grams::index_text(text).into();
        }
        // American Stories' text (05 §5.5.4) gets its own pairs.
        if let Some(text) = doc["text_as"].as_str() {
            doc["text_as_cg"] = usnm_core::common_grams::index_text(text).into();
        }
        serde_json::to_writer(&mut out, &doc)?;
        out.write_all(b"\n")?;
    }
    out.flush()
}
