//! The ledger of pages asked for and what became of them (12 §12.5 Step 8).
//!
//! Every message a cloud job puts on a work queue gets a row in a
//! create-only `ocr-local/queued/<day>/<run>.parquet` ([`Queued`]):
//! `ocr-enqueue` and `ocr-requeue` here, the seeder (Step 10) and the
//! requests step (Step 12) later. `ocr-sync` writes what became of them to
//! `ocr-local/outcomes/<day>/<run>.parquet` ([`Outcome`]): `imported` for a
//! part it imported, `dead_lettered` with the cause and kind for a dead
//! letter it drained. A queue job that fails to put a message writes
//! `not_sent` for the pages after it in that run, which it never sent.
//!
//! A page is outstanding while its latest queued row has no outcome after
//! it, or its latest outcome is a dead letter with cause `unroutable`,
//! which the seeder queues again ([`super::seed`]). `ocr-sync` imports a part only for an
//! outstanding page, with the batch and reason the ledger has for it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{bail, Context};
use arrow_array::builder::{
    Int16Builder, Int32Builder, StringBuilder, TimestampMicrosecondBuilder,
};
use arrow_array::{
    Array, ArrayRef, Int16Array, Int32Array, RecordBatch, StringArray, TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use usnm_store::ObjectStore;

use super::{from_micros, outcomes_path, queued_path, ROOT};

/// One message put on a work queue.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    pub doc_id: String,
    /// The base batch name, as in the message.
    pub batch: String,
    pub batch_version: u16,
    /// `bulk` or `user`.
    pub reason: String,
    /// ISO 639-3, as in the message.
    pub language: String,
    /// The seeding plan, or who asked (`ocr-enqueue`'s `--plan`).
    pub plan: String,
    /// 1 for the first time the page is queued under this plan.
    pub attempt: u32,
    /// The UTC request day, for the daily cap. Queue ordering uses `queued_at`.
    pub request_day: Option<chrono::NaiveDate>,
    pub queued_at: DateTime<Utc>,
    /// Exact message time for newly queued work, including BOINC retries.
    /// Legacy rows omit it; queue time can differ from ledger time.
    pub retry_message_at: Option<DateTime<Utc>>,
    pub retry_run: Option<String>,
}

pub const IMPORTED: &str = "imported";
pub const DEAD_LETTERED: &str = "dead_lettered";
/// A page whose ledger row was written but whose message was never put.
pub const NOT_SENT: &str = "not_sent";
/// The dead-letter cause for a page no node could route.
pub const UNROUTABLE: &str = "unroutable";

/// What became of a queued page.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub doc_id: String,
    /// [`IMPORTED`], [`DEAD_LETTERED`] or [`NOT_SENT`].
    pub outcome: String,
    /// A dead letter's cause (`dequeue_count`, `invalid`, `permanent`,
    /// `unroutable`); none for a v1 dead letter or an import.
    pub cause: Option<String>,
    pub kind: Option<String>,
    /// The part's path in `ocr-out`, or the dead letter's message id.
    pub source: String,
    pub at: DateTime<Utc>,
    pub run: String,
    pub message_sha256: Option<String>,
}

fn queued_schema() -> SchemaRef {
    let s = |n: &str| Field::new(n, DataType::Utf8, false);
    Arc::new(Schema::new(vec![
        s("doc_id"),
        s("batch"),
        Field::new("batch_version", DataType::Int16, false),
        s("reason"),
        s("language"),
        s("plan"),
        Field::new("attempt", DataType::Int32, false),
        Field::new("request_day", DataType::Utf8, true),
        Field::new(
            "queued_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "retry_message_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        ),
        Field::new("retry_run", DataType::Utf8, true),
    ]))
}

fn outcome_schema() -> SchemaRef {
    let s = |n: &str, null: bool| Field::new(n, DataType::Utf8, null);
    Arc::new(Schema::new(vec![
        s("doc_id", false),
        s("outcome", false),
        s("cause", true),
        s("kind", true),
        s("source", false),
        Field::new(
            "at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        s("run", false),
        s("message_sha256", true),
    ]))
}

fn write(schema: SchemaRef, cols: Vec<ArrayRef>) -> anyhow::Result<Vec<u8>> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).expect("valid level"),
        ))
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), schema.clone(), Some(props))?;
    w.write(&RecordBatch::try_new(schema, cols)?)?;
    Ok(w.into_inner()?)
}

pub fn encode_queued(rows: &[Queued]) -> anyhow::Result<Vec<u8>> {
    let mut doc_id = StringBuilder::new();
    let mut batch = StringBuilder::new();
    let mut version = Int16Builder::new();
    let mut reason = StringBuilder::new();
    let mut language = StringBuilder::new();
    let mut plan = StringBuilder::new();
    let mut attempt = Int32Builder::new();
    let mut request_day = StringBuilder::new();
    let mut at = TimestampMicrosecondBuilder::new().with_timezone("UTC");
    let mut retry_at = TimestampMicrosecondBuilder::new().with_timezone("UTC");
    let mut retry_run = StringBuilder::new();
    for r in rows {
        doc_id.append_value(&r.doc_id);
        batch.append_value(&r.batch);
        version.append_value(i16::try_from(r.batch_version).context("batch version")?);
        reason.append_value(&r.reason);
        language.append_value(&r.language);
        plan.append_value(&r.plan);
        attempt.append_value(i32::try_from(r.attempt).unwrap_or(i32::MAX));
        request_day.append_option(r.request_day.map(|d| d.format("%Y-%m-%d").to_string()));
        at.append_value(r.queued_at.timestamp_micros());
        retry_at.append_option(r.retry_message_at.map(|at| at.timestamp_micros()));
        retry_run.append_option(r.retry_run.as_deref());
    }
    write(
        queued_schema(),
        vec![
            Arc::new(doc_id.finish()),
            Arc::new(batch.finish()),
            Arc::new(version.finish()),
            Arc::new(reason.finish()),
            Arc::new(language.finish()),
            Arc::new(plan.finish()),
            Arc::new(attempt.finish()),
            Arc::new(request_day.finish()),
            Arc::new(at.finish()),
            Arc::new(retry_at.finish()),
            Arc::new(retry_run.finish()),
        ],
    )
}

pub fn encode_outcomes(rows: &[Outcome]) -> anyhow::Result<Vec<u8>> {
    let mut doc_id = StringBuilder::new();
    let mut outcome = StringBuilder::new();
    let mut cause = StringBuilder::new();
    let mut kind = StringBuilder::new();
    let mut source = StringBuilder::new();
    let mut at = TimestampMicrosecondBuilder::new().with_timezone("UTC");
    let mut run = StringBuilder::new();
    let mut message = StringBuilder::new();
    for r in rows {
        doc_id.append_value(&r.doc_id);
        outcome.append_value(&r.outcome);
        cause.append_option(r.cause.as_deref());
        kind.append_option(r.kind.as_deref());
        source.append_value(&r.source);
        at.append_value(r.at.timestamp_micros());
        run.append_value(&r.run);
        message.append_option(r.message_sha256.as_deref());
    }
    write(
        outcome_schema(),
        vec![
            Arc::new(doc_id.finish()),
            Arc::new(outcome.finish()),
            Arc::new(cause.finish()),
            Arc::new(kind.finish()),
            Arc::new(source.finish()),
            Arc::new(at.finish()),
            Arc::new(run.finish()),
            Arc::new(message.finish()),
        ],
    )
}

macro_rules! col {
    ($batch:expr, $name:expr, $t:ty) => {
        $batch
            .column_by_name($name)
            .with_context(|| format!("ledger file has no `{}` column", $name))?
            .as_any()
            .downcast_ref::<$t>()
            .with_context(|| format!("column `{}` has an unexpected type", $name))?
    };
}

pub fn decode_queued(bytes: Bytes) -> anyhow::Result<Vec<Queued>> {
    let mut out = Vec::new();
    for batch in ParquetRecordBatchReaderBuilder::try_new(bytes)?.build()? {
        let b = batch?;
        let doc_id = col!(b, "doc_id", StringArray);
        let batch_name = col!(b, "batch", StringArray);
        let version = col!(b, "batch_version", Int16Array);
        let reason = col!(b, "reason", StringArray);
        let language = col!(b, "language", StringArray);
        let plan = col!(b, "plan", StringArray);
        let attempt = col!(b, "attempt", Int32Array);
        let request_day = b
            .column_by_name("request_day")
            .map(|column| {
                column
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .context("column `request_day` has an unexpected type")
            })
            .transpose()?;
        let at = col!(b, "queued_at", TimestampMicrosecondArray);
        let retry_run = b
            .column_by_name("retry_run")
            .map(|column| {
                column
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .context("column `retry_run` has an unexpected type")
            })
            .transpose()?;
        let retry_at = b
            .column_by_name("retry_message_at")
            .map(|column| {
                column
                    .as_any()
                    .downcast_ref::<TimestampMicrosecondArray>()
                    .context("column `retry_message_at` has an unexpected type")
            })
            .transpose()?;
        for i in 0..b.num_rows() {
            let request_day = request_day
                .filter(|days| !days.is_null(i))
                .map(|days| {
                    chrono::NaiveDate::parse_from_str(days.value(i), "%Y-%m-%d")
                        .with_context(|| format!("invalid request day `{}`", days.value(i)))
                })
                .transpose()?;
            out.push(Queued {
                doc_id: doc_id.value(i).to_owned(),
                batch: batch_name.value(i).to_owned(),
                batch_version: u16::try_from(version.value(i)).unwrap_or(0),
                reason: reason.value(i).to_owned(),
                language: language.value(i).to_owned(),
                plan: plan.value(i).to_owned(),
                attempt: u32::try_from(attempt.value(i)).unwrap_or(0),
                request_day,
                queued_at: from_micros(at.value(i)),
                retry_message_at: retry_at
                    .filter(|times| !times.is_null(i))
                    .map(|times| from_micros(times.value(i))),
                retry_run: retry_run
                    .filter(|runs| !runs.is_null(i))
                    .map(|runs| runs.value(i).to_owned()),
            });
        }
    }
    Ok(out)
}

pub fn decode_outcomes(bytes: Bytes) -> anyhow::Result<Vec<Outcome>> {
    let mut out = Vec::new();
    for batch in ParquetRecordBatchReaderBuilder::try_new(bytes)?.build()? {
        let b = batch?;
        let doc_id = col!(b, "doc_id", StringArray);
        let outcome = col!(b, "outcome", StringArray);
        let cause = col!(b, "cause", StringArray);
        let kind = col!(b, "kind", StringArray);
        let source = col!(b, "source", StringArray);
        let at = col!(b, "at", TimestampMicrosecondArray);
        let run = col!(b, "run", StringArray);
        let message = b
            .column_by_name("message_sha256")
            .map(|column| {
                column
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .context("column `message_sha256` has an unexpected type")
            })
            .transpose()?;
        let opt = |a: &StringArray, i: usize| (!a.is_null(i)).then(|| a.value(i).to_owned());
        for i in 0..b.num_rows() {
            out.push(Outcome {
                doc_id: doc_id.value(i).to_owned(),
                outcome: outcome.value(i).to_owned(),
                cause: opt(cause, i),
                kind: opt(kind, i),
                source: source.value(i).to_owned(),
                at: from_micros(at.value(i)),
                run: run.value(i).to_owned(),
                message_sha256: message.and_then(|m| opt(m, i)),
            });
        }
    }
    Ok(out)
}

/// The ledger's latest state per page.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    message_times: HashMap<(String, chrono::NaiveDate), DateTime<Utc>>,
    /// Each page's latest queued row.
    pub queued: HashMap<String, Queued>,
    /// Each page's latest outcome.
    pub outcomes: HashMap<String, Outcome>,
    /// How often each page was queued under each plan.
    pub attempts: HashMap<(String, String), u32>,
    /// The dead letters with cause `unroutable` each page has had, by
    /// message id (a dead letter drained twice counts once): the seeder
    /// queues those pages again outside its retry limit, waiting longer
    /// each time (Step 10).
    pub unroutable: HashMap<String, HashSet<String>>,
    /// Pages already imported at least once, even if a later attempt has a
    /// newer outcome.
    pub imported_pages: HashSet<String>,
    /// Distinct page ids queued by hit requests on each UTC request day.
    pub user_requests_by_day: HashMap<chrono::NaiveDate, HashSet<(String, String)>>,
    /// Request queue attempts explicitly known not to have been sent.
    pub not_sent_attempts: HashSet<(String, String)>,
}

impl Ledger {
    pub fn add_queued(&mut self, q: Queued) {
        self.add_queued_from_run(q, "");
    }

    fn add_queued_from_run(&mut self, q: Queued, run: &str) {
        let at = q.retry_message_at.unwrap_or(q.queued_at);
        self.message_times
            .entry((q.doc_id.clone(), at.date_naive()))
            .and_modify(|previous| *previous = (*previous).max(at))
            .or_insert(at);
        if q.plan == "hit-results" {
            let day = q.request_day.unwrap_or_else(|| q.queued_at.date_naive());
            self.user_requests_by_day
                .entry(day)
                .or_default()
                .insert((q.doc_id.clone(), run.to_owned()));
        }
        let n = self
            .attempts
            .entry((q.doc_id.clone(), q.plan.clone()))
            .or_default();
        *n = (*n).max(q.attempt);
        match self.queued.get(&q.doc_id) {
            Some(old) if old.queued_at > q.queued_at => {}
            _ => {
                self.queued.insert(q.doc_id.clone(), q);
            }
        }
    }

    /// Mint a distinct microsecond fence across retained reservations, including
    /// other plans and historical rows. Closed hit requests keep their UTC day.
    pub fn next_message_time(
        &self,
        doc_id: &str,
        at: DateTime<Utc>,
    ) -> anyhow::Result<DateTime<Utc>> {
        let day = at.date_naive();
        let mut next = DateTime::from_timestamp_micros(at.timestamp_micros())
            .context("message timestamp is out of range")?;
        if let Some(previous) = self.message_times.get(&(doc_id.to_owned(), day)) {
            if *previous >= next {
                next = previous
                    .checked_add_signed(chrono::Duration::microseconds(1))
                    .context("message timestamp exhausted")?;
            }
        }
        // Historical producers emitted whole seconds, including request-day midnight.
        if next.timestamp_subsec_micros() == 0 {
            next = next
                .checked_add_signed(chrono::Duration::microseconds(1))
                .context("message timestamp exhausted")?;
        }
        anyhow::ensure!(
            next.date_naive() == day,
            "message identities exhausted for UTC day"
        );
        Ok(next)
    }

    pub fn add_outcome(&mut self, o: Outcome) {
        if o.outcome == NOT_SENT {
            self.not_sent_attempts
                .insert((o.doc_id.clone(), o.run.clone()));
        }
        if o.outcome == IMPORTED {
            self.imported_pages.insert(o.doc_id.clone());
        }
        if o.outcome == DEAD_LETTERED && o.cause.as_deref() == Some(UNROUTABLE) {
            self.unroutable
                .entry(o.doc_id.clone())
                .or_default()
                .insert(o.source.clone());
        }
        match self.outcomes.get(&o.doc_id) {
            Some(old) if old.at > o.at => {}
            _ => {
                self.outcomes.insert(o.doc_id.clone(), o);
            }
        }
    }

    /// The page's latest queued row while it is outstanding.
    pub fn outstanding(&self, doc_id: &str) -> Option<&Queued> {
        let q = self.queued.get(doc_id)?;
        match self.outcomes.get(doc_id) {
            None => Some(q),
            Some(o) if o.at < q.queued_at => Some(q),
            Some(o) if o.outcome == DEAD_LETTERED && o.cause.as_deref() == Some(UNROUTABLE) => {
                Some(q)
            }
            Some(_) => None,
        }
    }

    /// How many distinct `unroutable` dead letters the page has had.
    pub fn unroutable_cycles(&self, doc_id: &str) -> u32 {
        self.unroutable
            .get(doc_id)
            .map_or(0, |s| u32::try_from(s.len()).unwrap_or(u32::MAX))
    }

    /// Whether the page's latest outcome is an import.
    pub fn imported(&self, doc_id: &str) -> bool {
        self.imported_pages.contains(doc_id)
    }

    /// Pages with at least one attempt still charged to this UTC day.
    pub fn charged_user_requests(&self, day: chrono::NaiveDate) -> HashSet<&str> {
        self.user_requests_by_day
            .get(&day)
            .into_iter()
            .flatten()
            .filter(|attempt| !self.not_sent_attempts.contains(*attempt))
            .map(|(doc_id, _)| doc_id.as_str())
            .collect()
    }

    /// Distinct user-requested pages already charged to this UTC day's cap.
    pub fn user_requests_for_day(&self, day: chrono::NaiveDate) -> usize {
        self.charged_user_requests(day).len()
    }

    /// Pages outstanding now.
    pub fn outstanding_count(&self) -> usize {
        self.queued
            .keys()
            .filter(|d| self.outstanding(d).is_some())
            .count()
    }
}

/// Read the whole ledger.
pub async fn load(curated: &dyn ObjectStore) -> anyhow::Result<Ledger> {
    let mut ledger = Ledger::default();
    for path in curated.list(&format!("{ROOT}/queued")).await? {
        if !path.ends_with(".parquet") {
            continue;
        }
        let bytes = curated
            .get(&path)
            .await?
            .with_context(|| format!("{path} vanished"))?;
        let run = path
            .rsplit('/')
            .next()
            .and_then(|name| name.strip_suffix(".parquet"))
            .unwrap_or_default();
        for q in decode_queued(Bytes::from(bytes)).with_context(|| format!("reading {path}"))? {
            ledger.add_queued_from_run(q, run);
        }
    }
    for path in curated.list(&format!("{ROOT}/outcomes")).await? {
        if !path.ends_with(".parquet") {
            continue;
        }
        let bytes = curated
            .get(&path)
            .await?
            .with_context(|| format!("{path} vanished"))?;
        for o in decode_outcomes(Bytes::from(bytes)).with_context(|| format!("reading {path}"))? {
            ledger.add_outcome(o);
        }
    }
    Ok(ledger)
}

/// Write queued rows as one create-only file; `None` for no rows.
pub async fn write_queued(
    curated: &dyn ObjectStore,
    rows: &[Queued],
    at: DateTime<Utc>,
    run: &str,
) -> anyhow::Result<Option<String>> {
    if rows.is_empty() {
        return Ok(None);
    }
    let path = queued_path(at, run);
    if !curated
        .put_new(
            &path,
            encode_queued(rows)?,
            "application/vnd.apache.parquet",
        )
        .await?
    {
        bail!("{path} already exists");
    }
    Ok(Some(path))
}

/// Write outcome rows as one create-only file; `None` for no rows.
pub async fn write_outcomes(
    curated: &dyn ObjectStore,
    rows: &[Outcome],
    at: DateTime<Utc>,
    run: &str,
) -> anyhow::Result<Option<String>> {
    if rows.is_empty() {
        return Ok(None);
    }
    let path = outcomes_path(at, run);
    if !curated
        .put_new(
            &path,
            encode_outcomes(rows)?,
            "application/vnd.apache.parquet",
        )
        .await?
    {
        bail!("{path} already exists");
    }
    Ok(Some(path))
}
