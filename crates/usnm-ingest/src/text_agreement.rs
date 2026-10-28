//! `usnm-ingest ocr-agreement` (#962): LoC's text against American Stories'
//! text for every page of an index version that has both, with no image and
//! no LoC request.
//!
//! Each page gets [`usnm_ocr_quality::agreement::compare`] (LoC as reader
//! `a`, American Stories as `b`: the pair the `usnm_quality` hat solves on,
//! plus word-bag cover and the length ratio) and, for each reader on its
//! own, the damage metric (`usnm_core::damage::score`, against the title's
//! catalog languages plus English) and a known-word rate: the share of its
//! word tokens in the top 5,000 of one language, the same language for both
//! readers ([`known_lang`]), so the two rates compare.
//!
//! The pass reads a version's batches (`batches.json`) and their curated
//! parts as `ocr-damage` does, and each batch's American Stories text as the
//! release does ([`american_stories::Source::load`]). It writes to its own
//! store, never to `curated`: `text-agreement/v<VERSION>/<batch>_ver<NN>.parquet`
//! (one row per page with American Stories' text, [`schema`]) and a
//! `.summary.json` beside it, last, which marks the batch done. A batch with a
//! summary is skipped, so a stopped run is continued by the next. When every
//! batch of the version has its tables, `versions/<version>.json` lists them
//! and `versions/<version>.summary.json` sums the histograms.
//!
//! `--sample-pct` scores only the pages the damage audit's sampler picks
//! (`ocr_damage::sampled`), under `text-agreement/v<VERSION>/sample-<cut>/`,
//! so a sample never stands in for the full tables.
//!
//! Logs counts and doc_ids, never text.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use arrow_array::builder::{
    Date32Builder, Float64Builder, Int16Builder, Int32Builder, StringBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use bytes::Bytes;
use chrono::{Datelike, NaiveDate};
use futures::{StreamExt, TryStreamExt};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use usnm_core::text::TextStatus;
use usnm_corpus_signals::text::score_texts;
pub use usnm_corpus_signals::text::{known_lang, ReaderStats, SKIP_TOO_LONG};
use usnm_ocr_contract::quality::round6;
use usnm_ocr_local::ocr_damage::{sampled, title_languages, version_batches};
use usnm_ocr_quality::agreement::{self, Agreement};
use usnm_overlay::curated::{read_part, CuratedRow};
use usnm_state::state::RunBatch;
use usnm_store::ObjectStore;

use crate::american_stories;
use crate::dedup;

const PROGRESS_EVERY: Duration = Duration::from_secs(60);

/// Where this metric version's tables live in the output store, for the
/// full pass (`cut` none) or a sample.
pub fn root(cut: Option<u64>) -> String {
    match cut {
        None => format!("text-agreement/v{}", agreement::VERSION),
        Some(c) => format!("text-agreement/v{}/sample-{c}", agreement::VERSION),
    }
}

fn batch_file(cut: Option<u64>, batch: &str, version: u16, ext: &str) -> String {
    let file = format!("{batch}_ver{version:02}{ext}");
    if file.len() <= 128 {
        format!("{}/{file}", root(cut))
    } else {
        format!("{}/{batch}/ver{version:02}{ext}", root(cut))
    }
}

/// A batch version's page table.
pub fn table_path(cut: Option<u64>, batch: &str, version: u16) -> String {
    batch_file(cut, batch, version, ".parquet")
}

/// A batch version's summary; written last, it marks the batch done.
pub fn summary_path(cut: Option<u64>, batch: &str, version: u16) -> String {
    batch_file(cut, batch, version, ".summary.json")
}

/// The list of tables that make up an index version.
pub fn versions_path(cut: Option<u64>, index_version: &str) -> String {
    format!("{}/versions/{index_version}.json", root(cut))
}

/// The version's summed summary.
pub fn version_summary_path(cut: Option<u64>, index_version: &str) -> String {
    format!("{}/versions/{index_version}.summary.json", root(cut))
}

/// What to do.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub index_version: String,
    /// Only these batches (`name` or `name_verNN`).
    pub batches: Vec<String>,
    /// Score only the pages the damage audit's sampler picks at this cut
    /// (of 10,000).
    pub sample_cut: Option<u64>,
    /// Stop starting batches after this.
    pub deadline: Option<Instant>,
    /// Batches scored at once.
    pub parallel: usize,
}

/// One page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageRow {
    pub doc_id: String,
    pub lccn: String,
    pub date: NaiveDate,
    pub batch: String,
    pub batch_version: u16,
    /// LoC's curated status: `ok`, `short` or `empty` (American Stories may
    /// have text where LoC has none).
    pub loc_text_status: &'static str,
    pub loc: ReaderStats,
    pub as_: ReaderStats,
    /// The language both known-word rates are measured in (a MARC code).
    pub known_lang: Option<String>,
    /// None when the pair was skipped (`skipped` says why).
    pub agreement: Option<Agreement>,
    pub skipped: Option<&'static str>,
}

fn status_str(s: TextStatus) -> &'static str {
    match s {
        TextStatus::Ok => "ok",
        TextStatus::Short => "short",
        TextStatus::Empty => "empty",
    }
}

/// Score one page: its curated row against American Stories' text.
pub fn score_page(row: &CuratedRow, as_text: &str, languages: &[&str]) -> PageRow {
    let k = &row.key;
    let loc_text = match (&row.status, &row.text) {
        (TextStatus::Ok, Some(t)) => t.as_str(),
        _ => "",
    };
    let scores = score_texts(loc_text, as_text, languages);
    PageRow {
        doc_id: k.doc_id(),
        lccn: k.lccn.clone(),
        date: k.date,
        batch: row.batch.clone(),
        batch_version: row.batch_version,
        loc_text_status: status_str(row.status),
        loc: scores.loc,
        as_: scores.as_,
        known_lang: scores.known_lang,
        agreement: scores.agreement,
        skipped: scores.skipped,
    }
}

/// The page table's columns. Stable for a metric version: a change here
/// is a new [`agreement::VERSION`].
pub fn schema() -> SchemaRef {
    let s = |n: &str, null: bool| Field::new(n, DataType::Utf8, null);
    let f = |n: &str| Field::new(n, DataType::Float64, true);
    let i = |n: &str, null: bool| Field::new(n, DataType::Int32, null);
    let mut fields = vec![
        s("doc_id", false),
        s("lccn", false),
        Field::new("date", DataType::Date32, false),
        s("batch", false),
        Field::new("batch_version", DataType::Int16, false),
        i("metric_version", false),
        s("loc_text_status", false),
    ];
    for r in ["loc", "as"] {
        fields.extend([
            i(&format!("{r}_chars"), false),
            i(&format!("{r}_words"), false),
            s(&format!("{r}_lang"), true),
            s(&format!("{r}_decision"), true),
            f(&format!("{r}_damage_rate")),
            f(&format!("{r}_function_share")),
            f(&format!("{r}_garbage_share")),
            f(&format!("{r}_known_rate")),
        ]);
    }
    fields.extend([
        s("known_lang", true),
        i("char_edits", true),
        i("word_edits", true),
        f("cer_sym"),
        f("cer_loc"),
        f("cer_as"),
        f("wer_loc"),
        f("wer_as"),
        f("dice"),
        i("bag_words_loc", true),
        i("bag_words_as", true),
        i("shared_words", true),
        f("cover_loc"),
        f("cover_as"),
        f("len_ratio"),
        f("local_cer_sym"),
        f("local_cer_loc"),
        f("local_cer_as"),
        f("matched_loc"),
        f("matched_as"),
        i("chunks_loc", true),
        i("chunks_as", true),
        s("skipped", true),
    ]);
    Arc::new(Schema::new(fields))
}

fn unix_days(d: NaiveDate) -> i32 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid");
    i32::try_from((d - epoch).num_days()).expect("dates fit in i32 days")
}

#[derive(Default)]
struct ReaderCols {
    chars: Int32Builder,
    words: Int32Builder,
    lang: StringBuilder,
    decision: StringBuilder,
    damage_rate: Float64Builder,
    function_share: Float64Builder,
    garbage_share: Float64Builder,
    known_rate: Float64Builder,
}

impl ReaderCols {
    fn push(&mut self, r: &ReaderStats) {
        self.chars.append_value(int(r.chars));
        self.words.append_value(int(r.words));
        self.lang.append_option(r.lang.as_deref());
        self.decision.append_option(r.decision);
        self.damage_rate.append_option(r.damage_rate);
        self.function_share.append_option(r.function_share);
        self.garbage_share.append_option(r.garbage_share);
        self.known_rate.append_option(r.known_rate);
    }

    fn finish(mut self, out: &mut Vec<ArrayRef>) {
        out.push(Arc::new(self.chars.finish()));
        out.push(Arc::new(self.words.finish()));
        out.push(Arc::new(self.lang.finish()));
        out.push(Arc::new(self.decision.finish()));
        out.push(Arc::new(self.damage_rate.finish()));
        out.push(Arc::new(self.function_share.finish()));
        out.push(Arc::new(self.garbage_share.finish()));
        out.push(Arc::new(self.known_rate.finish()));
    }
}

fn int(v: impl TryInto<i32>) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}

/// The page table of a batch version, as Parquet.
pub fn encode_table(rows: &[PageRow]) -> anyhow::Result<Vec<u8>> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(6).expect("valid level"),
        ))
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), schema(), Some(props))?;
    for chunk in rows.chunks(8192) {
        let mut doc_id = StringBuilder::new();
        let mut lccn = StringBuilder::new();
        let mut date = Date32Builder::new();
        let mut batch = StringBuilder::new();
        let mut batch_version = Int16Builder::new();
        let mut metric_version = Int32Builder::new();
        let mut status = StringBuilder::new();
        let mut loc = ReaderCols::default();
        let mut as_ = ReaderCols::default();
        let mut known_lang = StringBuilder::new();
        let mut char_edits = Int32Builder::new();
        let mut word_edits = Int32Builder::new();
        let mut floats: Vec<Float64Builder> = (0..6).map(|_| Float64Builder::new()).collect();
        let mut bag_loc = Int32Builder::new();
        let mut bag_as = Int32Builder::new();
        let mut shared = Int32Builder::new();
        let mut cover_loc = Float64Builder::new();
        let mut cover_as = Float64Builder::new();
        let mut len_ratio = Float64Builder::new();
        let mut local: Vec<Float64Builder> = (0..5).map(|_| Float64Builder::new()).collect();
        let mut chunks_loc = Int32Builder::new();
        let mut chunks_as = Int32Builder::new();
        let mut skipped = StringBuilder::new();
        for r in chunk {
            doc_id.append_value(&r.doc_id);
            lccn.append_value(&r.lccn);
            date.append_value(unix_days(r.date));
            batch.append_value(&r.batch);
            batch_version
                .append_value(i16::try_from(r.batch_version).context("batch version above 32767")?);
            metric_version.append_value(int(agreement::VERSION));
            status.append_value(r.loc_text_status);
            loc.push(&r.loc);
            as_.push(&r.as_);
            known_lang.append_option(r.known_lang.as_deref());
            let g = r.agreement.as_ref();
            char_edits.append_option(g.map(|g| int(g.pair.char_edits)));
            word_edits.append_option(g.map(|g| int(g.pair.word_edits)));
            let p = g.map(|g| &g.pair);
            for (b, v) in floats.iter_mut().zip([
                p.map(|p| p.cer_sym),
                p.map(|p| p.cer_a),
                p.map(|p| p.cer_b),
                p.map(|p| p.wer_a),
                p.map(|p| p.wer_b),
                p.map(|p| p.dice),
            ]) {
                b.append_option(v);
            }
            bag_loc.append_option(g.map(|g| int(g.bag_words_a)));
            bag_as.append_option(g.map(|g| int(g.bag_words_b)));
            shared.append_option(g.map(|g| int(g.shared_words)));
            cover_loc.append_option(g.and_then(|g| g.cover_a));
            cover_as.append_option(g.and_then(|g| g.cover_b));
            len_ratio.append_option(g.and_then(|g| g.len_ratio));
            for (b, v) in local.iter_mut().zip([
                g.and_then(|g| g.local_cer_sym),
                g.and_then(|g| g.local_cer_a),
                g.and_then(|g| g.local_cer_b),
                g.and_then(|g| g.matched_a),
                g.and_then(|g| g.matched_b),
            ]) {
                b.append_option(v);
            }
            chunks_loc.append_option(g.map(|g| int(g.local_a.chunks)));
            chunks_as.append_option(g.map(|g| int(g.local_b.chunks)));
            skipped.append_option(r.skipped);
        }
        let mut cols: Vec<ArrayRef> = vec![
            Arc::new(doc_id.finish()),
            Arc::new(lccn.finish()),
            Arc::new(date.finish()),
            Arc::new(batch.finish()),
            Arc::new(batch_version.finish()),
            Arc::new(metric_version.finish()),
            Arc::new(status.finish()),
        ];
        loc.finish(&mut cols);
        as_.finish(&mut cols);
        cols.push(Arc::new(known_lang.finish()));
        cols.push(Arc::new(char_edits.finish()));
        cols.push(Arc::new(word_edits.finish()));
        for mut b in floats {
            cols.push(Arc::new(b.finish()));
        }
        cols.push(Arc::new(bag_loc.finish()));
        cols.push(Arc::new(bag_as.finish()));
        cols.push(Arc::new(shared.finish()));
        cols.push(Arc::new(cover_loc.finish()));
        cols.push(Arc::new(cover_as.finish()));
        cols.push(Arc::new(len_ratio.finish()));
        for mut b in local {
            cols.push(Arc::new(b.finish()));
        }
        cols.push(Arc::new(chunks_loc.finish()));
        cols.push(Arc::new(chunks_as.finish()));
        cols.push(Arc::new(skipped.finish()));
        w.write(&RecordBatch::try_new(schema(), cols)?)?;
    }
    Ok(w.into_inner()?)
}

/// Bins of a histogram: [`BINS`] of equal width over `[0, BINS × width]`
/// (the top edge in the last bin), and an overflow count above it.
pub const BINS: usize = 100;

/// A fixed-bin histogram, summed across batches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hist {
    pub width: f64,
    pub counts: Vec<u64>,
    /// Values above `BINS × width`.
    pub overflow: u64,
}

impl Hist {
    pub fn new(width: f64) -> Self {
        Self {
            width,
            counts: vec![0; BINS],
            overflow: 0,
        }
    }

    pub fn add(&mut self, x: Option<f64>) {
        let Some(x) = x.filter(|x| x.is_finite()) else {
            return;
        };
        let x = x.max(0.0);
        if x > BINS as f64 * self.width {
            self.overflow += 1;
            return;
        }
        let bin = ((x / self.width) as usize).min(BINS - 1);
        self.counts[bin] += 1;
    }

    pub fn merge(&mut self, o: &Hist) {
        for (a, b) in self.counts.iter_mut().zip(&o.counts) {
            *a += b;
        }
        self.overflow += o.overflow;
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().sum::<u64>() + self.overflow
    }

    /// The upper edge of the bin holding the `q` quantile; none for an
    /// empty histogram or when the quantile is in the overflow.
    pub fn quantile(&self, q: f64) -> Option<f64> {
        let n = self.total();
        if n == 0 {
            return None;
        }
        let target = (q * n as f64).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= target {
                return Some(round6((i + 1) as f64 * self.width));
            }
        }
        None
    }
}

/// The histograms a batch's summary carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hists {
    /// Over pages where LoC's text is `ok` and the pair was computed.
    pub cer_sym: Hist,
    pub dice: Hist,
    pub cover_loc: Hist,
    pub cover_as: Hist,
    pub len_ratio: Hist,
    pub local_cer_sym: Hist,
    pub matched_loc: Hist,
    pub matched_as: Hist,
    pub loc_known_rate: Hist,
    pub as_known_rate: Hist,
    pub loc_damage_rate: Hist,
    pub as_damage_rate: Hist,
}

impl Default for Hists {
    fn default() -> Self {
        Self {
            // cer_sym is at most 2 (edits over the mean length).
            cer_sym: Hist::new(0.02),
            dice: Hist::new(0.01),
            cover_loc: Hist::new(0.01),
            cover_as: Hist::new(0.01),
            len_ratio: Hist::new(0.03),
            local_cer_sym: Hist::new(0.005),
            matched_loc: Hist::new(0.01),
            matched_as: Hist::new(0.01),
            loc_known_rate: Hist::new(0.01),
            as_known_rate: Hist::new(0.01),
            loc_damage_rate: Hist::new(0.01),
            as_damage_rate: Hist::new(0.01),
        }
    }
}

impl Hists {
    fn add(&mut self, r: &PageRow) {
        let Some(g) = &r.agreement else { return };
        self.cer_sym.add(Some(g.pair.cer_sym));
        self.dice.add(Some(g.pair.dice));
        self.cover_loc.add(g.cover_a);
        self.cover_as.add(g.cover_b);
        self.len_ratio.add(g.len_ratio);
        self.local_cer_sym.add(g.local_cer_sym);
        self.matched_loc.add(g.matched_a);
        self.matched_as.add(g.matched_b);
        self.loc_known_rate.add(r.loc.known_rate);
        self.as_known_rate.add(r.as_.known_rate);
        self.loc_damage_rate.add(r.loc.damage_rate);
        self.as_damage_rate.add(r.as_.damage_rate);
    }

    fn merge(&mut self, o: &Hists) {
        self.cer_sym.merge(&o.cer_sym);
        self.dice.merge(&o.dice);
        self.cover_loc.merge(&o.cover_loc);
        self.cover_as.merge(&o.cover_as);
        self.len_ratio.merge(&o.len_ratio);
        self.local_cer_sym.merge(&o.local_cer_sym);
        self.matched_loc.merge(&o.matched_loc);
        self.matched_as.merge(&o.matched_as);
        self.loc_known_rate.merge(&o.loc_known_rate);
        self.as_known_rate.merge(&o.as_known_rate);
        self.loc_damage_rate.merge(&o.loc_damage_rate);
        self.as_damage_rate.merge(&o.as_damage_rate);
    }
}

/// A batch version's summary file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchSummary {
    pub metric_version: u32,
    pub batch: String,
    pub batch_version: u16,
    /// Rows written: pages with American Stories' text (sampled, for a sample).
    pub pages: u64,
    /// Of those, pages where LoC's text is `ok`.
    pub loc_ok: u64,
    pub skipped: u64,
    /// The American Stories parts the batch read, with their sha256.
    pub as_parts: Vec<PartDigest>,
    /// The catalog languages each of the batch's titles was scored against.
    pub languages: BTreeMap<String, Vec<String>>,
    /// `cer_sym` by decade, over the pages `hists` counts.
    pub cer_sym_by_decade: BTreeMap<u16, Hist>,
    /// `local_cer_sym` by decade.
    pub local_cer_sym_by_decade: BTreeMap<u16, Hist>,
    pub hists: Hists,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartDigest {
    pub path: String,
    pub sha256: String,
}

/// Summarize a batch version's pages.
pub fn summarize(
    batch: &str,
    version: u16,
    rows: &[PageRow],
    as_parts: Vec<PartDigest>,
    languages: BTreeMap<String, Vec<String>>,
) -> BatchSummary {
    let mut out = BatchSummary {
        metric_version: agreement::VERSION,
        batch: batch.to_owned(),
        batch_version: version,
        pages: rows.len() as u64,
        loc_ok: 0,
        skipped: 0,
        as_parts,
        languages,
        cer_sym_by_decade: BTreeMap::new(),
        local_cer_sym_by_decade: BTreeMap::new(),
        hists: Hists::default(),
    };
    for r in rows {
        out.skipped += u64::from(r.skipped.is_some());
        if r.loc_text_status != "ok" {
            continue;
        }
        out.loc_ok += 1;
        out.hists.add(r);
        if let Some(g) = &r.agreement {
            let decade = usnm_core::decade::of_year(r.date.year());
            out.cer_sym_by_decade
                .entry(decade)
                .or_insert_with(|| Hist::new(0.02))
                .add(Some(g.pair.cer_sym));
            out.local_cer_sym_by_decade
                .entry(decade)
                .or_insert_with(|| Hist::new(0.005))
                .add(g.local_cer_sym);
        }
    }
    out
}

/// Score a batch version's pages that have American Stories' text.
async fn score_batch(
    curated: &dyn ObjectStore,
    source: &american_stories::Source,
    b: &RunBatch,
    titles: &Arc<HashMap<String, Vec<String>>>,
    cut: Option<u64>,
) -> anyhow::Result<(Vec<PageRow>, Vec<PartDigest>)> {
    let counts = dedup::load_counts(curated, b).await?;
    let stories = source.load(curated, &counts).await?;
    let parts: Vec<PartDigest> = stories
        .parts
        .iter()
        .map(|p| PartDigest {
            path: p.path.clone(),
            sha256: p.sha256.clone(),
        })
        .collect();
    let texts = Arc::new(stories.texts);
    let mut rows = Vec::new();
    if texts.is_empty() {
        return Ok((rows, parts));
    }
    for part in &b.curated.parts {
        let bytes = curated
            .get(part)
            .await?
            .with_context(|| format!("curated part {part} not found"))?;
        let titles = Arc::clone(titles);
        let texts = Arc::clone(&texts);
        let part = part.clone();
        let scored = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<PageRow>> {
            let mut out = Vec::new();
            read_part(Bytes::from(bytes), true, |row| {
                let doc_id = row.key.doc_id();
                let Some(as_text) = texts.get(&doc_id) else {
                    return Ok(());
                };
                if cut.is_some_and(|c| !sampled(&doc_id, c)) {
                    return Ok(());
                }
                let langs: Vec<&str> = titles
                    .get(&row.key.lccn)
                    .map(|l| l.iter().map(String::as_str).collect())
                    .unwrap_or_default();
                out.push(score_page(&row, as_text, &langs));
                Ok(())
            })
            .with_context(|| format!("reading {part}"))?;
            Ok(out)
        })
        .await??;
        rows.extend(scored);
    }
    Ok((rows, parts))
}

/// The catalog languages each of a batch's titles is scored against.
pub fn batch_languages(
    titles: &HashMap<String, Vec<String>>,
    b: &RunBatch,
) -> BTreeMap<String, Vec<String>> {
    b.curated
        .lccns
        .iter()
        .map(|l| (l.clone(), titles.get(l).cloned().unwrap_or_default()))
        .collect()
}

/// A batch already scored is kept only if it was scored with this metric
/// version, from the American Stories parts the batch would read now and
/// against the same title languages; else the run stops and says which
/// files to delete.
async fn check_kept(
    curated: &dyn ObjectStore,
    source: &american_stories::Source,
    titles: &HashMap<String, Vec<String>>,
    b: &RunBatch,
    cut: Option<u64>,
    body: &[u8],
) -> anyhow::Result<()> {
    let (name, v) = (b.batch.as_str(), b.curated.version);
    let path = summary_path(cut, name, v);
    let kept: BatchSummary =
        serde_json::from_slice(body).with_context(|| format!("reading {path}"))?;
    let mut now = source.wanted(&dedup::load_counts(curated, b).await?);
    now.sort();
    let mut then: Vec<String> = kept.as_parts.into_iter().map(|p| p.path).collect();
    then.sort();
    if kept.metric_version != agreement::VERSION || now != then {
        bail!(
            "{path} was written with metric version {} from {} American Stories parts; \
             this run has version {} and {} parts for the batch. Delete it and {} to \
             score the batch again",
            kept.metric_version,
            then.len(),
            agreement::VERSION,
            now.len(),
            table_path(cut, name, v)
        );
    }
    if kept.languages != batch_languages(titles, b) {
        bail!(
            "{path} was scored against other title languages than this version's \
             titles have. Delete it and {} to score the batch again",
            table_path(cut, name, v)
        );
    }
    Ok(())
}

/// What a pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct PassReport {
    pub batches: usize,
    pub scored: usize,
    pub skipped: usize,
    pub left: usize,
    pub pages: u64,
    pub version_list_written: bool,
}

/// `versions/<index_version>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionList {
    pub index_version: String,
    pub metric_version: u32,
    pub sample_cut: Option<u64>,
    pub batches: Vec<VersionBatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionBatch {
    pub batch: String,
    pub version: u16,
    pub table: String,
    pub summary: String,
}

/// `versions/<index_version>.summary.json`: the batches' summaries summed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionSummary {
    pub index_version: String,
    pub metric_version: u32,
    pub sample_cut: Option<u64>,
    pub batches: u64,
    pub pages: u64,
    pub loc_ok: u64,
    pub skipped: u64,
    /// p10, p25, p50, p75 and p90 of each histogram (bin upper edges).
    pub quantiles: BTreeMap<String, Vec<Option<f64>>>,
    pub cer_sym_median_by_decade: BTreeMap<u16, Option<f64>>,
    pub local_cer_sym_median_by_decade: BTreeMap<u16, Option<f64>>,
    pub hists: Hists,
}

const QS: [f64; 5] = [0.1, 0.25, 0.5, 0.75, 0.9];

/// Sum batch summaries into the version's.
pub fn sum_summaries(
    index_version: &str,
    cut: Option<u64>,
    summaries: &[BatchSummary],
) -> VersionSummary {
    let mut hists = Hists::default();
    let mut by_decade: BTreeMap<u16, Hist> = BTreeMap::new();
    let mut local_by_decade: BTreeMap<u16, Hist> = BTreeMap::new();
    let (mut pages, mut loc_ok, mut skipped) = (0, 0, 0);
    for s in summaries {
        pages += s.pages;
        loc_ok += s.loc_ok;
        skipped += s.skipped;
        hists.merge(&s.hists);
        for (d, h) in &s.cer_sym_by_decade {
            by_decade
                .entry(*d)
                .or_insert_with(|| Hist::new(h.width))
                .merge(h);
        }
        for (d, h) in &s.local_cer_sym_by_decade {
            local_by_decade
                .entry(*d)
                .or_insert_with(|| Hist::new(h.width))
                .merge(h);
        }
    }
    let q = |h: &Hist| QS.iter().map(|q| h.quantile(*q)).collect::<Vec<_>>();
    let quantiles = BTreeMap::from([
        ("as_damage_rate".to_owned(), q(&hists.as_damage_rate)),
        ("as_known_rate".to_owned(), q(&hists.as_known_rate)),
        ("cer_sym".to_owned(), q(&hists.cer_sym)),
        ("cover_as".to_owned(), q(&hists.cover_as)),
        ("cover_loc".to_owned(), q(&hists.cover_loc)),
        ("dice".to_owned(), q(&hists.dice)),
        ("len_ratio".to_owned(), q(&hists.len_ratio)),
        ("local_cer_sym".to_owned(), q(&hists.local_cer_sym)),
        ("matched_as".to_owned(), q(&hists.matched_as)),
        ("matched_loc".to_owned(), q(&hists.matched_loc)),
        ("loc_damage_rate".to_owned(), q(&hists.loc_damage_rate)),
        ("loc_known_rate".to_owned(), q(&hists.loc_known_rate)),
    ]);
    VersionSummary {
        index_version: index_version.to_owned(),
        metric_version: agreement::VERSION,
        sample_cut: cut,
        batches: summaries.len() as u64,
        pages,
        loc_ok,
        skipped,
        quantiles,
        cer_sym_median_by_decade: by_decade
            .iter()
            .map(|(d, h)| (*d, h.quantile(0.5)))
            .collect(),
        local_cer_sym_median_by_decade: local_by_decade
            .iter()
            .map(|(d, h)| (*d, h.quantile(0.5)))
            .collect(),
        hists,
    }
}

/// The pass, full or sampled.
pub async fn run(
    curated: Arc<dyn ObjectStore>,
    reference: &dyn ObjectStore,
    out: Arc<dyn ObjectStore>,
    opts: &Options,
) -> anyhow::Result<PassReport> {
    let cut = opts.sample_cut;
    let batches = version_batches(reference, &opts.index_version, &opts.batches).await?;
    let titles = Arc::new(title_languages(reference, &opts.index_version).await?);
    let source = Arc::new(american_stories::Source::list(curated.as_ref()).await?);
    if source.parts() == 0 {
        bail!(
            "the curated store has no finished American Stories parts ({}/): \
             nothing to compare, and empty tables would be kept as done",
            american_stories::PAGES
        );
    }
    let started = Instant::now();
    let total = batches.len();
    tracing::info!(
        index_version = %opts.index_version,
        batches = total,
        metric_version = agreement::VERSION,
        sample_cut = ?cut,
        american_stories_years = source.years.len(),
        american_stories_parts = source.parts(),
        "ocr agreement starting"
    );
    let report = Arc::new(tokio::sync::Mutex::new(PassReport {
        batches: total,
        ..PassReport::default()
    }));
    let last_progress = Arc::new(tokio::sync::Mutex::new(Instant::now()));
    futures::stream::iter(batches.iter())
        .map(|b| {
            let curated = Arc::clone(&curated);
            let out = Arc::clone(&out);
            let titles = Arc::clone(&titles);
            let source = Arc::clone(&source);
            let report = Arc::clone(&report);
            let last_progress = Arc::clone(&last_progress);
            async move {
                let (name, v) = (b.batch.as_str(), b.curated.version);
                if let Some(body) = out.get(&summary_path(cut, name, v)).await? {
                    check_kept(curated.as_ref(), &source, &titles, b, cut, &body).await?;
                    report.lock().await.skipped += 1;
                    return Ok::<(), anyhow::Error>(());
                }
                if opts.deadline.is_some_and(|d| Instant::now() >= d) {
                    report.lock().await.left += 1;
                    return Ok(());
                }
                let (rows, parts) = score_batch(curated.as_ref(), &source, b, &titles, cut).await?;
                let pages = rows.len() as u64;
                let batch = name.to_owned();
                let languages = batch_languages(&titles, b);
                let (table, summary) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                    Ok((
                        encode_table(&rows)?,
                        serde_json::to_vec(&summarize(&batch, v, &rows, parts, languages))?,
                    ))
                })
                .await??;
                if !out
                    .put_new(
                        &table_path(cut, name, v),
                        table,
                        "application/vnd.apache.parquet",
                    )
                    .await?
                {
                    // A table without its summary: an earlier run stopped
                    // between the two, possibly from other inputs.
                    bail!(
                        "{} is there without its summary (an earlier run stopped between \
                         the two): delete it to score the batch again",
                        table_path(cut, name, v)
                    );
                }
                out.put_new(&summary_path(cut, name, v), summary, "application/json")
                    .await?;
                let mut r = report.lock().await;
                r.scored += 1;
                r.pages += pages;
                let mut last = last_progress.lock().await;
                if last.elapsed() >= PROGRESS_EVERY {
                    *last = Instant::now();
                    let secs = started.elapsed().as_secs_f64().max(1.0);
                    tracing::info!(
                        batches_done = r.scored + r.skipped,
                        batches = total,
                        pages = r.pages,
                        pages_per_sec = (r.pages as f64 / secs).round(),
                        elapsed_s = secs.round(),
                        "ocr agreement progress"
                    );
                }
                Ok(())
            }
        })
        .buffer_unordered(opts.parallel.max(1))
        .try_collect::<Vec<()>>()
        .await?;
    let mut report = Arc::try_unwrap(report)
        .expect("every task is done")
        .into_inner();
    if report.left == 0 && opts.batches.is_empty() {
        let mut summaries = Vec::with_capacity(batches.len());
        for b in &batches {
            let path = summary_path(cut, &b.batch, b.curated.version);
            let body = out
                .get(&path)
                .await?
                .with_context(|| format!("{path} vanished"))?;
            summaries.push(
                serde_json::from_slice::<BatchSummary>(&body)
                    .with_context(|| format!("reading {path}"))?,
            );
        }
        let summed = sum_summaries(&opts.index_version, cut, &summaries);
        out.put(
            &version_summary_path(cut, &opts.index_version),
            serde_json::to_vec_pretty(&summed)?,
            "application/json",
        )
        .await?;
        let list = VersionList {
            index_version: opts.index_version.clone(),
            metric_version: agreement::VERSION,
            sample_cut: cut,
            batches: batches
                .iter()
                .map(|b| VersionBatch {
                    batch: b.batch.clone(),
                    version: b.curated.version,
                    table: table_path(cut, &b.batch, b.curated.version),
                    summary: summary_path(cut, &b.batch, b.curated.version),
                })
                .collect(),
        };
        report.version_list_written = out
            .put_new(
                &versions_path(cut, &opts.index_version),
                serde_json::to_vec_pretty(&list)?,
                "application/json",
            )
            .await?;
        tracing::info!(
            pages = summed.pages,
            loc_ok = summed.loc_ok,
            cer_sym = ?summed.quantiles.get("cer_sym"),
            local_cer_sym = ?summed.quantiles.get("local_cer_sym"),
            dice = ?summed.quantiles.get("dice"),
            "ocr agreement summary"
        );
    }
    tracing::info!(
        index_version = %opts.index_version,
        batches = report.batches,
        scored = report.scored,
        skipped = report.skipped,
        left = report.left,
        pages = report.pages,
        version_list_written = report.version_list_written,
        elapsed_s = started.elapsed().as_secs(),
        "ocr agreement finished"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use usnm_core::damage;
    use usnm_core::ids::PageKey;
    use usnm_ocr_contract::quality::MAX_TEXT;

    fn row(text: Option<&str>, status: TextStatus) -> CuratedRow {
        CuratedRow {
            key: PageKey {
                lccn: "sn83045462".into(),
                date: NaiveDate::from_ymd_opt(1890, 3, 4).unwrap(),
                edition: 1,
                seq: 2,
            },
            batch: "dlc_abc".into(),
            batch_version: 1,
            ocr_source: "loc".into(),
            status,
            text: text.map(str::to_owned),
            text_chars: 0,
            word_count: 0,
            text_sha256: [0; 32],
            ingested_at: chrono::Utc::now(),
        }
    }

    const LOC: &str = "The Senate met at noon to-day, and the President pro tem. \
        laid before the body a message from the House of Representatives \
        announcing the passage of the bill for the relief of the settlers \
        upon the public lands in the State of Kansas and the Territory.";
    const AS: &str = "The Senate met at noon to-day, and the President pro tem. \
        laid before the body a message from the House of Representatlves \
        announcing the passage of the bill for the rclief of the settlers \
        upon the public lands in the State of Kansas and the Territory.";

    #[test]
    fn scores_a_page_with_both_readers() {
        let r = score_page(&row(Some(LOC), TextStatus::Ok), AS, &["eng"]);
        assert_eq!(r.doc_id, row(None, TextStatus::Ok).key.doc_id());
        assert_eq!(r.loc_text_status, "ok");
        let g = r.agreement.as_ref().unwrap();
        assert_eq!(g.pair.char_edits, 2);
        assert!(g.pair.cer_sym > 0.0 && g.pair.cer_sym < 0.02);
        // Too short a passage for detection: `und`, and the known-word
        // language falls back to the title's.
        assert_eq!(r.loc.lang.as_deref(), Some(damage::UNKNOWN));
        assert_eq!(r.known_lang.as_deref(), Some("eng"));
        // LoC's text has every word right, American Stories' two wrong.
        assert!(r.loc.known_rate.unwrap() > r.as_.known_rate.unwrap());
        assert_eq!(r.loc.chars as usize, g.pair.chars_a);
        assert!(r.skipped.is_none());
        // Deterministic.
        assert_eq!(r, score_page(&row(Some(LOC), TextStatus::Ok), AS, &["eng"]));
    }

    #[test]
    fn a_page_loc_has_no_text_for() {
        let r = score_page(&row(None, TextStatus::Empty), AS, &["eng"]);
        assert_eq!(r.loc_text_status, "empty");
        assert_eq!(r.loc.lang, None);
        assert_eq!(r.loc.chars, 0);
        let g = r.agreement.unwrap();
        assert_eq!(g.pair.cer_a, 1.0);
        assert_eq!(g.len_ratio, None);
        // The known-word language falls back to American Stories'.
        assert_eq!(r.known_lang.as_deref(), Some("eng"));
    }

    #[test]
    fn source_status_is_applied_before_shared_scoring() {
        for status in [TextStatus::Short, TextStatus::Empty] {
            let r = score_page(&row(Some(LOC), status), AS, &["eng"]);
            let expected = score_texts("", AS, &["eng"]);
            assert_eq!(r.loc, expected.loc);
            assert_eq!(r.as_, expected.as_);
            assert_eq!(r.agreement, expected.agreement);
            assert_eq!(r.known_lang, expected.known_lang);
            assert_eq!(r.loc_text_status, status_str(status));
        }
    }

    #[test]
    fn too_long_is_skipped() {
        let long = "word ".repeat(MAX_TEXT / 5 + 1);
        let r = score_page(&row(Some(&long), TextStatus::Ok), AS, &["eng"]);
        assert_eq!(r.skipped, Some(SKIP_TOO_LONG));
        assert!(r.agreement.is_none());
        assert_eq!(r.loc.words as usize, MAX_TEXT / 5 + 1);
        assert!(r.as_.chars > 0);
    }

    #[test]
    fn table_round_trips_with_the_schema() {
        let rows = vec![
            score_page(&row(Some(LOC), TextStatus::Ok), AS, &["eng"]),
            score_page(&row(None, TextStatus::Empty), AS, &["eng"]),
        ];
        let bytes = encode_table(&rows).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
        assert_eq!(reader.schema().as_ref(), schema().as_ref());
        let batches: Vec<_> = reader.build().unwrap().map(|b| b.unwrap()).collect();
        assert_eq!(batches[0].num_rows(), 2);
        let cer = batches[0]
            .column_by_name("cer_sym")
            .unwrap()
            .as_any()
            .downcast_ref::<arrow_array::Float64Array>()
            .unwrap()
            .clone();
        assert_eq!(
            cer.value(0),
            rows[0].agreement.as_ref().unwrap().pair.cer_sym
        );
        let mv = batches[0]
            .column_by_name("metric_version")
            .unwrap()
            .as_any()
            .downcast_ref::<arrow_array::Int32Array>()
            .unwrap()
            .clone();
        assert_eq!(mv.value(1), agreement::VERSION as i32);
        let lang = batches[0].column_by_name("loc_lang").unwrap();
        assert!(lang.is_null(1));
    }

    #[test]
    fn histograms_and_quantiles() {
        let mut h = Hist::new(0.01);
        for x in [0.0, 0.005, 0.02, 0.5, 3.0] {
            h.add(Some(x));
        }
        h.add(None);
        assert_eq!(h.total(), 5);
        assert_eq!(h.counts[0], 2);
        assert_eq!(h.overflow, 1);
        assert_eq!(h.quantile(0.5), Some(0.03));
        // The top edge is in range; a quantile in the overflow is none.
        h.add(Some(1.0));
        assert_eq!(h.counts[BINS - 1], 1);
        assert_eq!(h.quantile(0.99), None);
        let mut only = Hist::new(0.02);
        only.add(Some(1.333333));
        assert_eq!(only.quantile(0.5), Some(1.34));
        assert_eq!(Hist::new(0.01).quantile(0.5), None);
    }

    #[test]
    fn summaries_sum() {
        let rows = vec![
            score_page(&row(Some(LOC), TextStatus::Ok), AS, &["eng"]),
            score_page(&row(None, TextStatus::Empty), AS, &["eng"]),
        ];
        let s = summarize("dlc_abc", 1, &rows, vec![], BTreeMap::new());
        assert_eq!((s.pages, s.loc_ok, s.skipped), (2, 1, 0));
        assert_eq!(s.cer_sym_by_decade[&1890].total(), 1);
        let v = sum_summaries("pages-v1", None, &[s.clone(), s]);
        assert_eq!((v.pages, v.loc_ok, v.batches), (4, 2, 2));
        assert_eq!(v.hists.cer_sym.total(), 2);
        assert_eq!(v.quantiles["cer_sym"][2], Some(0.02));
    }

    #[tokio::test]
    async fn refuses_an_empty_source_and_batches_scored_from_other_parts() {
        use usnm_state::state::Curated;
        use usnm_store::LocalStore;
        let (c, r, o) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let curated: Arc<dyn ObjectStore> = Arc::new(LocalStore::new(c.path()));
        let reference = LocalStore::new(r.path());
        let out: Arc<dyn ObjectStore> = Arc::new(LocalStore::new(o.path()));
        let page = row(Some(LOC), TextStatus::Ok);
        let (doc_id, date) = (page.key.doc_id(), page.key.date);
        let mut w = usnm_overlay::curated::PartWriter::new();
        assert!(w.push(page).unwrap().is_none());
        let part = w.finish_part().unwrap().unwrap();
        let put = |store: Arc<dyn ObjectStore>, path: &'static str, body: Vec<u8>| async move {
            store
                .put(path, body, "application/octet-stream")
                .await
                .unwrap();
        };
        put(
            Arc::clone(&curated),
            "pages/dlc_abc/ver01/part-0.parquet",
            part,
        )
        .await;
        let day = usnm_core::time::day_number(date);
        put(
            Arc::clone(&curated),
            "pages/dlc_abc/ver01/counts.json",
            serde_json::to_vec(&serde_json::json!({"sn83045462": {day.to_string(): 1}})).unwrap(),
        )
        .await;
        let batches = vec![RunBatch {
            batch: "dlc_abc".into(),
            curated: Curated {
                version: 1,
                parts: vec!["pages/dlc_abc/ver01/part-0.parquet".into()],
                counts: "pages/dlc_abc/ver01/counts.json".into(),
                pages: 1,
                ok_pages: 1,
                lccns: vec!["sn83045462".into()],
                source_sha256: String::new(),
                first: String::new(),
                last: String::new(),
            },
        }];
        reference
            .put(
                "pages-v1/batches.json",
                serde_json::to_vec(&batches).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        reference
            .put(
                "pages-v1/titles.json",
                br#"[{"lccn":"sn83045462","languages":["eng"]}]"#.to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        let opts = Options {
            index_version: "pages-v1".into(),
            parallel: 1,
            ..Options::default()
        };
        let go = || run(Arc::clone(&curated), &reference, Arc::clone(&out), &opts);
        // No American Stories text yet: refused, nothing kept.
        let e = go().await.unwrap_err().to_string();
        assert!(e.contains("no finished American Stories parts"), "{e}");
        let as_part = |text: &'static str| {
            american_stories::encode_part(&[(doc_id.as_str(), "sn83045462", date, text)]).unwrap()
        };
        put(
            Arc::clone(&curated),
            "american-stories/pages/sn83045462/1890-000.parquet",
            as_part(AS),
        )
        .await;
        put(
            Arc::clone(&curated),
            "american-stories/years/1890.json",
            b"{}".to_vec(),
        )
        .await;
        let first = go().await.unwrap();
        assert_eq!((first.scored, first.pages, first.skipped), (1, 1, 0));
        assert!(first.version_list_written);
        let again = go().await.unwrap();
        assert_eq!((again.scored, again.skipped), (0, 1));
        // A new American Stories part for the batch's title-year: the kept
        // table no longer matches its inputs.
        put(
            Arc::clone(&curated),
            "american-stories/pages/sn83045462/1890-001.parquet",
            as_part("another copy"),
        )
        .await;
        let e = go().await.unwrap_err().to_string();
        assert!(
            e.contains("from 1 American Stories parts") && e.contains("2 parts"),
            "{e}"
        );
        std::fs::remove_file(
            c.path()
                .join("american-stories/pages/sn83045462/1890-001.parquet"),
        )
        .unwrap();
        // Other title languages: refused too.
        reference
            .put(
                "pages-v1/titles.json",
                br#"[{"lccn":"sn83045462","languages":["ger"]}]"#.to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        let e = go().await.unwrap_err().to_string();
        assert!(e.contains("other title languages"), "{e}");
        // A table without its summary is not kept.
        std::fs::remove_file(o.path().join(summary_path(None, "dlc_abc", 1))).unwrap();
        let e = go().await.unwrap_err().to_string();
        assert!(e.contains("without its summary"), "{e}");
    }

    #[test]
    fn paths() {
        assert_eq!(
            table_path(None, "dlc_abc", 2),
            format!(
                "text-agreement/v{}/dlc_abc_ver02.parquet",
                agreement::VERSION
            )
        );
        assert_eq!(
            summary_path(Some(100), "dlc_abc", 2),
            format!(
                "text-agreement/v{}/sample-100/dlc_abc_ver02.summary.json",
                agreement::VERSION
            )
        );
    }
}
