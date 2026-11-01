//! Per-step throughput (#704): hourly counters for each pipeline step, kept
//! in a small file so the history outlives a restart.
//!
//! Three collectors (`scripts/ocr-node/throughput-collect.py`) read what
//! their hosts already record and `PUT /api/throughput/<source>`, each from
//! its own address only; the fetch index gives the scan fetch here. A
//! collector recomputes whole hours, so a bucket is replaced, never added to.
//! An hour nobody observed has no value and shows as unknown, never zero. No
//! page text and no page ids: only counts and seconds.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub const HOUR: i64 = 3600;
/// Hours kept: two weeks.
pub const KEEP_HOURS: i64 = 336;
/// The hours the charts show: a week, in bars of [`BIN_HOURS`].
pub const CHART_HOURS: i64 = 168;
pub const BIN_HOURS: i64 = 4;
pub const MAX_SERIES: usize = 256;
pub const MAX_LIMIT_CHARS: usize = 160;
/// The largest history file read; a larger one isn't ours.
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// A source that hasn't reported for this long is shown as stale.
pub const SOURCE_FRESH: Duration = Duration::minutes(45);
pub const SOURCES: [&str; 3] = ["controller", "ship", "boinc"];
/// The source the dashboard reads for itself.
pub const INDEX: &str = "index";
/// The hours the zoom shows, one bar each, and that the rates cover.
pub const DAY_HOURS: i64 = 24;
/// Reasons listed for a step.
const MAX_REASONS: usize = 8;
/// Reasons listed for the scan fetch.
pub const FETCH_REASONS: usize = MAX_REASONS;

/// What a collector sends.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    pub schema: u32,
    pub source: String,
    pub observed_at: DateTime<Utc>,
    pub first_hour: i64,
    pub hours: usize,
    pub series: BTreeMap<String, Vec<Option<f64>>>,
    #[serde(default)]
    pub limits: BTreeMap<String, String>,
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

/// A pushed document, checked. A push from a source's own address is still
/// outside input: a bad one changes nothing.
pub fn parse(body: &[u8], source: &str, now: DateTime<Utc>) -> anyhow::Result<Packet> {
    let p: Packet = serde_json::from_slice(body).context("malformed throughput document")?;
    ensure!(p.schema == 1, "unsupported schema");
    ensure!(p.source == source, "the source isn't the one addressed");
    ensure!(
        p.observed_at - now <= Duration::seconds(60),
        "observed_at is in the future"
    );
    ensure!(
        p.first_hour > 0 && p.first_hour % HOUR == 0,
        "bad first_hour"
    );
    ensure!(
        (1..=KEEP_HOURS as usize).contains(&p.hours),
        "hours out of range"
    );
    // Hours are in the past: a far future one would stretch every series to it.
    let end = i64::try_from(p.hours)
        .ok()
        .and_then(|n| n.checked_mul(HOUR))
        .and_then(|n| n.checked_add(p.first_hour))
        .context("hours out of range")?;
    ensure!(
        end <= hour_of(now.min(p.observed_at)) + HOUR,
        "hours beyond the observation"
    );
    ensure!(p.series.len() <= MAX_SERIES, "too many series");
    for (name, values) in &p.series {
        ensure!(name_ok(name), "bad series name");
        ensure!(values.len() == p.hours, "series length isn't hours");
        ensure!(
            values
                .iter()
                .flatten()
                .all(|v| v.is_finite() && *v >= 0.0 && *v <= 1e12),
            "bad value"
        );
    }
    ensure!(p.limits.len() <= 32, "too many limits");
    for (name, text) in &p.limits {
        ensure!(name_ok(name), "bad limit name");
        ensure!(
            text.chars().count() <= MAX_LIMIT_CHARS && !text.chars().any(char::is_control),
            "bad limit text"
        );
    }
    Ok(p)
}

/// One series: consecutive hours from `first`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Hours {
    first: i64,
    values: Vec<Option<f64>>,
}

impl Hours {
    fn get(&self, hour: i64) -> Option<f64> {
        let at = (hour - self.first).div_euclid(HOUR);
        usize::try_from(at)
            .ok()
            .and_then(|i| self.values.get(i).copied().flatten())
    }

    fn put(&mut self, hour: i64, value: f64) {
        if self.values.is_empty() {
            self.first = hour;
        }
        if hour < self.first {
            let more = ((self.first - hour) / HOUR) as usize;
            self.values.splice(0..0, std::iter::repeat_n(None, more));
            self.first = hour;
        }
        let at = ((hour - self.first) / HOUR) as usize;
        if at >= self.values.len() {
            self.values.resize(at + 1, None);
        }
        self.values[at] = Some(value);
    }

    fn trim(&mut self, oldest: i64) {
        if self.first < oldest {
            let drop = (((oldest - self.first) / HOUR) as usize).min(self.values.len());
            self.values.drain(..drop);
            self.first += drop as i64 * HOUR;
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SourceState {
    received: Option<DateTime<Utc>>,
    observed_at: Option<DateTime<Utc>>,
    limits: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    schema: u32,
    sources: BTreeMap<String, SourceState>,
    series: BTreeMap<String, Hours>,
}

/// The history, and the file that keeps it.
#[derive(Debug, Default)]
pub struct Throughput {
    path: Option<PathBuf>,
    stored: Stored,
    pub error: Option<String>,
}

impl Throughput {
    /// Opens `<state dir>/throughput.json`. A missing file is an empty
    /// history; so is a broken one, which is set aside and said so: the
    /// dashboard still starts.
    pub fn open(state_dir: &Path) -> Self {
        let path = state_dir.join("throughput.json");
        let mut t = Throughput {
            path: Some(path.clone()),
            ..Throughput::default()
        };
        match read_file(&path) {
            Ok(Some(stored)) => t.stored = stored,
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "can't read the throughput history"
                );
                t.error = Some(format!(
                    "throughput history unreadable; started empty: {e:#}"
                ));
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            }
        }
        t
    }

    /// Merges a checked packet: its hours replace what was there, its unknown
    /// hours leave it alone. Then the history is cut to [`KEEP_HOURS`] and saved.
    pub fn apply(&mut self, p: Packet, now: DateTime<Utc>) -> anyhow::Result<()> {
        let oldest = hour_of(now) - KEEP_HOURS * HOUR;
        for (name, values) in &p.series {
            let series = self.stored.series.entry(name.clone()).or_default();
            for (i, v) in values.iter().enumerate() {
                let hour = p.first_hour + i as i64 * HOUR;
                if let (Some(v), true) = (v, hour >= oldest) {
                    series.put(hour, *v);
                }
            }
        }
        let state = self.stored.sources.entry(p.source).or_default();
        state.received = Some(now);
        state.observed_at = Some(p.observed_at);
        state.limits = p.limits;
        for series in self.stored.series.values_mut() {
            series.trim(oldest);
        }
        self.stored.series.retain(|_, s| !s.values.is_empty());
        self.save()
    }

    fn save(&mut self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        self.stored.schema = 1;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self.stored)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    fn get(&self, series: &str, hour: i64) -> Option<f64> {
        self.stored.series.get(series)?.get(hour)
    }
}

fn read_file(path: &Path) -> anyhow::Result<Option<Stored>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_FILE, "over {MAX_FILE} bytes");
    let stored: Stored = serde_json::from_slice(&bytes)?;
    ensure!(stored.schema == 1, "unsupported schema");
    Ok(Some(stored))
}

fn hour_of(t: DateTime<Utc>) -> i64 {
    t.timestamp().div_euclid(HOUR) * HOUR
}

/// What a row counts.
#[derive(Debug, Clone, Copy)]
struct Row {
    id: &'static str,
    label: &'static str,
    unit: &'static str,
    /// Summed. Empty for a step with no page count.
    counts: &'static [&'static str],
    /// Seconds in the same hours as `counts`; `per` divides them.
    seconds: &'static [&'static str],
    /// Which count `seconds` is divided by, if not `counts`.
    per: &'static [&'static str],
    /// Where the numbers come from, and what they aren't.
    scope: &'static str,
    /// The limit when no collector reports one.
    limit: &'static str,
    /// What says the step ran; `counts` if empty.
    ok: &'static [&'static str],
    /// Failures. Empty for a step whose source doesn't count them.
    failed: &'static [&'static str],
    /// What `failed` is a share of. Empty when nothing counts the attempts.
    attempts: &'static [&'static str],
    /// What a failure here is one of.
    failed_unit: &'static str,
    /// Controller phases whose failures belong to this step; `*` for all.
    phases: &'static [&'static str],
    /// Series of failure reasons, by this prefix.
    reasons: &'static str,
    /// Times this step held back because it was at its limit.
    deferred: &'static [&'static str],
    /// Seconds in the hour spent on it, one at a time.
    busy: &'static str,
    /// Where its numbers come from: [`SOURCES`], or [`INDEX`].
    sources: &'static [&'static str],
}

const ROW: Row = Row {
    id: "",
    label: "",
    unit: "pages",
    counts: &[],
    seconds: &[],
    per: &[],
    scope: "",
    limit: "",
    ok: &[],
    failed: &[],
    attempts: &[],
    failed_unit: "",
    phases: &[],
    reasons: "",
    deferred: &[],
    busy: "",
    sources: &[],
};

const ROWS: &[Row] = &[
    Row {
        id: "scan_fetch",
        label: "Scan fetch",
        counts: &["fetch_pages"],
        seconds: &["fetch_seconds"],
        scope: "Lines of the fetch index, every run. Seconds are download time per page.",
        limit: "the global LoC pacer; its rate is set on the fetch host",
        sources: &[INDEX],
        ..ROW
    },
    Row {
        id: "quality_ship",
        label: "Quality ship",
        counts: &["quality_ship_pages", "qwen_ship_pages"],
        seconds: &["quality_ship_seconds", "qwen_ship_seconds"],
        scope: "Pages received on the BOINC server, quality and Qwen chunks, from the shipper's \
                logs. A chunk retried counts once. Seconds are its copy and checks.",
        deferred: &["quality_deferred", "qwen_deferred"],
        sources: &["ship"],
        ..ROW
    },
    Row {
        id: "stage",
        label: "Stage",
        counts: &["quality_stage_pages", "qwen_stage_pages"],
        seconds: &["step_seconds.quality-stage"],
        scope: "Chunks staged, from the shipper's logs. Seconds are the quality-stage steps, \
                retries included.",
        limit: "one bridge step at a time; shipped chunks start in the hour's first 15 minutes",
        sources: &["ship", "boinc"],
        ..ROW
    },
    Row {
        id: "submit",
        label: "Submit",
        counts: &["quality_submit_pages", "qwen_submit_pages"],
        seconds: &["step_seconds.quality-submit"],
        scope: "Chunks that left the outbox, from the shipper's logs. Seconds are the \
                quality-submit steps.",
        limit: "one bridge step at a time",
        sources: &["ship", "boinc"],
        ..ROW
    },
    Row {
        id: "ocr_claim",
        label: "OCR claim to submit",
        counts: &["ocr_claim_pages"],
        seconds: &[
            "phase_seconds.claim",
            "phase_seconds.export",
            "phase_seconds.push",
            "phase_seconds.submit",
        ],
        scope: "Growth of the controller's selected pages at each passing tick. Seconds are its \
                claim, export, push and submit phases.",
        failed: &[
            "phase_failed.claim",
            "phase_failed.export",
            "phase_failed.push",
            "phase_failed.submit",
        ],
        attempts: &[
            "phase_runs.claim",
            "phase_runs.export",
            "phase_runs.push",
            "phase_runs.submit",
        ],
        failed_unit: "phase runs",
        phases: &["claim", "export", "push", "submit"],
        sources: &["controller"],
        ..ROW
    },
    Row {
        id: "usnm_ocr",
        label: "usnm_ocr",
        counts: &["ocr_units"],
        seconds: &["ocr_elapsed_seconds"],
        scope: "Workunits with a canonical result, by when that result came back. One workunit, \
                one page, and an hour with bundles has no count. Seconds are device time on those \
                workunits, every replica back, failed ones too.",
        failed: &["ocr_error"],
        attempts: &[
            "ocr_valid",
            "ocr_inconclusive",
            "ocr_invalid",
            "ocr_waiting",
            "ocr_error",
        ],
        failed_unit: "replicas",
        sources: &["boinc", "controller"],
        ..ROW
    },
    Row {
        id: "usnm_quality",
        label: "usnm_quality",
        unit: "units",
        counts: &["quality_units"],
        seconds: &["quality_elapsed_seconds"],
        scope: "Workunits with a canonical result. A unit is one page or a bundle of pages, and \
                the database doesn't say which, so pages per hour stay unknown here: quality \
                load counts them. Seconds are device time per unit.",
        failed: &["quality_error"],
        attempts: &[
            "quality_valid",
            "quality_inconclusive",
            "quality_invalid",
            "quality_waiting",
            "quality_error",
        ],
        failed_unit: "replicas",
        deferred: &["quality_deferred"],
        sources: &["boinc", "ship"],
        ..ROW
    },
    Row {
        id: "usnm_qwen",
        label: "usnm_qwen",
        unit: "units",
        counts: &["qwen_units"],
        seconds: &["qwen_elapsed_seconds"],
        scope: "Workunits two replicas agreed on, not replicas. A unit can hold several pages and \
                the database doesn't say how many: Qwen load counts them. Seconds are device time \
                per unit, every replica back, inconclusive and failed ones too.",
        failed: &["qwen_error"],
        attempts: &[
            "qwen_valid",
            "qwen_inconclusive",
            "qwen_invalid",
            "qwen_waiting",
            "qwen_error",
        ],
        failed_unit: "replicas",
        deferred: &["qwen_deferred"],
        sources: &["boinc", "ship"],
        ..ROW
    },
    Row {
        id: "ocr_collect",
        label: "OCR results collected",
        counts: &["ocr_collect_pages"],
        seconds: &["phase_seconds.collect"],
        scope: "The controller's newly published pages at each passing tick.",
        failed: &["phase_failed.collect"],
        attempts: &["phase_runs.collect"],
        failed_unit: "phase runs",
        phases: &["collect"],
        sources: &["controller"],
        ..ROW
    },
    Row {
        id: "quality_load",
        label: "Quality results loaded",
        counts: &["quality_load_pages"],
        seconds: &["quality_load_seconds"],
        scope: "Pages of each unit loaded into the quality database, from the loader's logs. \
                Seconds are whole loader runs, including runs that loaded nothing.",
        limit: "one unit a run",
        failed: &["quality_load_failed"],
        attempts: &["quality_load_runs"],
        failed_unit: "loader runs",
        sources: &["ship"],
        ..ROW
    },
    Row {
        id: "qwen_load",
        label: "Qwen results loaded",
        counts: &["qwen_load_pages"],
        seconds: &["qwen_load_seconds"],
        scope: "Growth of the loader's pages with readings at each successful load. Seconds are \
                whole loader runs, including waits for the lock.",
        failed: &["qwen_load_failed"],
        attempts: &["qwen_load_runs"],
        failed_unit: "loader runs",
        sources: &["ship"],
        ..ROW
    },
    Row {
        id: "controller",
        label: "Controller ticks",
        unit: "ticks",
        counts: &["ticks_ok", "ticks_failed"],
        seconds: &["tick_seconds"],
        per: &["ticks_ok", "ticks_failed"],
        scope: "Passing and failing ticks by when they ended. Seconds are per tick.",
        ok: &["ticks_ok"],
        failed: &["ticks_failed"],
        attempts: &["ticks_ok", "ticks_failed"],
        failed_unit: "ticks",
        phases: &["*"],
        reasons: "reason.controller.",
        busy: "tick_busy_seconds",
        sources: &["controller"],
        ..ROW
    },
    Row {
        id: "bridge_lock",
        label: "Bridge lock held",
        unit: "seconds",
        counts: &["lock_seconds"],
        seconds: &[],
        scope: "Seconds step containers ran, each under the host's one lock; a minimum, since \
                loaders and native submission hold it without a container.",
        limit: "3,600 seconds an hour",
        failed: &["phase_failed.lock"],
        attempts: &["phase_runs.lock"],
        failed_unit: "lock attempts",
        phases: &["lock"],
        busy: "lock_seconds",
        sources: &["boinc", "controller"],
        ..ROW
    },
];

/// One step's numbers.
#[derive(Debug, Clone, Serialize)]
pub struct StepRate {
    pub id: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    /// The last full hour; `None` if nobody observed it.
    pub last_hour: Option<f64>,
    /// The last 24 full hours, over the hours observed.
    pub last_day: Option<f64>,
    pub day_hours_known: u32,
    pub seconds_per_page: Option<f64>,
    /// Failures in the last 24 full hours, for steps that have them.
    pub failed_day: Option<f64>,
    pub limit: Option<String>,
    pub scope: &'static str,
    /// Oldest first, [`BIN_HOURS`] each; `None` if no hour in it was observed.
    pub bins: Vec<Option<f64>>,
    /// Hours observed in each bin.
    #[serde(skip)]
    pub bin_known: Vec<u32>,
    pub detail: StepDetail,
}

/// A sum over some hours, and how many of them were observed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Counted {
    pub value: Option<f64>,
    pub hours_known: u32,
}

impl Counted {
    fn of(hours: impl Iterator<Item = i64>, get: impl Fn(i64) -> Option<f64>) -> Self {
        let (value, hours_known) = total(hours, get);
        Counted { value, hours_known }
    }

    fn per_hour(self) -> Option<f64> {
        self.value.map(|v| v / f64::from(self.hours_known))
    }
}

/// Change from one day to the next, in percent, per hour observed so that a
/// day with a gap doesn't read as a drop. `None` if either is unknown or the
/// earlier one was zero.
pub fn trend(now: Counted, before: Counted) -> Option<f64> {
    let (n, b) = (now.per_hour()?, before.per_hour()?);
    (b > 0.0).then(|| (n - b) / b * 100.0)
}

/// Why a step failed, and how often.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reason {
    pub text: String,
    /// `None` where the source keeps only a count of its latest failures.
    pub day: Option<f64>,
    pub week: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Failures {
    pub unit: &'static str,
    pub day: Counted,
    pub week: Counted,
    /// Attempts in the hours where both are known; `None` if nothing counts them.
    pub day_attempts: Option<f64>,
    pub day_failed_of_attempts: Option<f64>,
    pub week_attempts: Option<f64>,
    pub week_failed_of_attempts: Option<f64>,
    pub reasons: Vec<Reason>,
    /// How many failures the reasons cover, where they are a sample.
    pub reasons_from_latest: Option<usize>,
}

/// One of a step's sources, and when it last reported.
#[derive(Debug, Clone, Serialize)]
pub struct StepSource {
    pub name: &'static str,
    pub received: Option<DateTime<Utc>>,
    pub stale: bool,
}

/// What opens under a step's row.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StepDetail {
    pub last_6h: Counted,
    pub last_week: Counted,
    /// The 24 full hours before the last 24.
    pub previous_day: Counted,
    /// Roughly: from a histogram, for the one step with a time per page.
    pub seconds_p50: Option<f64>,
    pub seconds_p95: Option<f64>,
    /// `None` for a step whose source doesn't count failures.
    pub failures: Option<Failures>,
    pub limit_now: Option<String>,
    pub deferred_day: Option<f64>,
    pub deferred_week: Option<f64>,
    /// Share of the last hour, and of the observed last 24, spent on the step.
    pub busy_hour: Option<f64>,
    pub busy_day: Option<f64>,
    /// The start of the newest hour it did something in.
    pub last_ok: Option<DateTime<Utc>>,
    /// The hour of the newest failure, or its time if `last_failed_exact`.
    pub last_failed: Option<DateTime<Utc>>,
    pub last_failed_exact: bool,
    /// Hours with a success count, and with a failure count, that those two
    /// looked through.
    pub hours_searched: u32,
    pub failed_hours_searched: u32,
    /// The newest hour with a value.
    pub newest_hour: Option<DateTime<Utc>>,
    /// Oldest first: [`DAY_HOURS`] of them, the last full ones.
    pub hours: Vec<Option<f64>>,
    pub sources: Vec<StepSource>,
}

/// An app's results by validation outcome, as replicas, not pages (#1420).
#[derive(Debug, Clone, Serialize)]
pub struct QuorumRow {
    pub app: &'static str,
    pub valid_day: Option<f64>,
    pub inconclusive_day: Option<f64>,
    pub invalid_day: Option<f64>,
    pub error_day: Option<f64>,
    pub waiting_day: Option<f64>,
    pub valid_week: Option<f64>,
    pub inconclusive_week: Option<f64>,
    /// Pages with a canonical result in the last 24 full hours.
    pub pages_day: Option<f64>,
    pub valid_bins: Vec<Option<f64>>,
    pub inconclusive_bins: Vec<Option<f64>>,
}

/// A controller phase over the last 24 full hours.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseRow {
    pub phase: String,
    pub runs: f64,
    pub failed: f64,
    pub seconds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRow {
    pub name: String,
    pub received: Option<DateTime<Utc>>,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ThroughputView {
    pub steps: Vec<StepRate>,
    pub quorum: Vec<QuorumRow>,
    pub phases: Vec<PhaseRow>,
    pub sources: Vec<SourceRow>,
    /// Replicas back in the last full hour, across the three apps, and of
    /// those the valid ones.
    pub results_last_hour: Option<f64>,
    pub valid_last_hour: Option<f64>,
    /// When the first bin starts.
    pub chart_start: Option<DateTime<Utc>>,
    /// When the first bar of the hourly zoom starts.
    pub hours_start: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// What the scan fetch's own files say, for its row.
#[derive(Debug, Clone, Default)]
pub struct FetchFacts {
    pub read_at: Option<DateTime<Utc>>,
    /// Failures in the hours the index covers of the last 24, if the failure
    /// log was read and those hours are known.
    pub failed_day: Option<usize>,
    pub last_failed: Option<DateTime<Utc>>,
    /// Reasons among the latest failures, by count, and how many that was.
    pub reasons: Vec<(String, usize)>,
    pub reasons_from: usize,
    pub seconds: Option<(f64, f64)>,
}

impl ThroughputView {
    pub fn step(&self, id: &str) -> Option<&StepRate> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// The source reported lately, so its last hour can stand for now.
    pub fn fresh(&self, source: &str) -> bool {
        self.sources.iter().any(|s| s.name == source && !s.stale)
    }

    /// Fills in the scan fetch's row from what only the fetch knows.
    pub fn add_fetch(&mut self, facts: FetchFacts, now: DateTime<Utc>) {
        let Some(s) = self.steps.iter_mut().find(|s| s.id == "scan_fetch") else {
            return;
        };
        let d = &mut s.detail;
        for source in &mut d.sources {
            source.received = facts.read_at;
            source.stale = facts.read_at.is_none_or(|at| now - at > SOURCE_FRESH);
        }
        (d.seconds_p50, d.seconds_p95) = facts.seconds.unzip();
        d.last_failed = facts.last_failed;
        d.last_failed_exact = facts.last_failed.is_some() || facts.failed_day.is_some();
        let Some(failed) = facts.failed_day else {
            return;
        };
        let failed = failed as f64;
        d.failures = Some(Failures {
            unit: "pages",
            day: Counted {
                value: Some(failed),
                hours_known: s.day_hours_known,
            },
            day_attempts: s.last_day.map(|pages| pages + failed),
            day_failed_of_attempts: s.last_day.map(|_| failed),
            reasons: facts
                .reasons
                .into_iter()
                .map(|(text, n)| Reason {
                    text,
                    day: Some(n as f64),
                    week: None,
                })
                .collect(),
            reasons_from_latest: Some(facts.reasons_from),
            ..Failures::default()
        });
    }
}

/// `get` over `hours`: the sum of what is known, and how many hours were.
fn total(hours: impl Iterator<Item = i64>, get: impl Fn(i64) -> Option<f64>) -> (Option<f64>, u32) {
    let (mut sum, mut known) = (0.0, 0);
    for h in hours {
        if let Some(v) = get(h) {
            sum += v;
            known += 1;
        }
    }
    (if known > 0 { Some(sum) } else { None }, known)
}

/// `series` summed at an hour: unknown if any is, since the row or total
/// stands for all of them.
fn sum_all(get: &impl Fn(&str, i64) -> Option<f64>, series: &[&str], hour: i64) -> Option<f64> {
    series
        .iter()
        .map(|s| get(s, hour))
        .sum::<Option<f64>>()
        .filter(|_| !series.is_empty())
}

/// `series` of seconds summed at an hour, unknown only if every one is: a
/// phase that hasn't run yet has no series.
fn sum_at(get: &impl Fn(&str, i64) -> Option<f64>, series: &[&str], hour: i64) -> Option<f64> {
    series
        .iter()
        .filter_map(|s| get(s, hour))
        .fold(None, |a, v| Some(a.unwrap_or(0.0) + v))
}

fn range(end: i64, hours: i64) -> impl DoubleEndedIterator<Item = i64> {
    (0..hours).rev().map(move |i| end - i * HOUR)
}

fn bins(get: impl Fn(i64) -> Option<f64>, current: i64) -> (Vec<Option<f64>>, Vec<u32>) {
    let n = CHART_HOURS / BIN_HOURS;
    let last = current.div_euclid(BIN_HOURS * HOUR) * BIN_HOURS * HOUR;
    let mut out = Vec::with_capacity(n as usize);
    let mut known = Vec::with_capacity(n as usize);
    for i in (0..n).rev() {
        let start = last - i * BIN_HOURS * HOUR;
        let (sum, k) = total((0..BIN_HOURS).map(|j| start + j * HOUR), &get);
        out.push(sum);
        known.push(k);
    }
    (out, known)
}

/// `failed` over `attempts`, in the hours both are known: the attempts, then
/// the failures among them.
fn share_of(
    hours: impl Iterator<Item = i64>,
    failed: impl Fn(i64) -> Option<f64>,
    attempts: impl Fn(i64) -> Option<f64>,
) -> (Option<f64>, Option<f64>) {
    let (mut tried, mut lost, mut known) = (0.0, 0.0, false);
    for h in hours {
        if let (Some(f), Some(a)) = (failed(h), attempts(h)) {
            tried += a;
            lost += f;
            known = true;
        }
    }
    if known {
        (Some(tried), Some(lost))
    } else {
        (None, None)
    }
}

/// The start of the newest of `hours`, newest first, with something in it.
fn newest(
    mut hours: impl Iterator<Item = i64>,
    get: impl Fn(i64) -> Option<f64>,
) -> Option<DateTime<Utc>> {
    hours
        .find(|h| get(*h).is_some_and(|v| v > 0.0))
        .and_then(|h| DateTime::from_timestamp(h, 0))
}

/// A share of the seconds in `hours` observed, in percent.
fn busy_share(c: Counted) -> Option<f64> {
    c.value
        .map(|v| v / (f64::from(c.hours_known) * HOUR as f64) * 100.0)
}

/// Failure reasons recorded for a step: its controller phases, then what
/// `prefix` series say. Most in the last day first.
fn reasons_of(
    t: &Throughput,
    r: &Row,
    get: &impl Fn(&str, i64) -> Option<f64>,
    end: i64,
) -> Vec<Reason> {
    let mut out = Vec::new();
    let mut add = |series: &str, text: String| {
        let day = total(range(end, DAY_HOURS), |h| get(series, h)).0;
        let week = total(range(end, CHART_HOURS), |h| get(series, h)).0;
        if week.is_some_and(|n| n > 0.0) {
            out.push(Reason { text, day, week });
        }
    };
    for name in t.stored.series.keys() {
        if let Some(phase) = name.strip_prefix("phase_failed.") {
            if r.phases.contains(&phase) || r.phases == ["*"] {
                add(name, format!("{phase} phase failed"));
            }
        }
        if !r.reasons.is_empty() {
            if let Some(slug) = name.strip_prefix(r.reasons) {
                add(name, slug.replace('-', " "));
            }
        }
    }
    out.sort_by(|a, b| {
        let key = |r: &Reason| (r.day.unwrap_or(0.0), r.week.unwrap_or(0.0));
        let (x, y) = (key(a), key(b));
        y.0.total_cmp(&x.0).then(y.1.total_cmp(&x.1))
    });
    out.truncate(MAX_REASONS);
    out
}

/// The panel's numbers. `fetch` is the scan fetch's hours, from the index.
pub fn view(
    t: &Throughput,
    fetch: &BTreeMap<i64, (u64, u64)>,
    fetch_covered: Option<(i64, i64)>,
    now: DateTime<Utc>,
) -> ThroughputView {
    let current = hour_of(now);
    let end = current - HOUR;
    let get = |series: &str, hour: i64| -> Option<f64> {
        match series {
            "fetch_pages" | "fetch_seconds" => {
                let (from, to) = fetch_covered?;
                if !(from..=to).contains(&hour) {
                    return None;
                }
                let (pages, ms) = fetch.get(&hour).copied().unwrap_or((0, 0));
                Some(if series == "fetch_pages" {
                    pages as f64
                } else {
                    ms as f64 / 1000.0
                })
            }
            // A bundled OCR unit holds an unknown number of pages, so an hour
            // with one has no page count.
            "ocr_units" | "ocr_elapsed_seconds"
                if t.get("ocr_bundles", hour).is_some_and(|n| n > 0.0) =>
            {
                None
            }
            _ => t.get(series, hour),
        }
    };
    let limits: BTreeMap<&str, &String> = t
        .stored
        .sources
        .values()
        .flat_map(|s| s.limits.iter().map(|(k, v)| (k.as_str(), v)))
        .collect();
    let mut steps = Vec::with_capacity(ROWS.len());
    for r in ROWS {
        let count_at = |h: i64| sum_all(&get, r.counts, h);
        let (last_day, day_hours_known) = total(range(end, 24), count_at);
        let (bin_values, bin_known) = bins(count_at, current);
        // Seconds over the same hours, only where both are known.
        let per: &[&str] = if r.per.is_empty() { r.counts } else { r.per };
        let (mut secs, mut pages) = (0.0, 0.0);
        for h in range(end, 24) {
            if let (Some(n), Some(s)) = (sum_all(&get, per, h), sum_at(&get, r.seconds, h)) {
                secs += s;
                pages += n;
            }
        }
        let failed_day = match r.id {
            "controller" => total(range(end, 24), |h| get("ticks_failed", h)).0,
            "usnm_ocr" => total(range(end, 24), |h| get("ocr_error", h)).0,
            "usnm_quality" => total(range(end, 24), |h| get("quality_error", h)).0,
            "usnm_qwen" => total(range(end, 24), |h| get("qwen_error", h)).0,
            "quality_load" => total(range(end, 24), |h| get("quality_load_failed", h)).0,
            "qwen_load" => total(range(end, 24), |h| get("qwen_load_failed", h)).0,
            _ => None,
        };
        let detail = {
            let searched = || range(end, KEEP_HOURS).rev();
            let oks = if r.ok.is_empty() { r.counts } else { r.ok };
            let failed_at = |h: i64| sum_at(&get, r.failed, h);
            let attempts_at = |h: i64| sum_at(&get, r.attempts, h);
            let deferred_at = |h: i64| sum_at(&get, r.deferred, h);
            let day = Counted::of(range(end, DAY_HOURS), failed_at);
            let week = Counted::of(range(end, CHART_HOURS), failed_at);
            let (day_attempts, day_failed_of_attempts) =
                share_of(range(end, DAY_HOURS), failed_at, attempts_at);
            let (week_attempts, week_failed_of_attempts) =
                share_of(range(end, CHART_HOURS), failed_at, attempts_at);
            let busy_at = |h: i64| get(r.busy, h);
            StepDetail {
                last_6h: Counted::of(range(end, 6), count_at),
                last_week: Counted::of(range(end, CHART_HOURS), count_at),
                previous_day: Counted::of(range(end - DAY_HOURS * HOUR, DAY_HOURS), count_at),
                failures: (!r.failed.is_empty()).then(|| Failures {
                    unit: r.failed_unit,
                    day,
                    week,
                    day_attempts,
                    day_failed_of_attempts,
                    week_attempts,
                    week_failed_of_attempts,
                    reasons: reasons_of(t, r, &get, end),
                    reasons_from_latest: None,
                }),
                limit_now: limits
                    .get(format!("{}.now", r.id).as_str())
                    .map(|s| (*s).clone()),
                deferred_day: total(range(end, DAY_HOURS), deferred_at).0,
                deferred_week: total(range(end, CHART_HOURS), deferred_at).0,
                busy_hour: busy_share(Counted::of(range(end, 1), busy_at)),
                busy_day: busy_share(Counted::of(range(end, DAY_HOURS), busy_at)),
                last_ok: newest(searched(), |h| sum_at(&get, oks, h)),
                last_failed: newest(searched(), failed_at),
                hours_searched: total(searched(), |h| sum_at(&get, oks, h)).1,
                failed_hours_searched: total(searched(), failed_at).1,
                newest_hour: searched()
                    .find(|h| count_at(*h).is_some())
                    .and_then(|h| DateTime::from_timestamp(h, 0)),
                hours: range(end, DAY_HOURS).map(count_at).collect(),
                sources: r
                    .sources
                    .iter()
                    .map(|&name| {
                        let received = t.stored.sources.get(name).and_then(|s| s.received);
                        StepSource {
                            name,
                            received,
                            stale: received.is_none_or(|at| now - at > SOURCE_FRESH),
                        }
                    })
                    .collect(),
                ..StepDetail::default()
            }
        };
        steps.push(StepRate {
            id: r.id,
            label: r.label,
            unit: r.unit,
            last_hour: count_at(end),
            last_day,
            day_hours_known,
            seconds_per_page: (pages > 0.0).then(|| secs / pages),
            failed_day,
            limit: limits
                .get(r.id)
                .map(|s| (*s).clone())
                .or_else(|| (!r.limit.is_empty()).then(|| r.limit.to_owned())),
            scope: r.scope,
            bins: bin_values,
            bin_known,
            detail,
        });
    }
    let quorum = [
        ("usnm_ocr", "ocr"),
        ("usnm_quality", "quality"),
        ("usnm_qwen", "qwen"),
    ]
    .iter()
    .map(|(app, key)| {
        let day = |name: &str| {
            let series = format!("{key}_{name}");
            total(range(end, 24), |h| get(&series, h)).0
        };
        let week = |name: &str| {
            let series = format!("{key}_{name}");
            total(range(end, 168), |h| get(&series, h)).0
        };
        let valid = format!("{key}_valid");
        let inconclusive = format!("{key}_inconclusive");
        QuorumRow {
            app,
            valid_day: day("valid"),
            inconclusive_day: day("inconclusive"),
            invalid_day: day("invalid"),
            error_day: day("error"),
            waiting_day: day("waiting"),
            valid_week: week("valid"),
            inconclusive_week: week("inconclusive"),
            pages_day: day("units"),
            valid_bins: bins(|h| get(&valid, h), current).0,
            inconclusive_bins: bins(|h| get(&inconclusive, h), current).0,
        }
    })
    .collect();
    let mut phases = Vec::new();
    for name in t.stored.series.keys() {
        let Some(phase) = name.strip_prefix("phase_runs.") else {
            continue;
        };
        let sum = |prefix: &str| {
            let series = format!("{prefix}{phase}");
            total(range(end, 24), |h| get(&series, h)).0
        };
        if let Some(runs) = sum("phase_runs.").filter(|n| *n > 0.0) {
            phases.push(PhaseRow {
                phase: phase.to_owned(),
                runs,
                failed: sum("phase_failed.").unwrap_or(0.0),
                seconds: sum("phase_seconds.").unwrap_or(0.0),
            });
        }
    }
    phases.sort_by(|a, b| b.seconds.total_cmp(&a.seconds));
    let sources = SOURCES
        .iter()
        .map(|name| {
            let s = t.stored.sources.get(*name);
            let received = s.and_then(|s| s.received);
            SourceRow {
                name: (*name).to_owned(),
                received,
                observed_at: s.and_then(|s| s.observed_at),
                stale: received.is_none_or(|at| now - at > SOURCE_FRESH),
            }
        })
        .collect();
    let first =
        current.div_euclid(BIN_HOURS * HOUR) * BIN_HOURS * HOUR - (CHART_HOURS - BIN_HOURS) * HOUR;
    let results: Vec<String> = ["ocr", "quality", "qwen"]
        .iter()
        .flat_map(|app| {
            ["valid", "inconclusive", "invalid", "waiting", "error"]
                .iter()
                .map(move |outcome| format!("{app}_{outcome}"))
        })
        .collect();
    let results: Vec<&str> = results.iter().map(String::as_str).collect();
    ThroughputView {
        results_last_hour: sum_all(&get, &results, end),
        valid_last_hour: sum_all(&get, &["ocr_valid", "quality_valid", "qwen_valid"], end),
        steps,
        quorum,
        phases,
        sources,
        chart_start: DateTime::from_timestamp(first, 0),
        hours_start: DateTime::from_timestamp(end - (DAY_HOURS - 1) * HOUR, 0),
        error: t.error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 30, 0).unwrap()
    }

    fn packet(source: &str, first: i64, series: serde_json::Value) -> Vec<u8> {
        let hours = series
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_array()
            .unwrap()
            .len();
        serde_json::to_vec(&json!({
            "schema": 1, "source": source, "observed_at": now().to_rfc3339(),
            "first_hour": first, "hours": hours, "series": series,
            "limits": {"usnm_ocr": "300 runnable workunits"}
        }))
        .unwrap()
    }

    fn apply(t: &mut Throughput, source: &str, first: i64, series: serde_json::Value) {
        let p = parse(&packet(source, first, series), source, now()).unwrap();
        t.apply(p, now()).unwrap();
    }

    fn step<'a>(v: &'a ThroughputView, id: &str) -> &'a StepRate {
        v.step(id).unwrap()
    }

    #[test]
    fn a_bad_push_is_refused() {
        let good = packet("ship", 3600, json!({"a_b.c-d": [1.5, null]}));
        assert!(parse(&good, "ship", now()).is_ok());
        assert!(parse(&good, "boinc", now()).is_err());
        assert!(parse(b"not json", "ship", now()).is_err());
        for bad in [
            packet("ship", 3601, json!({"a": [1]})),
            packet("ship", -3600, json!({"a": [1]})),
            packet("ship", 3600, json!({"A B": [1]})),
            packet("ship", 3600, json!({"a": [-1]})),
            packet("ship", 3600, json!({"a": [1e13]})),
        ] {
            assert!(parse(&bad, "ship", now()).is_err());
        }
        // One sample a year on would stretch every series out to it.
        let far = hour_of(now()) + 365 * 24 * HOUR;
        assert!(parse(&packet("ship", far, json!({"a": [1]})), "ship", now()).is_err());
        let current = hour_of(now());
        assert!(parse(&packet("ship", current, json!({"a": [1]})), "ship", now()).is_ok());
        assert!(parse(
            &packet("ship", current + HOUR, json!({"a": [1]})),
            "ship",
            now()
        )
        .is_err());
        let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
        v["first_hour"] = json!(i64::MAX / HOUR * HOUR);
        assert!(parse(&serde_json::to_vec(&v).unwrap(), "ship", now()).is_err());
        v["first_hour"] = json!(3600);
        v["hours"] = json!(3);
        assert!(parse(&serde_json::to_vec(&v).unwrap(), "ship", now()).is_err());
        v["hours"] = json!(2);
        v["page_text"] = json!("not permitted");
        assert!(parse(&serde_json::to_vec(&v).unwrap(), "ship", now()).is_err());
        v.as_object_mut().unwrap().remove("page_text");
        v["limits"]["x"] = json!("line\nbreak");
        assert!(parse(&serde_json::to_vec(&v).unwrap(), "ship", now()).is_err());
        v["limits"].as_object_mut().unwrap().remove("x");
        v["observed_at"] = json!((now() + Duration::minutes(5)).to_rfc3339());
        assert!(parse(&serde_json::to_vec(&v).unwrap(), "ship", now()).is_err());
    }

    #[test]
    fn unknown_is_never_zero_and_zero_is_shown_as_zero() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        let empty = view(&t, &BTreeMap::new(), None, now());
        assert!(empty.steps.iter().all(|s| s.last_hour.is_none()));
        assert!(empty.steps.iter().all(|s| s.last_day.is_none()));
        assert!(empty.steps.iter().all(|s| s.seconds_per_page.is_none()));
        assert!(empty.sources.iter().all(|s| s.stale));
        // Read and quiet: a real zero. The hour before, nobody looked.
        apply(
            &mut t,
            "ship",
            end - 2 * HOUR,
            json!({"quality_load_pages": [5, null, 0]}),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        let q = step(&v, "quality_load");
        assert_eq!(q.last_hour, Some(0.0));
        assert_eq!(q.last_day, Some(5.0));
        assert_eq!(q.day_hours_known, 2);
        assert!(q.seconds_per_page.is_none());
        assert!(step(&v, "bridge_lock").last_hour.is_none());
    }

    #[test]
    fn an_old_hour_is_replaced_and_an_unknown_one_keeps_what_it_had() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(&mut t, "boinc", end - HOUR, json!({"qwen_units": [2, 3]}));
        apply(
            &mut t,
            "boinc",
            end - HOUR,
            json!({"qwen_units": [null, 7]}),
        );
        assert_eq!(t.get("qwen_units", end - HOUR), Some(2.0));
        assert_eq!(t.get("qwen_units", end), Some(7.0));
        // A push of older hours extends the series backwards.
        apply(&mut t, "boinc", end - 5 * HOUR, json!({"qwen_units": [1]}));
        assert_eq!(t.get("qwen_units", end - 5 * HOUR), Some(1.0));
        assert_eq!(t.get("qwen_units", end - 3 * HOUR), None);
        assert_eq!(t.get("qwen_units", end), Some(7.0));
    }

    #[test]
    fn history_survives_a_restart_and_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Throughput::open(dir.path());
        let end = hour_of(now()) - HOUR;
        let long: Vec<f64> = (0..KEEP_HOURS).map(|i| i as f64).collect();
        apply(
            &mut t,
            "boinc",
            end - (KEEP_HOURS - 1) * HOUR,
            json!({"lock_seconds": long}),
        );
        let again = Throughput::open(dir.path());
        assert_eq!(
            again.get("lock_seconds", end),
            Some((KEEP_HOURS - 1) as f64)
        );
        assert_eq!(
            again.stored.sources["boinc"].limits["usnm_ocr"],
            "300 runnable workunits"
        );
        // Two weeks of a hundred series stay well under the file's limit.
        assert!(
            std::fs::metadata(dir.path().join("throughput.json"))
                .unwrap()
                .len()
                < 16 * 1024
        );
        // Time passes: the oldest hours go.
        let later = now() + Duration::days(7);
        let mut body: serde_json::Value = serde_json::from_slice(&packet(
            "boinc",
            hour_of(later),
            json!({"lock_seconds": [9]}),
        ))
        .unwrap();
        body["observed_at"] = json!(later.to_rfc3339());
        let p = parse(&serde_json::to_vec(&body).unwrap(), "boinc", later).unwrap();
        let mut t = again;
        t.apply(p, later).unwrap();
        assert_eq!(t.get("lock_seconds", end - (KEEP_HOURS - 1) * HOUR), None);
        assert!(t.stored.series["lock_seconds"].values.len() <= KEEP_HOURS as usize + 1);
    }

    #[test]
    fn a_broken_history_file_is_set_aside_and_said_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("throughput.json"), b"{broken").unwrap();
        let t = Throughput::open(dir.path());
        assert!(t.error.as_deref().unwrap().contains("started empty"));
        assert!(dir.path().join("throughput.json.bad").exists());
        assert!(!dir.path().join("throughput.json").exists());
    }

    #[test]
    fn seconds_per_page_use_only_hours_with_both_numbers() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(
            &mut t,
            "ship",
            end - HOUR,
            json!({
                "quality_stage_pages": [100, 200],
                "qwen_stage_pages": [0, 24],
            }),
        );
        apply(
            &mut t,
            "boinc",
            end,
            json!({"step_seconds.quality-stage": [448]}),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        let s = step(&v, "stage");
        assert_eq!(s.last_hour, Some(224.0));
        assert_eq!(s.last_day, Some(324.0));
        assert_eq!(s.seconds_per_page, Some(2.0));
    }

    #[test]
    fn a_row_of_two_lanes_is_unknown_while_one_of_them_is() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(&mut t, "ship", end, json!({"quality_ship_pages": [100]}));
        let v = view(&t, &BTreeMap::new(), None, now());
        assert!(step(&v, "quality_ship").last_hour.is_none());
        apply(
            &mut t,
            "ship",
            end,
            json!({"quality_ship_pages": [100], "qwen_ship_pages": [0]}),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        assert_eq!(step(&v, "quality_ship").last_hour, Some(100.0));
    }

    #[test]
    fn replicas_are_not_pages() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(
            &mut t,
            "boinc",
            end,
            json!({
                "qwen_valid": [156], "qwen_inconclusive": [409], "qwen_units": [78],
                "qwen_elapsed_seconds": [7800], "qwen_invalid": [1], "qwen_error": [2],
                "qwen_waiting": [3]
            }),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        assert_eq!(step(&v, "usnm_qwen").last_hour, Some(78.0));
        assert_eq!(step(&v, "usnm_qwen").unit, "units");
        assert_eq!(step(&v, "usnm_qwen").seconds_per_page, Some(100.0));
        assert_eq!(step(&v, "usnm_qwen").failed_day, Some(2.0));
        let q = v.quorum.iter().find(|q| q.app == "usnm_qwen").unwrap();
        assert_eq!(q.valid_day, Some(156.0));
        assert_eq!(q.inconclusive_day, Some(409.0));
        assert_eq!(q.pages_day, Some(78.0));
        assert_eq!(q.inconclusive_bins.iter().flatten().sum::<f64>(), 409.0);
        assert!(v
            .quorum
            .iter()
            .find(|q| q.app == "usnm_ocr")
            .unwrap()
            .valid_day
            .is_none());
    }

    #[test]
    fn bundled_ocr_units_make_the_page_count_unknown() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(
            &mut t,
            "boinc",
            end,
            json!({"ocr_units": [10], "ocr_bundles": [0]}),
        );
        assert_eq!(
            step(&view(&t, &BTreeMap::new(), None, now()), "usnm_ocr").last_hour,
            Some(10.0)
        );
        apply(
            &mut t,
            "boinc",
            end,
            json!({"ocr_units": [10], "ocr_bundles": [3]}),
        );
        apply(
            &mut t,
            "boinc",
            end - HOUR,
            json!({"ocr_units": [4], "ocr_bundles": [0]}),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        // The bundled hour is unknown; the hour before it still counts.
        assert!(step(&v, "usnm_ocr").last_hour.is_none());
        assert_eq!(step(&v, "usnm_ocr").last_day, Some(4.0));
        assert_eq!(step(&v, "usnm_ocr").bins.iter().flatten().sum::<f64>(), 4.0);
    }

    #[test]
    fn the_fetch_hours_come_from_the_index_only_where_it_was_read() {
        let t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        let fetch = [(end, (120, 60_000)), (end - 3 * HOUR, (30, 1000))].into();
        let v = view(&t, &fetch, Some((end - HOUR, hour_of(now()))), now());
        let f = step(&v, "scan_fetch");
        assert_eq!(f.last_hour, Some(120.0));
        assert_eq!(f.last_day, Some(120.0));
        assert_eq!(f.day_hours_known, 2);
        assert_eq!(f.seconds_per_page, Some(0.5));
        let v = view(&t, &fetch, None, now());
        assert!(step(&v, "scan_fetch").last_hour.is_none());
    }

    #[test]
    fn limits_come_from_the_collector_or_the_row_or_are_not_known() {
        let mut t = Throughput::default();
        let v = view(&t, &BTreeMap::new(), None, now());
        assert!(step(&v, "usnm_ocr").limit.is_none());
        assert_eq!(
            step(&v, "submit").limit.as_deref(),
            Some("one bridge step at a time")
        );
        apply(&mut t, "controller", 3600, json!({"x": [1]}));
        let v = view(&t, &BTreeMap::new(), None, now());
        assert_eq!(
            step(&v, "usnm_ocr").limit.as_deref(),
            Some("300 runnable workunits")
        );
    }

    #[test]
    fn phases_are_listed_longest_first_and_a_quiet_source_goes_stale() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(
            &mut t,
            "controller",
            end,
            json!({
                "phase_runs.submit": [2], "phase_seconds.submit": [80], "phase_failed.submit": [1],
                "phase_runs.lock": [2], "phase_seconds.lock": [4], "phase_failed.lock": [0],
            }),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        assert_eq!(v.phases[0].phase, "submit");
        assert_eq!(v.phases[0].failed, 1.0);
        assert_eq!(v.phases[1].phase, "lock");
        let controller = |v: &ThroughputView| {
            v.sources
                .iter()
                .find(|s| s.name == "controller")
                .unwrap()
                .stale
        };
        assert!(!controller(&v));
        assert!(controller(&view(
            &t,
            &BTreeMap::new(),
            None,
            now() + SOURCE_FRESH + Duration::minutes(1)
        )));
    }

    #[test]
    fn a_step_counts_its_windows_over_the_hours_observed() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        let hours: Vec<Option<f64>> = (0..48)
            .map(|i| (i != 40).then_some(if i < 24 { 1.0 } else { 3.0 }))
            .collect();
        apply(
            &mut t,
            "ship",
            end - 47 * HOUR,
            json!({"quality_load_pages": hours}),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        let s = step(&v, "quality_load");
        let d = &s.detail;
        assert_eq!(
            d.last_6h,
            Counted {
                value: Some(18.0),
                hours_known: 6
            }
        );
        assert_eq!(s.last_day, Some(69.0));
        assert_eq!(s.day_hours_known, 23);
        assert_eq!(d.previous_day.value, Some(24.0));
        assert_eq!(d.last_week.value, Some(93.0));
        assert_eq!(d.last_week.hours_known, 47);
        // 3 an hour against 1, though the last day has a gap.
        let now_day = Counted {
            value: s.last_day,
            hours_known: s.day_hours_known,
        };
        assert_eq!(trend(now_day, d.previous_day), Some(200.0));
        assert_eq!(d.hours.len(), DAY_HOURS as usize);
        assert_eq!(d.hours[23], Some(3.0));
        assert_eq!(d.hours.iter().filter(|h| h.is_none()).count(), 1);
        assert_eq!(d.hours.iter().flatten().sum::<f64>(), 69.0);
        assert_eq!(
            d.newest_hour,
            DateTime::from_timestamp(end, 0),
            "the last hour had a value"
        );
    }

    #[test]
    fn with_no_earlier_day_or_a_zero_one_there_is_no_percentage() {
        let known = |value| Counted {
            value,
            hours_known: 24,
        };
        assert_eq!(trend(known(Some(5.0)), known(None)), None);
        assert_eq!(trend(known(None), known(Some(5.0))), None);
        assert_eq!(trend(known(Some(5.0)), known(Some(0.0))), None);
        assert_eq!(trend(known(Some(5.0)), known(Some(10.0))), Some(-50.0));
    }

    #[test]
    fn failures_have_a_rate_only_where_the_attempts_are_counted() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(
            &mut t,
            "controller",
            end - HOUR,
            json!({
                "ticks_ok": [3, 9], "ticks_failed": [1, 0],
                "phase_failed.inventory": [1, 0], "phase_runs.inventory": [4, 9],
                "phase_failed.lock": [0, 0], "phase_runs.lock": [4, 9],
                "reason.controller.bridge-batch-inventory-failed": [1, 0],
            }),
        );
        apply(
            &mut t,
            "ship",
            end - HOUR,
            json!({"qwen_load_pages": [0, 2], "qwen_load_failed": [4, 1]}),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        let c = step(&v, "controller").detail.failures.as_ref().unwrap();
        assert_eq!(c.day.value, Some(1.0));
        assert_eq!(c.day_attempts, Some(13.0));
        assert_eq!(c.reasons.len(), 2);
        assert!(c
            .reasons
            .iter()
            .any(|r| r.text == "bridge batch inventory failed" && r.day == Some(1.0)));
        assert!(c.reasons.iter().any(|r| r.text == "inventory phase failed"));
        // A phase that never failed is no reason.
        assert!(!c.reasons.iter().any(|r| r.text.starts_with("lock")));
        // The collector counts failed runs here; until it counts all runs,
        // there is no rate.
        let q = step(&v, "qwen_load").detail.failures.as_ref().unwrap();
        assert_eq!(q.day.value, Some(5.0));
        assert_eq!(q.day_attempts, None);
        assert!(q.reasons.is_empty());
        // Nothing counts a shipper's failures: unknown, not none.
        assert!(step(&v, "quality_ship").detail.failures.is_none());
        let d = &step(&v, "qwen_load").detail;
        assert_eq!(d.last_ok, DateTime::from_timestamp(end, 0));
        assert_eq!(d.last_failed, DateTime::from_timestamp(end, 0));
        let c = &step(&v, "controller").detail;
        assert_eq!(c.last_failed, DateTime::from_timestamp(end - HOUR, 0));
        assert_eq!(c.last_ok, DateTime::from_timestamp(end, 0));
        assert_eq!(c.hours_searched, 2);
        assert_eq!(c.failed_hours_searched, 2);
        // The lock's seconds can arrive before anyone counted its failures.
        let lock = &step(&v, "stage").detail;
        assert_eq!(lock.failed_hours_searched, 0);
    }

    #[test]
    fn how_close_a_step_is_comes_from_what_was_reported() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        let mut body: serde_json::Value = serde_json::from_slice(&packet(
            "boinc",
            end,
            json!({"lock_seconds": [1800], "qwen_deferred": [3]}),
        ))
        .unwrap();
        body["limits"]["usnm_ocr.now"] = json!("225 runnable");
        let p = parse(&serde_json::to_vec(&body).unwrap(), "boinc", now()).unwrap();
        t.apply(p, now()).unwrap();
        let v = view(&t, &BTreeMap::new(), None, now());
        let lock = &step(&v, "bridge_lock").detail;
        assert_eq!(lock.busy_hour, Some(50.0));
        assert_eq!(lock.busy_day, Some(50.0));
        assert_eq!(
            step(&v, "usnm_ocr").detail.limit_now.as_deref(),
            Some("225 runnable")
        );
        assert_eq!(step(&v, "usnm_qwen").detail.deferred_day, Some(3.0));
        // A row of two lanes is held back if either is.
        assert_eq!(step(&v, "quality_ship").detail.deferred_day, Some(3.0));
        let quiet = &step(&v, "stage").detail;
        assert!(quiet.limit_now.is_none() && quiet.busy_hour.is_none());
        assert!(quiet.deferred_day.is_none());
    }

    #[test]
    fn each_step_names_its_sources_and_how_fresh_they_are() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(&mut t, "ship", end, json!({"quality_load_pages": [1]}));
        let later = now() + SOURCE_FRESH + Duration::minutes(1);
        let names = |v: &ThroughputView, id: &str| -> Vec<(&'static str, bool)> {
            step(v, id)
                .detail
                .sources
                .iter()
                .map(|s| (s.name, s.stale))
                .collect()
        };
        let v = view(&t, &BTreeMap::new(), None, now());
        assert_eq!(names(&v, "quality_load"), [("ship", false)]);
        assert_eq!(names(&v, "stage"), [("ship", false), ("boinc", true)]);
        assert_eq!(names(&v, "scan_fetch"), [(INDEX, true)]);
        let v = view(&t, &BTreeMap::new(), None, later);
        assert_eq!(names(&v, "quality_load"), [("ship", true)]);
    }

    #[test]
    fn the_scan_fetch_row_takes_what_only_the_fetch_knows() {
        let end = hour_of(now()) - HOUR;
        let fetch = [(end, (95, 95_000))].into();
        let mut v = view(
            &Throughput::default(),
            &fetch,
            Some((end - 23 * HOUR, end)),
            now(),
        );
        v.add_fetch(
            FetchFacts {
                read_at: Some(now()),
                failed_day: Some(5),
                last_failed: Some(now() - Duration::minutes(3)),
                reasons: vec![("transient: HTTP 520".into(), 4)],
                reasons_from: 50,
                seconds: Some((0.5, 4.0)),
            },
            now(),
        );
        let d = &step(&v, "scan_fetch").detail;
        assert_eq!(d.seconds_p50, Some(0.5));
        assert_eq!(d.seconds_p95, Some(4.0));
        assert!(!d.sources[0].stale);
        assert!(d.last_failed_exact);
        let f = d.failures.as_ref().unwrap();
        assert_eq!(f.day_attempts, Some(100.0));
        assert_eq!(f.day_failed_of_attempts, Some(5.0));
        assert_eq!(f.week.value, None);
        assert_eq!(f.reasons[0].day, Some(4.0));
        assert_eq!(f.reasons_from_latest, Some(50));
    }

    #[test]
    fn bins_cover_a_week_and_a_bin_nobody_observed_stays_unknown() {
        let mut t = Throughput::default();
        let end = hour_of(now()) - HOUR;
        apply(
            &mut t,
            "ship",
            end,
            json!({"quality_ship_pages": [30], "qwen_ship_pages": [10]}),
        );
        let v = view(&t, &BTreeMap::new(), None, now());
        let s = step(&v, "quality_ship");
        assert_eq!(s.bins.len(), (CHART_HOURS / BIN_HOURS) as usize);
        assert_eq!(s.bins.iter().flatten().count(), 1);
        assert_eq!(s.bins.iter().flatten().sum::<f64>(), 40.0);
        assert_eq!(s.bin_known.iter().sum::<u32>(), 1);
    }
}
