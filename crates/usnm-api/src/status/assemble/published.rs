//! Published reference counts and source summaries.

use super::percent;
use crate::refdata::RefData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use usnm_core::{ids::PageKey, names};
use usnm_state::state::MAX_DELTAS;

/// The version the API is serving, from its reference data (always available).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Published {
    pub index_version: String,
    pub published_at: String,
    pub synthetic: bool,
    pub pages: u64,
    pub titles: usize,
    pub places: usize,
    pub bounds: crate::refdata::Bounds,
    /// Base index first, then its deltas.
    pub indexes: Vec<String>,
    pub deltas: usize,
    /// Deltas a version may carry; the release after that many is a full rebuild.
    pub max_deltas: usize,
    pub next_release_full: bool,
    /// Batches the version was built from, when its snapshot records them.
    pub batches: Option<usize>,
    /// Copies of pages that also ship in another batch, counted once in
    /// `pages` (04 §4.7); `None` when the snapshot doesn't record them.
    pub duplicate_pages: Option<u64>,
    /// Pages by state or territory, most first.
    pub by_state: Vec<StatePages>,
    pub by_language: ByLanguage,
    /// The Japanese pages we OCR (#139), when the version has their index.
    pub ja: Option<crate::refdata::JaIndexes>,
    /// Pages by year of publication, oldest first; years without pages are
    /// left out.
    pub by_year: Vec<YearPages>,
    /// The texts searched besides LoC's OCR, as the version holds them.
    pub sources: Sources,
}

/// One year's published pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct YearPages {
    pub year: i32,
    pub pages: u64,
}

/// The texts the version searches besides LoC's own OCR, each `None` when
/// the version has none of it.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Sources {
    /// Attribution frozen with the serving version; absent on old releases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_credits: Option<serde_json::Value>,
    /// Union of Japanese-index and main-index Latin OCR pages, per engine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_ja: Option<OcrLocalSummary>,
    /// American Stories' text (04 §4.9).
    pub american_stories: Option<AmericanStories>,
    /// Our OCR's Latin-script text of Japanese titles' pages, in the main
    /// index (#203, 04 §4.8).
    pub ja_latin: Option<JaLatinPages>,
    /// Our own OCR's text in place of LoC's (12 §12.5 Step 9).
    pub ocr_local: Option<OcrLocalSummary>,
}

/// American Stories' text in the version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AmericanStories {
    /// Searches cover it (`USNM_AMERICAN_STORIES_SEARCH` leaves it on, or
    /// the version shares one field with LoC's text).
    pub searched: bool,
    /// Pages with its text, and of those pages with no text of LoC's, when
    /// the release that made the version built its only main index (no
    /// deltas): the manifest records the index it built, so a delta's
    /// count leaves out its base. `None` otherwise.
    pub pages: Option<u64>,
    pub only_pages: Option<u64>,
    /// Years of its text the release could read, under the same condition.
    pub years: Option<u64>,
}

/// Pages whose main-index document holds our OCR's Latin-script text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct JaLatinPages {
    pub pages: u64,
    /// Exact engine keys from the serving version, not the current OCR job.
    pub by_engine: Vec<EnginePages>,
    /// Why our OCR read them (`missing`, `empty`, `short`, `garbled`,
    /// `mixed`), most pages first.
    pub by_reason: Vec<ReasonPages>,
    /// By newspaper, most pages first.
    pub by_title: Vec<TitleOcrPages>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReasonPages {
    pub reason: String,
    pub pages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TitleOcrPages {
    pub lccn: String,
    /// The catalog's name; the LCCN when the catalog lacks it.
    pub name: String,
    pub pages: u64,
}

/// Pages whose main-index document holds our own OCR's text in place of
/// LoC's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OcrLocalSummary {
    pub pages: u64,
    /// By engine, most pages first.
    pub by_engine: Vec<EnginePages>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnginePages {
    /// `ocr_source`, e.g. `usnm-tesseract`.
    pub source: String,
    /// The short engine key.
    pub engine: String,
    pub pages: u64,
}

/// One state's (or territory's) share of the published pages.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct StatePages {
    /// The places' postal code, e.g. `IL`.
    pub state: String,
    /// LoC's name for it, or the code if it isn't a known state.
    pub name: String,
    pub places: usize,
    pub titles: usize,
    pub pages: u64,
    /// Percent of every page in the version, to one decimal.
    pub percent: f64,
}

/// Pages by language. A title that lists several languages counts in each
/// one's row (as a search filtered to any of them finds its pages), so the
/// rows add up to more than the version's pages when there are such titles.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ByLanguage {
    /// The snapshot records pages per title. Older snapshots don't: then
    /// only the newspaper counts are known.
    pub pages_known: bool,
    /// Titles that list more than one language, and their pages.
    pub multilingual_titles: usize,
    pub multilingual_pages: Option<u64>,
    /// Most pages first (most titles, when pages aren't known).
    pub rows: Vec<LanguagePages>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LanguagePages {
    /// The catalog's code (MARC, e.g. `eng`, or LoC's own name for a language
    /// without one); `None` for titles that list no language.
    pub code: Option<String>,
    /// LoC's name for it, e.g. "English"; "Not recorded" when `code` is `None`.
    pub name: String,
    pub titles: usize,
    pub pages: Option<u64>,
    pub percent: Option<f64>,
}

/// Published pages summed by each place's state (its baselines).
pub fn by_state(rd: &RefData) -> Vec<StatePages> {
    let mut states: BTreeMap<&str, StatePages> = BTreeMap::new();
    for p in &rd.places {
        let r = states
            .entry(p.state.as_str())
            .or_insert_with(|| StatePages {
                state: p.state.clone(),
                name: names::state_name(&p.state).unwrap_or(&p.state).to_owned(),
                places: 0,
                titles: 0,
                pages: 0,
                percent: 0.0,
            });
        r.places += 1;
        r.pages += rd.place_pages.get(&p.id).copied().unwrap_or(0);
    }
    for t in rd.titles.values() {
        // A title that moved counts once in each state it was in.
        let in_states: BTreeSet<&str> = t
            .place_ids()
            .filter_map(|id| rd.place(id))
            .map(|p| p.state.as_str())
            .collect();
        for state in in_states {
            if let Some(r) = states.get_mut(state) {
                r.titles += 1;
            }
        }
    }
    let mut rows: Vec<StatePages> = states.into_values().collect();
    for r in &mut rows {
        r.percent = percent(r.pages, rd.pages);
    }
    rows.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.name.cmp(&b.name)));
    rows
}

const NOT_RECORDED: &str = "Not recorded";

/// Published pages (when the snapshot has them per title) and titles by language.
pub fn by_language(rd: &RefData) -> ByLanguage {
    let pages_of = |lccn: &str| {
        rd.title_pages
            .as_ref()
            .map(|tp| tp.get(lccn).copied().unwrap_or(0))
    };
    let mut langs: BTreeMap<Option<&str>, (usize, u64)> = BTreeMap::new();
    let (mut multilingual_titles, mut multilingual_pages) = (0, 0);
    for t in rd.titles.values() {
        let codes: BTreeSet<&str> = t
            .languages
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        let pages = pages_of(&t.lccn).unwrap_or(0);
        if codes.len() > 1 {
            multilingual_titles += 1;
            multilingual_pages += pages;
        }
        let keys: Vec<Option<&str>> = if codes.is_empty() {
            vec![None]
        } else {
            codes.into_iter().map(Some).collect()
        };
        for k in keys {
            let e = langs.entry(k).or_default();
            e.0 += 1;
            e.1 += pages;
        }
    }
    let known = rd.title_pages.is_some();
    let mut rows: Vec<LanguagePages> = langs
        .into_iter()
        .map(|(code, (titles, pages))| LanguagePages {
            name: code.map_or_else(|| NOT_RECORDED.to_owned(), names::language_name),
            code: code.map(str::to_owned),
            titles,
            pages: known.then_some(pages),
            percent: known.then(|| percent(pages, rd.pages)),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.pages
            .cmp(&a.pages)
            .then_with(|| b.titles.cmp(&a.titles))
            .then_with(|| a.name.cmp(&b.name))
    });
    ByLanguage {
        pages_known: known,
        multilingual_titles,
        multilingual_pages: known.then_some(multilingual_pages),
        rows,
    }
}

pub fn published(rd: &RefData) -> Published {
    let c = &rd.current;
    let deltas = c.indexes.len().saturating_sub(1);
    Published {
        index_version: c.index_version.clone(),
        published_at: c.published_at.clone(),
        synthetic: c.synthetic,
        pages: rd.pages,
        titles: rd.titles.len(),
        places: rd.places.len(),
        bounds: c.bounds.clone(),
        indexes: c.indexes.clone(),
        deltas,
        max_deltas: MAX_DELTAS,
        next_release_full: deltas >= MAX_DELTAS,
        batches: rd.published_batches.as_ref().map(HashMap::len),
        duplicate_pages: rd.duplicate_pages,
        by_state: by_state(rd),
        by_language: by_language(rd),
        ja: c.ja.clone(),
        by_year: rd
            .year_pages
            .iter()
            .map(|&(year, pages)| YearPages { year, pages })
            .collect(),
        sources: sources(rd),
    }
}

/// The texts the version searches besides LoC's, from its reference data.
pub fn sources(rd: &RefData) -> Sources {
    let american_stories = rd.has_american_stories().then(|| {
        // The manifest counts the main index its release built: the whole
        // version's only when that is its one main index.
        let whole = rd
            .american_stories_built
            .as_ref()
            .filter(|_| rd.current.indexes.len() == 1);
        AmericanStories {
            searched: rd.searches_american_stories(),
            pages: whole.map(|b| b.docs),
            only_pages: whole.map(|b| b.only_american_stories),
            years: whole.map(|b| b.years),
        }
    });
    let ja_latin = (!rd.ja_latin.is_empty()).then(|| {
        let mut engines: BTreeMap<(&str, &str), u64> = BTreeMap::new();
        let mut reasons: BTreeMap<&str, u64> = BTreeMap::new();
        let mut titles: BTreeMap<String, u64> = BTreeMap::new();
        for (doc_id, p) in &rd.ja_latin {
            *engines.entry((&p.ocr_source, &p.ocr_engine)).or_default() += 1;
            *reasons.entry(p.loc_text.as_str()).or_default() += 1;
            // The overlay holds LoC pages only (ADR-0026): parse the id rather than split it,
            // so anything else is counted under its own id, not a misread LCCN.
            let lccn = PageKey::from_doc_id(doc_id).map_or_else(|_| doc_id.clone(), |k| k.lccn);
            *titles.entry(lccn).or_default() += 1;
        }
        let mut by_reason: Vec<ReasonPages> = reasons
            .into_iter()
            .map(|(r, pages)| ReasonPages {
                reason: r.to_owned(),
                pages,
            })
            .collect();
        by_reason.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.reason.cmp(&b.reason)));
        let mut by_title: Vec<TitleOcrPages> = titles
            .into_iter()
            .map(|(lccn, pages)| TitleOcrPages {
                name: rd
                    .titles
                    .get(lccn.as_str())
                    .map_or_else(|| lccn.clone(), |t| t.name.clone()),
                lccn,
                pages,
            })
            .collect();
        by_title.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.lccn.cmp(&b.lccn)));
        JaLatinPages {
            pages: rd.ja_latin.len() as u64,
            by_engine: engines
                .into_iter()
                .map(|((source, engine), pages)| EnginePages {
                    source: source.to_owned(),
                    engine: engine.to_owned(),
                    pages,
                })
                .collect(),
            by_reason,
            by_title,
        }
    });
    let ocr_local = (!rd.ocr_local.is_empty()).then(|| {
        let mut per = vec![0u64; rd.ocr_local.engines.len()];
        for e in &rd.ocr_local.entries {
            if let Some(n) = per.get_mut(usize::from(e.engine)) {
                *n += 1;
            }
        }
        let mut by_engine: Vec<EnginePages> = rd
            .ocr_local
            .engines
            .iter()
            .zip(per)
            .filter(|(_, pages)| *pages > 0)
            .map(|(e, pages)| EnginePages {
                source: e.source.clone(),
                engine: e.engine.clone(),
                pages,
            })
            .collect();
        by_engine.sort_by(|a, b| b.pages.cmp(&a.pages).then_with(|| a.engine.cmp(&b.engine)));
        OcrLocalSummary {
            pages: rd.ocr_local.len() as u64,
            by_engine,
        }
    });
    Sources {
        ocr_credits: rd.ocr_credits.clone(),
        ocr_ja: rd
            .ocr_credits
            .as_ref()
            .and_then(|v| v.get("japanese"))
            .and_then(|v| serde_json::from_value::<OcrLocalSummary>(v.clone()).ok())
            .filter(|v| v.pages > 0),
        american_stories,
        ja_latin,
        ocr_local,
    }
}
