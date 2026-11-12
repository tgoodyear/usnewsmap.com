//! The BOINC week: the last seven days of the BOINC server in 3-hour
//! buckets, queued jobs, active volunteers and machines, and completed jobs.
//!
//! `scripts/ocr-node/boinc-week-collect.py` on the BOINC server counts them
//! from the `result` table and `PUT`s one document to `/api/boinc-week`,
//! from the address the `boinc` throughput collector pushes from. The
//! document is counts and app names only; [`parse`] takes nothing else, so
//! no account, host or address can reach the page. The last document is
//! kept in the state directory, so the chart outlives a restart.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub const BUCKET_HOURS: u32 = 3;
/// A week of 3-hour buckets is 56; a clock change can add one.
pub const MAX_BUCKETS: usize = 64;
pub const MAX_APPS: usize = 32;
const MAX_COUNT: u64 = 1_000_000_000;
/// The largest document taken, and the largest file read back.
pub const MAX_BYTES: usize = 128 * 1024;
/// The collector runs every five minutes: older than this, the chart says so.
pub const FRESH: Duration = Duration::minutes(20);

/// What the collector sends: whole buckets, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Week {
    pub schema: u32,
    /// When the server counted them, in epoch seconds.
    pub generated_at: i64,
    /// US Eastern's offset from UTC at `generated_at`; the last bucket's
    /// if not sent.
    #[serde(default)]
    pub generated_utc_offset_minutes: Option<i32>,
    pub bucket_hours: u32,
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "BucketWire")]
pub struct Bucket {
    /// Epoch seconds; a US Eastern 3-hour boundary.
    pub start: i64,
    /// US Eastern's offset from UTC at `start`, so the page can label it.
    pub utc_offset_minutes: i32,
    /// The bucket isn't over yet.
    pub partial: bool,
    /// Unsent results at the bucket's end, by app.
    pub queued: BTreeMap<String, u64>,
    /// Successful results received in the bucket, by app.
    pub completed: BTreeMap<String, u64>,
    /// Accounts and machines that were sent or reported a result in the bucket.
    pub volunteers: u64,
    pub hosts: u64,
}

/// A bucket as pushed or stored. Documents from before every account counted
/// as a volunteer have `accounts` (everyone) beside a smaller `volunteers`
/// and a `volunteer_hosts`; they read as `volunteers = accounts` and `hosts`
/// as sent, until the collector's next push replaces them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BucketWire {
    start: i64,
    utc_offset_minutes: i32,
    partial: bool,
    queued: BTreeMap<String, u64>,
    completed: BTreeMap<String, u64>,
    volunteers: u64,
    hosts: u64,
    #[serde(default)]
    accounts: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    volunteer_hosts: Option<u64>,
}

impl From<BucketWire> for Bucket {
    fn from(w: BucketWire) -> Self {
        Bucket {
            start: w.start,
            utc_offset_minutes: w.utc_offset_minutes,
            partial: w.partial,
            queued: w.queued,
            completed: w.completed,
            volunteers: w.accounts.unwrap_or(w.volunteers),
            hosts: w.hosts,
        }
    }
}

impl Bucket {
    pub fn queued_total(&self) -> u64 {
        self.queued.values().sum()
    }

    pub fn completed_total(&self) -> u64 {
        self.completed.values().sum()
    }
}

/// The project's BOINC apps, live and retired. The public view shows any
/// other key as [`OTHER_APP`], so a name that only looks like an app can't
/// reach it.
pub const KNOWN_APPS: &[&str] = &[
    "usnm_assess",
    "usnm_gold",
    "usnm_gold_native",
    "usnm_iris",
    "usnm_ndlocr",
    "usnm_ocr",
    "usnm_quality",
    "usnm_qwen",
    "usnm_qwen_train",
    "usnm_surya",
];
pub const OTHER_APP: &str = "other";

/// An app's short name: `usnm_` and lowercase, digits and `_`.
pub fn app_ok(s: &str) -> bool {
    s.len() <= 40
        && s.strip_prefix("usnm_").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// Counts by app with every app not in [`KNOWN_APPS`] folded into
/// [`OTHER_APP`].
pub fn known_apps_only(apps: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for (app, n) in apps {
        let key = if KNOWN_APPS.contains(&app.as_str()) {
            app.as_str()
        } else {
            OTHER_APP
        };
        *out.entry(key.to_owned()).or_insert(0) += n;
    }
    out
}

/// A pushed document, checked. Anything but counts and app names is refused.
pub fn parse(body: &[u8], now: DateTime<Utc>) -> anyhow::Result<Week> {
    ensure!(body.len() <= MAX_BYTES, "over {MAX_BYTES} bytes");
    let w: Week = serde_json::from_slice(body).context("malformed BOINC week")?;
    check(&w, now)?;
    Ok(w)
}

fn check(w: &Week, now: DateTime<Utc>) -> anyhow::Result<()> {
    ensure!(w.schema == 1, "unsupported schema");
    ensure!(w.bucket_hours == BUCKET_HOURS, "buckets aren't 3 hours");
    ensure!(
        (1..=MAX_BUCKETS).contains(&w.buckets.len()),
        "bucket count out of range"
    );
    ensure!(
        w.generated_at > 0 && w.generated_at <= now.timestamp() + 60,
        "generated_at is in the future"
    );
    ensure!(
        w.generated_utc_offset_minutes
            .is_none_or(|m| (-720..=840).contains(&m)),
        "bad UTC offset"
    );
    let mut last = 0;
    for b in &w.buckets {
        ensure!(
            b.start > last && b.start % 3600 == 0,
            "bucket starts out of order or off the hour"
        );
        last = b.start;
        ensure!(
            (-720..=840).contains(&b.utc_offset_minutes),
            "bad UTC offset"
        );
        for apps in [&b.queued, &b.completed] {
            ensure!(apps.len() <= MAX_APPS, "too many apps");
            ensure!(apps.keys().all(|a| app_ok(a)), "bad app name");
            ensure!(apps.values().all(|n| *n <= MAX_COUNT), "bad count");
        }
        ensure!(
            b.volunteers <= MAX_COUNT && b.hosts <= MAX_COUNT,
            "bad count"
        );
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    received: DateTime<Utc>,
    week: Week,
}

/// The last document, and the file that keeps it.
#[derive(Debug, Default)]
pub struct BoincWeek {
    path: Option<PathBuf>,
    stored: Option<Stored>,
    error: Option<String>,
}

impl BoincWeek {
    /// Opens `<state dir>/boinc-week.json`. A missing or broken file is no
    /// chart until the next push; a broken one is set aside.
    pub fn open(state_dir: &Path) -> Self {
        let path = state_dir.join("boinc-week.json");
        let mut w = BoincWeek {
            path: Some(path.clone()),
            ..BoincWeek::default()
        };
        match read_file(&path) {
            Ok(s) => w.stored = s,
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "can't read the BOINC week");
                w.error = Some("the saved BOINC week was unreadable; waiting for the next".into());
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            }
        }
        w
    }

    /// Takes a checked document and saves it.
    pub fn put(&mut self, week: Week, now: DateTime<Utc>) -> anyhow::Result<()> {
        self.stored = Some(Stored {
            received: now,
            week,
        });
        self.error = None;
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self.stored)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    pub fn view(&self, now: DateTime<Utc>) -> WeekView {
        let Some(s) = &self.stored else {
            return WeekView {
                error: self.error.clone(),
                ..WeekView::default()
            };
        };
        let generated_at = DateTime::from_timestamp(s.week.generated_at, 0);
        WeekView {
            received: Some(s.received),
            generated_at,
            stale: generated_at.is_none_or(|g| now - g > FRESH),
            week: Some(s.week.clone()),
            error: self.error.clone(),
        }
    }
}

fn read_file(path: &Path) -> anyhow::Result<Option<Stored>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(2 * MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2 * MAX_BYTES, "too large");
    let stored: Stored = serde_json::from_slice(&bytes)?;
    check(&stored.week, stored.received + Duration::minutes(1))?;
    Ok(Some(stored))
}

/// What the page and `/api/view.json` show: counts only.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WeekView {
    pub received: Option<DateTime<Utc>>,
    pub generated_at: Option<DateTime<Utc>>,
    /// The server hasn't counted for over [`FRESH`].
    pub stale: bool,
    pub week: Option<Week>,
    pub error: Option<String>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// A week ending at `now`'s next 3-hour boundary, with something in each bucket.
    pub(crate) fn document(now: DateTime<Utc>) -> serde_json::Value {
        let end = (now.timestamp() / 10800 + 1) * 10800;
        let buckets: Vec<_> = (0..56)
            .map(|i| {
                let start = end - (56 - i) * 10800;
                json!({
                    "hosts": 7, "volunteers": 4,
                    "completed": {"usnm_quality": 200 + i, "usnm_qwen": 30},
                    "queued": {"usnm_qwen": 1700 - i},
                    "partial": i == 55, "start": start, "utc_offset_minutes": -300,
                })
            })
            .collect();
        json!({"schema": 1, "generated_at": now.timestamp(), "bucket_hours": 3, "buckets": buckets})
    }

    #[test]
    fn the_old_shape_reads_with_everyone_counted() {
        let now = Utc::now();
        let mut d = document(now);
        for b in d["buckets"].as_array_mut().unwrap() {
            b["accounts"] = json!(4);
            b["volunteers"] = json!(2);
            b["volunteer_hosts"] = json!(3);
            b["hosts"] = json!(7);
        }
        let w = parse(&serde_json::to_vec(&d).unwrap(), now).unwrap();
        assert_eq!((w.buckets[0].volunteers, w.buckets[0].hosts), (4, 7));
        // Saved again, it has the new shape only.
        let saved = serde_json::to_value(&w).unwrap();
        assert!(saved["buckets"][0].get("accounts").is_none());
        assert!(saved["buckets"][0].get("volunteer_hosts").is_none());
    }

    #[test]
    fn takes_counts_and_app_names_only() {
        let now = Utc::now();
        let good = document(now);
        let w = parse(&serde_json::to_vec(&good).unwrap(), now).unwrap();
        assert_eq!(w.buckets.len(), 56);
        assert_eq!(w.buckets[0].queued_total(), 1700);
        assert_eq!(w.buckets[1].completed_total(), 231);
        let refused = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut d = good.clone();
            edit(&mut d);
            parse(&serde_json::to_vec(&d).unwrap(), now).is_err()
        };
        // Names, ids and addresses have nowhere to go.
        assert!(refused(&|d| d["buckets"][0]["users"] = json!(["tgoodyear"])));
        assert!(refused(&|d| d["buckets"][0]["hostid"] = json!(12)));
        assert!(refused(&|d| d["host"] = json!("pve")));
        assert!(refused(
            &|d| d["buckets"][0]["queued"]["pve-big.tjg.fyi"] = json!(1)
        ));
        assert!(refused(
            &|d| d["buckets"][0]["completed"]["Issue559 acceptance"] = json!(1)
        ));
        assert!(refused(
            &|d| d["buckets"][0]["completed"]["192.0.2.7"] = json!(1)
        ));
        assert!(refused(
            &|d| d["buckets"][0]["queued"]["tgoodyear"] = json!(1)
        ));
        assert!(refused(
            &|d| d["buckets"][0]["completed"]["hostid_12"] = json!(1)
        ));
        assert!(refused(&|d| d["generated_utc_offset_minutes"] = json!(5000)));
        assert!(refused(
            &|d| d["buckets"][0]["volunteers"] = json!("boinc-test")
        ));
        // And the rest holds together.
        assert!(refused(&|d| d["schema"] = json!(2)));
        assert!(refused(&|d| d["bucket_hours"] = json!(4)));
        assert!(refused(
            &|d| d["generated_at"] = json!(now.timestamp() + 3600)
        ));
        assert!(refused(
            &|d| d["buckets"][1]["start"] = d["buckets"][0]["start"].clone()
        ));
        assert!(refused(
            &|d| d["buckets"][0]["volunteers"] = json!(2_000_000_000u64)
        ));
        assert!(refused(&|d| d["buckets"] = json!([])));
    }

    #[test]
    fn apps_not_on_the_list_are_other() {
        let apps: BTreeMap<String, u64> = [
            ("usnm_qwen".to_owned(), 3),
            ("usnm_tgoodyear".to_owned(), 1),
            ("usnm_hostid_12".to_owned(), 2),
        ]
        .into();
        let kept = known_apps_only(&apps);
        assert_eq!(
            kept,
            [("other".to_owned(), 3), ("usnm_qwen".to_owned(), 3)].into()
        );
    }

    #[test]
    fn the_last_week_outlives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut w = BoincWeek::open(dir.path());
        assert!(w.view(now).week.is_none());
        let doc = parse(&serde_json::to_vec(&document(now)).unwrap(), now).unwrap();
        w.put(doc.clone(), now).unwrap();
        let again = BoincWeek::open(dir.path());
        let v = again.view(now);
        assert_eq!(v.week, Some(doc));
        assert!(!v.stale);
        assert!(again.view(now + Duration::minutes(30)).stale);
        std::fs::write(dir.path().join("boinc-week.json"), b"{").unwrap();
        let broken = BoincWeek::open(dir.path());
        assert!(broken.view(now).week.is_none());
        assert!(broken.view(now).error.is_some());
        assert!(dir.path().join("boinc-week.json.bad").exists());
    }
}
