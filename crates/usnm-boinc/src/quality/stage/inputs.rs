//! Campaign places and language function-word inputs.

use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::path::Path;
use usnm_ocr_contract::quality::MAX_PLACES;

/// Reads a text file of one word per line into the job's list, folded.
pub fn read_words(path: &Path) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path:?}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(usnm_ocr_quality::consensus::fold)
        .collect())
}

/// The campaign's `places.txt`: from a `.txt` as it is, or from the
/// catalog's `places.json` (`usnm_ingest::catalog::Place` rows): every
/// place's name and its titles' variant names, distinct, sorted, one per
/// line.
pub fn read_places(path: &Path) -> anyhow::Result<String> {
    let bytes = read(path, MAX_PLACES).with_context(|| format!("reading {}", path.display()))?;
    if path.extension().is_some_and(|e| e == "json") {
        let places: Vec<usnm_ingest::catalog::Place> =
            serde_json::from_slice(&bytes).context("places.json")?;
        Ok(places_txt(&places))
    } else {
        let text = String::from_utf8(bytes).context("places.txt is not UTF-8")?;
        ensure!(
            text.lines().any(|l| !l.trim().is_empty()),
            "places.txt is empty"
        );
        Ok(text)
    }
}

pub fn places_txt(places: &[usnm_ingest::catalog::Place]) -> String {
    let mut names: Vec<&str> = places
        .iter()
        .flat_map(|p| {
            std::iter::once(p.name.as_str()).chain(p.variants.iter().map(|v| v.name.as_str()))
        })
        .map(str::trim)
        .filter(|n| !n.is_empty() && !n.chars().any(char::is_control))
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut out = String::new();
    for n in names {
        out.push_str(n);
        out.push('\n');
    }
    out
}

/// `<lang>.txt` for the campaign's languages: the first `n` words of the
/// language's embedded word list (wordfreq's `top_n_list`, the one
/// `usnm-ingest ocr-damage` scores with), as `ocr/ja-ocr/quality.py` takes
/// the 100 most frequent words for its function-word share. Languages
/// with no list (`usnm_core::lang::wordlist`) are an error.
pub fn write_function_words(langs: &[String], n: usize, out_dir: &Path) -> anyhow::Result<()> {
    ensure!(n > 0, "--top must be positive");
    std::fs::create_dir_all(out_dir)?;
    for lang in langs {
        crate::surya::check_word(lang, "language")?;
        let marc = usnm_core::lang::iso3_to_marc(lang)
            .with_context(|| format!("{lang} is not an ISO 639-3 code the catalog knows"))?;
        let code = usnm_core::lang::wordlist_code(marc)
            .with_context(|| format!("{lang} has no word list"))?;
        let list = usnm_core::lang::wordlist(code)
            .with_context(|| format!("{lang}: word list {code} is not embedded"))?;
        let mut text = format!("# {lang}: the {n} most frequent words of word list {code}\n");
        for w in list
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .take(n)
        {
            text.push_str(w);
            text.push('\n');
        }
        atomic(&out_dir.join(format!("{lang}.txt")), text.as_bytes())?;
    }
    Ok(())
}
