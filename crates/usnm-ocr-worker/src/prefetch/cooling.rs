//! Persisted per-issue item errors, cool-offs and permanent failures.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An issue whose item was an item error, in `item-errors.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemError {
    /// Item errors seen; past `item_max_cooloffs` the issue is permanent.
    pub errors: u32,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    /// Asked again after this; none once permanent.
    pub retry_after: Option<DateTime<Utc>>,
    pub error: String,
}

/// What [`Cooling::check`] says of an issue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Check {
    Go,
    Cooling(DateTime<Utc>),
    Permanent,
}

/// The issues cooling off after an item error, kept in a file.
pub(super) struct Cooling {
    path: PathBuf,
    cooloff: chrono::Duration,
    max: u32,
    issues: Mutex<BTreeMap<String, ItemError>>,
}

impl Cooling {
    pub(super) fn load(path: PathBuf, cooloff: Duration, max: u32) -> anyhow::Result<Self> {
        let issues = match std::fs::read(&path) {
            Ok(b) => {
                serde_json::from_slice(&b).with_context(|| format!("reading {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path,
            cooloff: chrono::Duration::from_std(cooloff).context("--item-cooloff-secs")?,
            max,
            issues: Mutex::new(issues),
        })
    }

    fn save(&self, issues: &BTreeMap<String, ItemError>) -> anyhow::Result<()> {
        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, serde_json::to_vec_pretty(issues)?)?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("writing {}", self.path.display()))
    }

    fn check_in(issues: &BTreeMap<String, ItemError>, issue: &str) -> Check {
        match issues.get(issue) {
            None => Check::Go,
            Some(e) => match e.retry_after {
                None => Check::Permanent,
                Some(t) if t > Utc::now() => Check::Cooling(t),
                Some(_) => Check::Go,
            },
        }
    }

    pub(super) fn check(&self, issue: &str) -> Check {
        Self::check_in(&self.issues.lock().expect("cooling lock"), issue)
    }

    /// An item error for `issue`: a cool-off, or permanent after the last.
    /// Pages of one issue in flight together share one cool-off.
    pub(super) fn mark(&self, issue: &str, error: &str) -> anyhow::Result<Check> {
        let mut issues = self.issues.lock().expect("cooling lock");
        let now = Utc::now();
        match Self::check_in(&issues, issue) {
            Check::Go => {}
            c => return Ok(c),
        }
        let e = issues.entry(issue.to_owned()).or_insert(ItemError {
            errors: 0,
            first_at: now,
            last_at: now,
            retry_after: None,
            error: String::new(),
        });
        e.errors += 1;
        e.last_at = now;
        e.error = error.chars().take(300).collect();
        e.retry_after = (e.errors <= self.max).then(|| now + self.cooloff);
        let c = e.retry_after.map_or(Check::Permanent, Check::Cooling);
        tracing::warn!(
            target: "usnm_ocr_worker::prefetch",
            issue,
            errors = e.errors,
            retry_after = ?e.retry_after,
            "skipping an issue whose item loc.gov can't serve"
        );
        self.save(&issues)?;
        Ok(c)
    }

    /// The issue's item answered: forget its item errors.
    pub(super) fn clear(&self, issue: &str) -> anyhow::Result<()> {
        let mut issues = self.issues.lock().expect("cooling lock");
        if issues.remove(issue).is_some() {
            self.save(&issues)?;
        }
        Ok(())
    }

    /// (cooling, permanent) issues.
    pub(super) fn counts(&self) -> (u64, u64) {
        let issues = self.issues.lock().expect("cooling lock");
        let now = Utc::now();
        issues
            .values()
            .fold((0, 0), |(c, p), e| match e.retry_after {
                None => (c, p + 1),
                Some(t) if t > now => (c + 1, p),
                Some(_) => (c, p),
            })
    }
}
