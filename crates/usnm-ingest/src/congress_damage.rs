//! `usnm-ingest congress-damage` (#922): the damage metric over every page of
//! the curated Congress pages of one series, the Serial Set by default, for the
//! quality gate. The text there is GPO's text layer, read with `pdftotext`, so
//! the metric is the one the OCR audit uses for newspapers (`usnm_core::damage`,
//! the Rust port of `ja-ocr/damage_v2.py`), measured on every page instead of a
//! sample.
//!
//! **Which volumes.** Each unit's newest finished attempt
//! ([`curated::finished_in`]), as the release lists them, less the units a later
//! empty attempt supersedes ([`curated::superseded_by_empty`]). Each attempt's
//! parts are read as the release reads them ([`curated::read_checked`]: sha256,
//! bytes and rows against `counts.json`), and `counts.json` is checked against
//! the attempt it was found under.
//!
//! **Which pages.** A page with text (`text_status` `ok`) of at least
//! [`MIN_CHARS`] characters is scored. The headline `damage_rate` and
//! `function_share` are the forced English score ([`damage::score_forced`], what
//! the 20 October sample on #922 used), and `lang`, `decision` and
//! `garbage_share` come from [`damage::score`] against English. Every other page
//! stays in the table with null scores and `skipped` saying why (`no_text`,
//! `below_floor`), so a table has a row for each page of the attempt. The text
//! is never written.
//!
//! **Where it goes.** `audit/damage/v<VERSION>/congress/<series>/` in `--out`
//! (the curated store when `--out` is not given), never under `congress/`, so no
//! listing of the units sees a table as an attempt. Per volume and attempt:
//! `<unit>/<attempt>.parquet`, then `<unit>/<attempt>.summary.json` last, which
//! marks the volume done, so a run stopped by `--max-runtime-secs` or a failure
//! is continued by the next. Every write is create-only. A run over every volume
//! that leaves none behind also writes `corpus/<key>.json`, the summaries summed
//! (pages above [`damage::DAMAGED`] and [`damage::BADLY_DAMAGED`] by decade), keyed
//! by the set of attempts it covers: a later run over the same set finds it and
//! writes nothing, and one over a changed set writes another beside it.
//!
//! Logs counts and doc_ids, never text.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use arrow_array::builder::{
    Date32Builder, Float64Builder, Int32Builder, StringBuilder, UInt32Builder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use chrono::{Datelike, NaiveDate};
use futures::StreamExt;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use usnm_congress::curated::{self, Counts, CuratedPage, Finished};
use usnm_congress::{globe, serialset};
use usnm_core::damage;
use usnm_core::digest::sha256_hex;
use usnm_store::ObjectStore;

const PROGRESS_EVERY: Duration = Duration::from_secs(60);

/// A page needs at least this many characters of text to be scored: the floor
/// the 20 October sample on #922 used, under which a page is a heading, a
/// table fragment or a blank leaf.
pub const MIN_CHARS: usize = 200;
/// The language every page is scored against, as ISO 639-3 for the headline
/// ([`damage::score_forced`]) and as a catalog code for [`damage::score`].
const FORCED: &str = "eng";
const CATALOG: &str = "eng";
/// Why a page has no scores: its text status isn't `ok`.
pub const NO_TEXT: &str = "no_text";
/// Why a page has no scores: its text is under [`MIN_CHARS`].
pub const BELOW_FLOOR: &str = "below_floor";
const PARQUET: &str = "application/vnd.apache.parquet";
const JSON: &str = "application/json";

/// Where this metric version's tables of a series live in the output store.
pub fn root(series: &str) -> String {
    format!("audit/damage/v{}/congress/{series}", damage::VERSION)
}

/// An attempt's page table.
pub fn table_path(series: &str, unit: &str, attempt: u32) -> String {
    format!("{}/{unit}/{attempt:04}.parquet", root(series))
}

/// An attempt's summary; written last, it marks the attempt done.
pub fn summary_path(series: &str, unit: &str, attempt: u32) -> String {
    format!("{}/{unit}/{attempt:04}.summary.json", root(series))
}

/// The roll-up of the set of attempts `key` names ([`corpus_key`]).
pub fn corpus_path(series: &str, key: &str) -> String {
    format!("{}/corpus/{key}.json", root(series))
}

/// What to do.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// One of [`usnm_congress::SERIES`].
    pub series: String,
    /// Only these volumes (`00033_00_00`); empty: all.
    pub volumes: Vec<String>,
    /// Only the volumes with these serial numbers (Serial Set only); empty: all.
    pub serials: Vec<u32>,
    /// Stop starting volumes after this.
    pub deadline: Option<Instant>,
    /// Volumes scored at once.
    pub parallel: usize,
}

/// A finished attempt to score.
#[derive(Debug, Clone)]
pub struct Unit {
    pub series: String,
    pub unit: String,
    pub attempt: u32,
    pub counts: Counts,
    /// The sha256 of its `counts.json`.
    pub counts_sha256: String,
}

/// The units to score: each one's newest finished attempt, and the units left
/// out because a later empty attempt (or, for the Globe, another item that
/// holds the same pages) supersedes it. Sorted by unit.
pub async fn units(
    store: &dyn ObjectStore,
    series: &str,
) -> anyhow::Result<(Vec<Unit>, Vec<String>)> {
    ensure!(
        usnm_congress::SERIES.contains(&series),
        "unknown Congress series `{series}` (one of {})",
        usnm_congress::SERIES.join(", ")
    );
    let empties = curated::empties_in(store, series).await?;
    let (mut out, mut left_out) = (Vec::new(), Vec::new());
    for f in curated::finished_in(store, series).await? {
        if f.series != series {
            continue;
        }
        let item_superseded =
            series == globe::SERIES && globe::SUPERSEDED.iter().any(|(id, _, _)| *id == f.unit);
        if item_superseded || curated::superseded_by_empty(&empties, &f) {
            left_out.push(f.unit);
            continue;
        }
        let (counts, counts_sha256) = curated::read_counts(store, &f).await?;
        let Finished {
            series,
            unit,
            attempt,
            counts_path,
        } = f;
        ensure!(
            counts.series == series && counts.unit == unit && counts.attempt == attempt,
            "`{counts_path}` describes another unit"
        );
        out.push(Unit {
            series,
            unit,
            attempt,
            counts,
            counts_sha256,
        });
    }
    Ok((out, left_out))
}

/// The serial number a Serial Set volume id starts with (`00033_00_00`: 33).
fn serial_of(unit: &str) -> Option<u32> {
    unit.split('_').next()?.parse().ok()
}

/// The units a run covers: all of them, or those `opts` names. A name that
/// matches no unit is an error, as for `congress-serialset --serials`.
fn select<'a>(all: &'a [Unit], opts: &Options) -> anyhow::Result<Vec<&'a Unit>> {
    if opts.volumes.is_empty() && opts.serials.is_empty() {
        return Ok(all.iter().collect());
    }
    ensure!(
        opts.serials.is_empty() || opts.series == serialset::SERIES,
        "--serials is for the Serial Set's volumes, not `{}`",
        opts.series
    );
    let unknown: Vec<&String> = opts
        .volumes
        .iter()
        .filter(|v| !all.iter().any(|u| &u.unit == *v))
        .collect();
    ensure!(
        unknown.is_empty(),
        "no finished {} volume {unknown:?} to score (a volume with a later empty attempt is left out)",
        opts.series
    );
    let unknown: Vec<u32> = opts
        .serials
        .iter()
        .copied()
        .filter(|s| !all.iter().any(|u| serial_of(&u.unit) == Some(*s)))
        .collect();
    ensure!(
        unknown.is_empty(),
        "no finished volume for serial numbers {unknown:?} to score"
    );
    Ok(all
        .iter()
        .filter(|u| {
            let by_volume = opts.volumes.contains(&u.unit);
            let by_serial = serial_of(&u.unit).is_some_and(|s| opts.serials.contains(&s));
            by_volume || by_serial
        })
        .collect())
}

/// One page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageRow {
    pub doc_id: String,
    pub unit_id: String,
    pub package_id: Option<String>,
    pub serial: Option<String>,
    pub seq: u32,
    pub date: NaiveDate,
    pub text_status: String,
    pub text_chars: i32,
    /// None when the page was scored; else [`NO_TEXT`] or [`BELOW_FLOOR`].
    pub skipped: Option<&'static str>,
    /// The forced English damage rate; None when skipped, or when the page has
    /// none of the words the rate counts.
    pub damage_rate: Option<f64>,
    pub function_share: Option<f64>,
    pub lang: Option<String>,
    pub decision: Option<&'static str>,
    pub garbage_share: Option<f64>,
}

/// Score one curated page.
pub fn score_page(p: &CuratedPage) -> PageRow {
    let mut row = PageRow {
        doc_id: p.doc_id.clone(),
        unit_id: p.unit_id.clone(),
        package_id: p.package_id.clone(),
        serial: p.serial.clone(),
        seq: p.seq,
        date: p.date,
        text_status: p.text_status.clone(),
        text_chars: p.text_chars,
        skipped: Some(NO_TEXT),
        damage_rate: None,
        function_share: None,
        lang: None,
        decision: None,
        garbage_share: None,
    };
    let Some(text) = p.text.as_deref().filter(|_| p.indexable()) else {
        return row;
    };
    row.skipped = Some(BELOW_FLOOR);
    if text.chars().count() < MIN_CHARS {
        return row;
    }
    let Some(forced) = damage::score_forced(text, FORCED) else {
        return row;
    };
    let s = damage::score(text, &[CATALOG]);
    row.skipped = None;
    row.damage_rate = forced.damage_rate;
    row.function_share = forced.function_share;
    row.decision = Some(s.decision.as_str());
    row.garbage_share = s.garbage_share;
    row.lang = Some(s.lang);
    row
}

/// The page table's columns. Stable for a metric version: a change here is a
/// new [`damage::VERSION`].
pub fn schema() -> SchemaRef {
    let s = |n: &str, null: bool| Field::new(n, DataType::Utf8, null);
    let f = |n: &str| Field::new(n, DataType::Float64, true);
    Arc::new(Schema::new(vec![
        s("doc_id", false),
        s("unit_id", false),
        s("package_id", true),
        s("serial", true),
        Field::new("seq", DataType::UInt32, false),
        Field::new("date", DataType::Date32, false),
        s("text_status", false),
        Field::new("text_chars", DataType::Int32, false),
        s("skipped", true),
        f("damage_rate"),
        f("function_share"),
        s("lang", true),
        s("decision", true),
        f("garbage_share"),
    ]))
}

fn unix_days(d: NaiveDate) -> i32 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid");
    i32::try_from((d - epoch).num_days()).expect("dates fit in i32 days")
}

/// An attempt's page table, as Parquet.
pub fn encode_table(rows: &[PageRow]) -> anyhow::Result<Vec<u8>> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(6).expect("valid level"),
        ))
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), schema(), Some(props))?;
    for chunk in rows.chunks(8192) {
        let mut doc_id = StringBuilder::new();
        let mut unit_id = StringBuilder::new();
        let mut package_id = StringBuilder::new();
        let mut serial = StringBuilder::new();
        let mut seq = UInt32Builder::new();
        let mut date = Date32Builder::new();
        let mut status = StringBuilder::new();
        let mut chars = Int32Builder::new();
        let mut skipped = StringBuilder::new();
        let mut damage_rate = Float64Builder::new();
        let mut function_share = Float64Builder::new();
        let mut lang = StringBuilder::new();
        let mut decision = StringBuilder::new();
        let mut garbage_share = Float64Builder::new();
        for r in chunk {
            doc_id.append_value(&r.doc_id);
            unit_id.append_value(&r.unit_id);
            package_id.append_option(r.package_id.as_deref());
            serial.append_option(r.serial.as_deref());
            seq.append_value(r.seq);
            date.append_value(unix_days(r.date));
            status.append_value(&r.text_status);
            chars.append_value(r.text_chars);
            skipped.append_option(r.skipped);
            damage_rate.append_option(r.damage_rate);
            function_share.append_option(r.function_share);
            lang.append_option(r.lang.as_deref());
            decision.append_option(r.decision);
            garbage_share.append_option(r.garbage_share);
        }
        let cols: Vec<ArrayRef> = vec![
            Arc::new(doc_id.finish()),
            Arc::new(unit_id.finish()),
            Arc::new(package_id.finish()),
            Arc::new(serial.finish()),
            Arc::new(seq.finish()),
            Arc::new(date.finish()),
            Arc::new(status.finish()),
            Arc::new(chars.finish()),
            Arc::new(skipped.finish()),
            Arc::new(damage_rate.finish()),
            Arc::new(function_share.finish()),
            Arc::new(lang.finish()),
            Arc::new(decision.finish()),
            Arc::new(garbage_share.finish()),
        ];
        w.write(&RecordBatch::try_new(schema(), cols)?)?;
    }
    Ok(w.into_inner()?)
}

/// Damage rates in bins of 0.001 over `[0, 1]` (1.0 is in the last), kept
/// sparse, so a median is known to a thousandth after the summaries are summed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateHist {
    /// Pages by bin: bin `i` holds the rates from `i / 1000` up to `(i + 1) / 1000`.
    pub bins: BTreeMap<u16, u64>,
}

impl RateHist {
    const BINS: u16 = 1000;

    pub fn add(&mut self, rate: f64) {
        if !rate.is_finite() {
            return;
        }
        let bin = (rate.max(0.0) * f64::from(Self::BINS)) as u16;
        *self.bins.entry(bin.min(Self::BINS - 1)).or_default() += 1;
    }

    pub fn merge(&mut self, o: &RateHist) {
        for (bin, n) in &o.bins {
            *self.bins.entry(*bin).or_default() += n;
        }
    }

    pub fn total(&self) -> u64 {
        self.bins.values().sum()
    }

    /// The upper edge of the bin holding the `q` quantile; none when empty.
    pub fn quantile(&self, q: f64) -> Option<f64> {
        let n = self.total();
        if n == 0 {
            return None;
        }
        let target = (q * n as f64).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (bin, c) in &self.bins {
            seen += c;
            if seen >= target {
                return Some(f64::from(*bin + 1) / f64::from(Self::BINS));
            }
        }
        None
    }
}

/// Pages counted by what became of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    /// Every page: `no_text + below_floor + scored`.
    pub pages: u64,
    /// Pages whose text status isn't `ok`.
    pub no_text: u64,
    /// Pages with text under [`MIN_CHARS`].
    pub below_floor: u64,
    /// Pages scored.
    pub scored: u64,
    /// Scored pages with a damage rate.
    pub rated: u64,
    /// Rated pages above [`damage::DAMAGED`].
    pub damaged: u64,
    /// Rated pages above [`damage::BADLY_DAMAGED`].
    pub badly_damaged: u64,
    pub rates: RateHist,
}

impl Tally {
    fn add(&mut self, r: &PageRow) {
        self.pages += 1;
        match r.skipped {
            Some(NO_TEXT) => self.no_text += 1,
            Some(_) => self.below_floor += 1,
            None => {
                self.scored += 1;
                if let Some(d) = r.damage_rate {
                    self.rated += 1;
                    self.damaged += u64::from(d > damage::DAMAGED);
                    self.badly_damaged += u64::from(d > damage::BADLY_DAMAGED);
                    self.rates.add(d);
                }
            }
        }
    }

    fn merge(&mut self, o: &Tally) {
        self.pages += o.pages;
        self.no_text += o.no_text;
        self.below_floor += o.below_floor;
        self.scored += o.scored;
        self.rated += o.rated;
        self.damaged += o.damaged;
        self.badly_damaged += o.badly_damaged;
        self.rates.merge(&o.rates);
    }
}

/// The calendar decade of a date (1817 is in the 1810s, unlike
/// `usnm_core::decade`, which has no partition before 1820).
pub fn decade(date: NaiveDate) -> u16 {
    u16::try_from(date.year().div_euclid(10) * 10).unwrap_or(0)
}

/// An attempt's summary file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub metric_version: u32,
    pub min_chars: u32,
    pub series: String,
    pub unit: String,
    pub attempt: u32,
    /// The sha256 of the attempt's `counts.json`, which the table was scored from.
    pub counts_sha256: String,
    pub total: Tally,
    /// By the decade of each page's date.
    pub by_decade: BTreeMap<u16, Tally>,
    /// Scored pages by [`damage::Decision`] (`detected`, `mixed`, `und`).
    pub decisions: BTreeMap<String, u64>,
    /// Scored pages by detected language (`eng`, or `und`).
    pub languages: BTreeMap<String, u64>,
}

/// Summarize an attempt's pages.
pub fn summarize(u: &Unit, rows: &[PageRow]) -> Summary {
    let mut out = Summary {
        metric_version: damage::VERSION,
        min_chars: MIN_CHARS as u32,
        series: u.series.clone(),
        unit: u.unit.clone(),
        attempt: u.attempt,
        counts_sha256: u.counts_sha256.clone(),
        total: Tally::default(),
        by_decade: BTreeMap::new(),
        decisions: BTreeMap::new(),
        languages: BTreeMap::new(),
    };
    for r in rows {
        out.total.add(r);
        out.by_decade.entry(decade(r.date)).or_default().add(r);
        if let (Some(d), Some(l)) = (r.decision, &r.lang) {
            *out.decisions.entry(d.to_owned()).or_default() += 1;
            *out.languages.entry(l.clone()).or_default() += 1;
        }
    }
    out
}

/// A [`Tally`] with its shares and median, for reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rollup {
    pub pages: u64,
    pub no_text: u64,
    pub below_floor: u64,
    pub scored: u64,
    pub rated: u64,
    pub damaged: u64,
    pub badly_damaged: u64,
    /// `damaged / rated`.
    pub damaged_share: Option<f64>,
    /// `badly_damaged / rated`.
    pub badly_damaged_share: Option<f64>,
    /// The median damage rate of the rated pages, to a thousandth (the upper
    /// edge of its [`RateHist`] bin).
    pub damage_median: Option<f64>,
}

impl Rollup {
    pub fn of(t: &Tally) -> Self {
        let share = |n: u64| (t.rated > 0).then(|| n as f64 / t.rated as f64);
        Self {
            pages: t.pages,
            no_text: t.no_text,
            below_floor: t.below_floor,
            scored: t.scored,
            rated: t.rated,
            damaged: t.damaged,
            badly_damaged: t.badly_damaged,
            damaged_share: share(t.damaged),
            badly_damaged_share: share(t.badly_damaged),
            damage_median: t.rates.quantile(0.5),
        }
    }
}

/// `corpus/<key>.json`: the summaries of a set of attempts summed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Corpus {
    pub metric_version: u32,
    pub min_chars: u32,
    pub series: String,
    /// [`corpus_key`] of `attempts`.
    pub key: String,
    pub volumes: u64,
    pub total: Rollup,
    pub by_decade: BTreeMap<u16, Rollup>,
    pub decisions: BTreeMap<String, u64>,
    pub languages: BTreeMap<String, u64>,
    /// The attempts it covers: `[unit, attempt]`, by unit.
    pub attempts: Vec<(String, u32)>,
}

/// A name for a set of attempts: the first 16 hex digits of the sha256 of
/// their `unit/attempt` lines in unit order.
pub fn corpus_key(attempts: &[(String, u32)]) -> String {
    let mut sorted: Vec<&(String, u32)> = attempts.iter().collect();
    sorted.sort();
    let lines: String = sorted
        .iter()
        .map(|(unit, attempt)| format!("{unit}/{attempt:04}\n"))
        .collect();
    sha256_hex(lines.as_bytes())[..16].to_owned()
}

/// Sum attempt summaries (all of one series) into the corpus roll-up.
pub fn roll_up(series: &str, summaries: &[Summary]) -> Corpus {
    let mut total = Tally::default();
    let mut by_decade: BTreeMap<u16, Tally> = BTreeMap::new();
    let (mut decisions, mut languages) = (BTreeMap::new(), BTreeMap::new());
    let mut attempts = Vec::with_capacity(summaries.len());
    for s in summaries {
        total.merge(&s.total);
        for (d, t) in &s.by_decade {
            by_decade.entry(*d).or_default().merge(t);
        }
        for (k, n) in &s.decisions {
            *decisions.entry(k.clone()).or_default() += n;
        }
        for (k, n) in &s.languages {
            *languages.entry(k.clone()).or_default() += n;
        }
        attempts.push((s.unit.clone(), s.attempt));
    }
    attempts.sort();
    Corpus {
        metric_version: damage::VERSION,
        min_chars: MIN_CHARS as u32,
        series: series.to_owned(),
        key: corpus_key(&attempts),
        volumes: summaries.len() as u64,
        total: Rollup::of(&total),
        by_decade: by_decade.iter().map(|(d, t)| (*d, Rollup::of(t))).collect(),
        decisions,
        languages,
        attempts,
    }
}

/// An attempt's pages, scored: every part read and checked as the release
/// reads them.
async fn score_unit(store: &dyn ObjectStore, u: &Unit) -> anyhow::Result<Vec<PageRow>> {
    let mut rows = Vec::new();
    let mut with_text = 0u64;
    for part in &u.counts.parts {
        let pages = curated::read_checked(store, part).await?;
        with_text += pages.iter().filter(|p| p.indexable()).count() as u64;
        rows.extend(
            tokio::task::spawn_blocking(move || pages.iter().map(score_page).collect::<Vec<_>>())
                .await?,
        );
    }
    ensure!(
        rows.len() as u64 == u.counts.pages && with_text == u.counts.indexable,
        "the parts hold {} pages, {with_text} with text; counts.json says {} and {}",
        rows.len(),
        u.counts.pages,
        u.counts.indexable
    );
    Ok(rows)
}

/// A summary written earlier is kept only if it was made for this very
/// attempt, with this metric; else the run stops and says which files to
/// delete.
fn check_kept(path: &str, body: &[u8], u: &Unit) -> anyhow::Result<Summary> {
    let kept: Summary = serde_json::from_slice(body).with_context(|| format!("reading {path}"))?;
    if kept.metric_version != damage::VERSION
        || kept.min_chars != MIN_CHARS as u32
        || kept.series != u.series
        || kept.unit != u.unit
        || kept.attempt != u.attempt
        || kept.counts_sha256 != u.counts_sha256
    {
        bail!(
            "{path} was not written for this attempt and metric (it has metric version {}, a \
             floor of {} characters and counts.json sha256 {}; this run has {}, {MIN_CHARS} and \
             {}): delete it and {} to score the volume again",
            kept.metric_version,
            kept.min_chars,
            kept.counts_sha256,
            damage::VERSION,
            u.counts_sha256,
            table_path(&u.series, &u.unit, u.attempt)
        );
    }
    Ok(kept)
}

/// What became of one volume.
enum Outcome {
    /// Done by an earlier run.
    Kept(Summary),
    /// Scored and written now.
    Scored(Summary),
    /// The deadline came first.
    Left,
}

/// Write an attempt's table, then its summary. Create-only: a table that is
/// already there without a summary (an earlier run stopped between the two) is
/// kept when it is the same bytes, since the scoring is deterministic, and is
/// otherwise an error.
async fn write(
    out: &dyn ObjectStore,
    u: &Unit,
    table: Vec<u8>,
    summary: &Summary,
) -> anyhow::Result<()> {
    let path = table_path(&u.series, &u.unit, u.attempt);
    if !out.put_new(&path, table.clone(), PARQUET).await? {
        let there = out
            .get(&path)
            .await?
            .with_context(|| format!("{path} vanished"))?;
        if there != table {
            bail!(
                "{path} is there without its summary (an earlier run stopped between the two) \
                 and differs from this run's scores: delete it to score the volume again"
            );
        }
        tracing::warn!(
            unit = %u.unit,
            attempt = u.attempt,
            "the table was there without its summary and is the same; writing the summary"
        );
    }
    out.put_new(
        &summary_path(&u.series, &u.unit, u.attempt),
        serde_json::to_vec_pretty(summary)?,
        JSON,
    )
    .await?;
    Ok(())
}

async fn one(
    curated: &dyn ObjectStore,
    out: &dyn ObjectStore,
    u: &Unit,
    deadline: Option<Instant>,
) -> anyhow::Result<Outcome> {
    let path = summary_path(&u.series, &u.unit, u.attempt);
    if let Some(body) = out.get(&path).await? {
        return Ok(Outcome::Kept(check_kept(&path, &body, u)?));
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Ok(Outcome::Left);
    }
    let rows = score_unit(curated, u).await?;
    let unit = u.clone();
    let (table, summary) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok((encode_table(&rows)?, summarize(&unit, &rows)))
    })
    .await??;
    write(out, u, table, &summary).await?;
    Ok(Outcome::Scored(summary))
}

/// What a pass did.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct PassReport {
    pub series: String,
    /// Volumes the run covers.
    pub volumes: usize,
    /// Volumes scored and written by this run.
    pub scored: usize,
    /// Volumes an earlier run had done.
    pub skipped: usize,
    /// Volumes not started: the deadline came first.
    pub left: usize,
    /// Pages in the tables this run wrote.
    pub pages: u64,
    /// Units left out: a later empty attempt (or, for the Globe, another item)
    /// supersedes their finished one.
    pub left_out: Vec<String>,
    /// The roll-up, when the run covered every volume and finished them all.
    pub corpus: Option<CorpusReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CorpusReport {
    pub path: String,
    /// False when an earlier run had written it for the same attempts.
    pub written: bool,
    pub volumes: u64,
    pub total: Rollup,
    pub by_decade: BTreeMap<u16, Rollup>,
}

/// Score the series' volumes: `curated` is read, `out` written.
pub async fn run(
    curated: Arc<dyn ObjectStore>,
    out: Arc<dyn ObjectStore>,
    opts: &Options,
) -> anyhow::Result<PassReport> {
    let (all, left_out) = units(curated.as_ref(), &opts.series).await?;
    ensure!(
        !all.is_empty(),
        "the curated store has no finished {} volumes ({}/): nothing to score",
        opts.series,
        curated::series_dir(&opts.series)
    );
    let selected = select(&all, opts)?;
    let every_volume = selected.len() == all.len();
    let started = Instant::now();
    tracing::info!(
        series = %opts.series,
        volumes = selected.len(),
        left_out = left_out.len(),
        metric_version = damage::VERSION,
        "congress damage starting"
    );
    let mut report = PassReport {
        series: opts.series.clone(),
        volumes: selected.len(),
        left_out,
        ..PassReport::default()
    };
    let mut summaries = Vec::with_capacity(selected.len());
    let mut last_progress = Instant::now();
    let mut done = futures::stream::iter(selected)
        .map(|u| {
            let (curated, out) = (Arc::clone(&curated), Arc::clone(&out));
            async move {
                one(curated.as_ref(), out.as_ref(), u, opts.deadline)
                    .await
                    .with_context(|| format!("{}/{} attempt {}", u.series, u.unit, u.attempt))
            }
        })
        .buffer_unordered(opts.parallel.max(1));
    while let Some(outcome) = done.next().await {
        match outcome? {
            Outcome::Kept(s) => {
                report.skipped += 1;
                summaries.push(s);
            }
            Outcome::Scored(s) => {
                report.scored += 1;
                report.pages += s.total.pages;
                tracing::info!(
                    unit = %s.unit,
                    attempt = s.attempt,
                    pages = s.total.pages,
                    scored = s.total.scored,
                    damaged = s.total.damaged,
                    badly_damaged = s.total.badly_damaged,
                    "congress damage volume scored"
                );
                summaries.push(s);
            }
            Outcome::Left => report.left += 1,
        }
        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            let secs = started.elapsed().as_secs_f64().max(1.0);
            tracing::info!(
                volumes_done = report.scored + report.skipped,
                volumes = report.volumes,
                pages = report.pages,
                pages_per_sec = (report.pages as f64 / secs).round(),
                elapsed_s = secs.round(),
                "congress damage progress"
            );
        }
    }
    drop(done);
    if every_volume && report.left == 0 {
        let corpus = roll_up(&opts.series, &summaries);
        let path = corpus_path(&opts.series, &corpus.key);
        let written = out
            .put_new(&path, serde_json::to_vec_pretty(&corpus)?, JSON)
            .await?;
        tracing::info!(
            path = %path,
            written,
            volumes = corpus.volumes,
            pages = corpus.total.pages,
            scored = corpus.total.scored,
            damaged = corpus.total.damaged,
            badly_damaged = corpus.total.badly_damaged,
            damage_median = corpus.total.damage_median,
            "congress damage corpus"
        );
        report.corpus = Some(CorpusReport {
            path,
            written,
            volumes: corpus.volumes,
            total: corpus.total,
            by_decade: corpus.by_decade,
        });
    }
    tracing::info!(
        series = %opts.series,
        volumes = report.volumes,
        scored = report.scored,
        skipped = report.skipped,
        left = report.left,
        pages = report.pages,
        elapsed_s = started.elapsed().as_secs(),
        "congress damage finished"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;
    use bytes::Bytes;
    use chrono::{DateTime, Utc};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use usnm_congress::StateRef;
    use usnm_store::LocalStore;

    /// English prose of well over [`MIN_CHARS`], with the common words the
    /// damage rate counts.
    const CLEAN: &str = "The Committee on the Judiciary, to whom was referred the petition of \
        the inhabitants of the Territory, have had the same under consideration, and report \
        that the prayer of the petitioners is reasonable, and that it ought to be granted. \
        The Senate and the House of Representatives of the United States of America in \
        Congress assembled have power to make all needful rules and regulations respecting \
        the territory belonging to the United States.";
    /// The same prose with the common words one OCR edit off.
    const DAMAGED: &str = "Tbe Comrnittee on tbe Judiciary, to wbom was referrcd tbe petition of \
        tbe inbabitants of tbe Territory, bave bad tbe sarne under consideration, aud report \
        tbat tbe prayer of tbe petitioners is reasonable, aud tbat it ougbt to be granted. \
        Tbe Senate aud tbe House of Representatives of tbe United States of Arnerica in \
        Congress assernbled bave power to rnake all needful rules aud regulations respecting \
        tbe territory belonging to tbe United States.";

    fn page(unit: &str, seq: u32, year: i32, text: Option<&str>) -> CuratedPage {
        let status = match text {
            Some(t) if t.chars().count() >= 20 => "ok",
            Some(_) => "short",
            None => "empty",
        };
        CuratedPage {
            doc_id: format!("SERIALSET-{unit}-001-0001-0000_p{seq}"),
            source: "govinfo".into(),
            series: "serialset".into(),
            unit_id: unit.into(),
            unit_ordinal: 1,
            package_id: Some(format!("SERIALSET-{unit}-001-0001-0000")),
            seq,
            printed_page: None,
            appendix: false,
            date: NaiveDate::from_ymd_opt(year, 3, 4).unwrap(),
            date_precision: "day".into(),
            date_from: "date_issued".into(),
            congress: Some(16),
            session: Some("1".into()),
            chamber: Some("house".into()),
            doc_class: None,
            doc_number: None,
            citation: None,
            serial: Some("33".into()),
            title: None,
            text_status: status.into(),
            text: (status == "ok").then(|| text.unwrap().to_owned()),
            text_chars: text.map_or(0, |t| t.chars().count() as i32),
            text_sha256: [0; 32],
            ocr_source: "govinfo".into(),
            ocr_engine: None,
            link: None,
            states: Vec::<StateRef>::new(),
            places: vec![],
            speakers: vec![],
            fetched_at: DateTime::<Utc>::from_timestamp(1_790_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn scores_a_page_as_the_issue_sample_did() {
        let r = score_page(&page("00033_00_00", 1, 1820, Some(CLEAN)));
        let forced = damage::score_forced(CLEAN, "eng").unwrap();
        assert_eq!(r.skipped, None);
        assert_eq!(r.damage_rate, forced.damage_rate);
        assert_eq!(r.function_share, forced.function_share);
        assert!(r.damage_rate.unwrap() < damage::DAMAGED);
        let s = damage::score(CLEAN, &["eng"]);
        assert_eq!(r.lang.as_deref(), Some(s.lang.as_str()));
        assert_eq!(r.decision, Some(s.decision.as_str()));
        assert_eq!(r.garbage_share, s.garbage_share);
        assert_eq!(r.doc_id, "SERIALSET-00033_00_00-001-0001-0000_p1");
        assert_eq!(r.serial.as_deref(), Some("33"));
        // Deterministic.
        assert_eq!(r, score_page(&page("00033_00_00", 1, 1820, Some(CLEAN))));
        let bad = score_page(&page("00033_00_00", 2, 1820, Some(DAMAGED)));
        assert!(
            bad.damage_rate.unwrap() > damage::BADLY_DAMAGED,
            "{:?}",
            bad.damage_rate
        );
    }

    #[test]
    fn keeps_pages_it_does_not_score() {
        // A page with no text, and one with a stub of text, have no text.
        for text in [None, Some("12")] {
            let r = score_page(&page("00033_00_00", 1, 1820, text));
            assert_eq!(r.skipped, Some(NO_TEXT));
            assert_eq!((r.damage_rate, r.lang, r.decision), (None, None, None));
            assert_eq!(r.text_chars, text.map_or(0, |t| t.len() as i32));
        }
        // Text under the floor is kept with its length and no scores.
        let short = &CLEAN[..MIN_CHARS - 1];
        let r = score_page(&page("00033_00_00", 1, 1820, Some(short)));
        assert_eq!(r.skipped, Some(BELOW_FLOOR));
        assert_eq!(r.text_status, "ok");
        assert_eq!(r.text_chars as usize, MIN_CHARS - 1);
        assert_eq!(
            (r.damage_rate, r.function_share, r.garbage_share, r.lang),
            (None, None, None, None)
        );
        // At the floor it is scored.
        let r = score_page(&page("00033_00_00", 1, 1820, Some(&CLEAN[..MIN_CHARS])));
        assert_eq!(r.skipped, None);
    }

    #[test]
    fn counts_characters_not_bytes() {
        // 199 two-byte characters are under the floor; 200 are not.
        let under = "é".repeat(MIN_CHARS - 1);
        let at = "é".repeat(MIN_CHARS);
        assert_eq!(
            score_page(&page("u", 1, 1820, Some(&under))).skipped,
            Some(BELOW_FLOOR)
        );
        assert_eq!(score_page(&page("u", 1, 1820, Some(&at))).skipped, None);
    }

    fn unit(name: &str, attempt: u32) -> Unit {
        Unit {
            series: "serialset".into(),
            unit: name.into(),
            attempt,
            counts: Counts::of(
                "serialset",
                "govinfo",
                name,
                attempt,
                &[],
                vec![],
                DateTime::<Utc>::from_timestamp(1_790_000_000, 0).unwrap(),
            ),
            counts_sha256: "ab".repeat(32),
        }
    }

    #[test]
    fn the_summary_adds_up() {
        let pages = [
            page("00033_00_00", 1, 1817, Some(CLEAN)),
            page("00033_00_00", 2, 1819, Some(DAMAGED)),
            page("00033_00_00", 3, 1820, Some(CLEAN)),
            page("00033_00_00", 4, 1829, Some(&CLEAN[..50])),
            page("00033_00_00", 5, 1829, None),
            page("00033_00_00", 6, 1830, Some(DAMAGED)),
        ];
        let rows: Vec<PageRow> = pages.iter().map(score_page).collect();
        let s = summarize(&unit("00033_00_00", 2), &rows);
        let t = &s.total;
        assert_eq!(
            (t.pages, t.no_text, t.below_floor, t.scored),
            (6, 1, 1, 4),
            "{t:?}"
        );
        assert_eq!(t.pages, t.no_text + t.below_floor + t.scored);
        assert_eq!((t.rated, t.damaged, t.badly_damaged), (4, 2, 2));
        // 1817 and 1819 are the 1810s; 1829 the 1820s with 1820.
        assert_eq!(
            s.by_decade.keys().copied().collect::<Vec<_>>(),
            [1810, 1820, 1830]
        );
        let d = &s.by_decade[&1820];
        assert_eq!((d.pages, d.scored, d.below_floor, d.no_text), (3, 1, 1, 1));
        assert_eq!(
            (s.by_decade[&1810].damaged, s.by_decade[&1830].damaged),
            (1, 1)
        );
        // The decades add up to the total.
        let mut sum = Tally::default();
        s.by_decade.values().for_each(|d| sum.merge(d));
        assert_eq!(&sum, t);
        assert_eq!(s.decisions.values().sum::<u64>(), t.scored);
        assert_eq!(s.languages.values().sum::<u64>(), t.scored);
        assert_eq!(t.rates.total(), t.rated);
        let back: Summary = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn rates_are_binned_to_a_thousandth() {
        let mut h = RateHist::default();
        for x in [0.0, 0.0004, 0.009, 0.0095, 0.1, 0.25, 1.0, 1.5, f64::NAN] {
            h.add(x);
        }
        assert_eq!(h.total(), 8);
        assert_eq!(h.bins[&0], 2);
        assert_eq!(h.bins[&9], 2);
        assert_eq!(h.bins[&999], 2, "1.0 and above are in the last bin");
        assert_eq!(h.quantile(0.5), Some(0.01));
        assert_eq!(h.quantile(1.0), Some(1.0));
        assert_eq!(RateHist::default().quantile(0.5), None);
        // A median known to a thousandth: the issue's sample read 0.009.
        let mut m = RateHist::default();
        [0.002, 0.0085, 0.2].iter().for_each(|x| m.add(*x));
        assert_eq!(m.quantile(0.5), Some(0.009));
        let mut both = h.clone();
        both.merge(&m);
        assert_eq!(both.total(), 11);
    }

    #[test]
    fn rolls_up_the_summaries() {
        let rows = |unit: &str, years: &[i32]| -> Vec<PageRow> {
            years
                .iter()
                .enumerate()
                .map(|(i, y)| {
                    let text = if i % 2 == 0 { CLEAN } else { DAMAGED };
                    score_page(&page(unit, i as u32 + 1, *y, Some(text)))
                })
                .collect()
        };
        let a = summarize(&unit("00001_00_00", 1), &rows("00001_00_00", &[1818, 1821]));
        let b = summarize(
            &unit("00002_00_00", 3),
            &rows("00002_00_00", &[1821, 1822, 1823]),
        );
        let c = roll_up("serialset", &[b.clone(), a.clone()]);
        assert_eq!(c.volumes, 2);
        assert_eq!(c.total.pages, 5);
        assert_eq!(c.total.scored, 5);
        assert_eq!(c.total.damaged, a.total.damaged + b.total.damaged);
        assert_eq!(c.total.damaged_share, Some(2.0 / 5.0));
        assert_eq!(c.by_decade[&1820].pages, 4);
        assert_eq!(c.by_decade[&1810].pages, 1);
        assert_eq!(
            c.attempts,
            [("00001_00_00".to_owned(), 1), ("00002_00_00".to_owned(), 3)]
        );
        // Order of the summaries doesn't matter; the set of attempts does.
        assert_eq!(c, roll_up("serialset", &[a.clone(), b.clone()]));
        let b2 = summarize(&unit("00002_00_00", 4), &rows("00002_00_00", &[1821]));
        assert_ne!(c.key, roll_up("serialset", &[a, b2]).key);
        assert_eq!(c.key.len(), 16);
        let back: Corpus = serde_json::from_slice(&serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn the_table_round_trips_with_the_schema() {
        let rows: Vec<PageRow> = [Some(CLEAN), Some(&CLEAN[..40]), None]
            .iter()
            .enumerate()
            .map(|(i, t)| score_page(&page("00033_00_00", i as u32 + 1, 1820, *t)))
            .collect();
        let bytes = encode_table(&rows).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes.clone())).unwrap();
        assert_eq!(reader.schema().as_ref(), schema().as_ref());
        let batches: Vec<_> = reader.build().unwrap().map(|b| b.unwrap()).collect();
        assert_eq!(batches[0].num_rows(), 3);
        let col = |name: &str| batches[0].column_by_name(name).unwrap().clone();
        let rate = col("damage_rate");
        let rate = rate
            .as_any()
            .downcast_ref::<arrow_array::Float64Array>()
            .unwrap();
        assert_eq!(rate.value(0), rows[0].damage_rate.unwrap());
        assert!(rate.is_null(1) && rate.is_null(2));
        let skipped = col("skipped");
        let skipped = skipped
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .unwrap();
        assert!(skipped.is_null(0));
        assert_eq!(skipped.value(1), BELOW_FLOOR);
        assert_eq!(skipped.value(2), NO_TEXT);
        assert!(col("lang").is_null(1) && col("lang").is_null(2));
        // The same rows encode to the same bytes, which a resumed run relies on.
        assert_eq!(bytes, encode_table(&rows).unwrap());
    }

    #[test]
    fn paths_stay_beside_the_newspaper_tables() {
        assert_eq!(
            table_path("serialset", "00033_00_00", 2),
            "audit/damage/v2/congress/serialset/00033_00_00/0002.parquet"
        );
        assert_eq!(
            summary_path("serialset", "00033_00_00", 12),
            "audit/damage/v2/congress/serialset/00033_00_00/0012.summary.json"
        );
        assert_eq!(
            corpus_path("serialset", "0123456789abcdef"),
            "audit/damage/v2/congress/serialset/corpus/0123456789abcdef.json"
        );
        for p in [
            table_path("serialset", "00033_00_00", 1),
            summary_path("globe", "metadc29274", 1),
            corpus_path("serialset", &corpus_key(&[("u".into(), 1)])),
        ] {
            usnm_store::validate_path(&p).unwrap();
            // The newspaper tables are `<batch>_ver<NN>.parquet` in this
            // directory; nothing here looks like one to the readers that
            // list it.
            assert!(!p.contains("_ver"), "{p}");
            assert!(p.starts_with("audit/damage/"), "{p}");
            assert!(!p.starts_with("congress/"), "{p}");
        }
    }

    #[test]
    fn names_the_volume_by_its_serial() {
        assert_eq!(serial_of("00033_00_00"), Some(33));
        assert_eq!(serial_of("13993_00_00"), Some(13993));
        assert_eq!(serial_of("metadc29274"), None);
    }

    #[tokio::test]
    async fn an_empty_store_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(LocalStore::new(dir.path()));
        let opts = Options {
            series: "serialset".into(),
            parallel: 1,
            ..Options::default()
        };
        let e = run(Arc::clone(&store), Arc::clone(&store), &opts)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("no finished serialset volumes"), "{e}");
        let opts = Options {
            series: "../x".into(),
            ..opts
        };
        let e = run(Arc::clone(&store), store, &opts)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown Congress series"), "{e}");
    }
}
