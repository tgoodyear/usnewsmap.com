//! The fixture indexes with one searched field for all the texts (05 §5.5.7,
//! #283, #925), for scripts/dev/quickwit-fixtures.sh and the parity test:
//!
//! - `single_field_fixture --template`: the index template on stdin, with
//!   the text fields of `TextLayout::Single`, as a release creates the index.
//! - `single_field_fixture --docs`: fixture documents on stdin, with
//!   `text_all` and `text_all_cg` as a release writes them
//!   (`usnm_core::text_layout::index_fields`); `text` and `text_as` stay.
//! - Both with `--loc-original`: the fields of `TextLayout::SingleWithLoc`.
//!   On the pages `fixtures/loc_original.rs` says our OCR replaced LoC's text
//!   on, `text` is ours and `text_loc` LoC's original, and `text_all` holds
//!   the page's three texts (`usnm_core::text_layout::index_texts`).

use std::io::{self, BufRead, Read, Write};

use usnm_core::text::Analyzer;
use usnm_core::text_layout::{self, TextLayout};

#[path = "../../../fixtures/loc_original.rs"]
mod loc_original;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--template"] => template(TextLayout::Single, &mut out)?,
        ["--template", "--loc-original"] => template(TextLayout::SingleWithLoc, &mut out)?,
        ["--docs"] => docs(TextLayout::Single, &mut out)?,
        ["--docs", "--loc-original"] => docs(TextLayout::SingleWithLoc, &mut out)?,
        _ => anyhow::bail!("usage: single_field_fixture (--template | --docs) [--loc-original]"),
    }
    out.flush()?;
    Ok(())
}

/// The index template on stdin with the text fields of `layout`.
fn template(layout: TextLayout, out: &mut impl Write) -> anyhow::Result<()> {
    let mut template = String::new();
    io::stdin().read_to_string(&mut template)?;
    let yaml = usnm_ingest::sink::text_fields(layout, &template)?;
    out.write_all(yaml.as_bytes())?;
    Ok(())
}

/// The fixture documents on stdin, as a release lays out `layout`.
fn docs(layout: TextLayout, out: &mut impl Write) -> anyhow::Result<()> {
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut doc: serde_json::Value = serde_json::from_str(&line)?;
        let (words, pairs) = if layout.keeps_loc_original() {
            loc_original::add(&mut doc);
            text_layout::index_texts(
                [doc["text"].as_str().unwrap_or_default()]
                    .into_iter()
                    .chain(doc["text_as"].as_str())
                    .chain(doc["text_loc"].as_str()),
                Analyzer::LATEST,
            )
        } else {
            text_layout::index_fields(
                doc["text"].as_str().unwrap_or_default(),
                doc["text_as"].as_str(),
                Analyzer::LATEST,
            )
        };
        doc[text_layout::FIELD] = words.into();
        doc[text_layout::PAIRS_FIELD] = pairs.into();
        serde_json::to_writer(&mut *out, &doc)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}
