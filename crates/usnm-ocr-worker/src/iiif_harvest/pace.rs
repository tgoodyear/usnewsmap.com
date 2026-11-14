//! The pace: one request every [`MIN_GAP_SECS`] seconds at the fastest, in total.

use std::time::Duration;

use super::Policy;

/// The fastest the tool goes, the site's stated `Crawl-delay` and `Request-rate`. A flag can
/// slow the tool, never lower this.
pub const MIN_GAP_SECS: u64 = 3;

/// The name of the one policy, which holds every host the run asks.
pub const POLICY: &str = "iiif-harvest";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    gap: Duration,
}

impl Pace {
    /// The gap asked for with `--min-gap-secs`; below the minimum is an error, not a clamp.
    pub fn new(requested_secs: u64) -> anyhow::Result<Self> {
        anyhow::ensure!(
            requested_secs >= MIN_GAP_SECS,
            "--min-gap-secs {requested_secs} is below the {MIN_GAP_SECS} s the site asks for; \
             it can only be raised"
        );
        Ok(Self {
            gap: Duration::from_secs(requested_secs),
        })
    }

    /// Tests only: a gap short enough not to wait.
    #[cfg(test)]
    pub fn for_test(gap: Duration) -> Self {
        Self { gap }
    }

    pub fn gap(&self) -> Duration {
        self.gap
    }
}

/// Whether `host` is this machine, the only place plain `http://` is spoken to.
pub fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
}

/// The one policy for `hosts`: one connection, `pace` between requests, retries with
/// back-off, and a push-back (403 or 429, or an HTML page) that holds the budget for its
/// `Retry-After`, an hour without one.
pub fn policy(hosts: &[String], pace: Pace) -> anyhow::Result<Policy> {
    let loopback = hosts.iter().all(|h| is_loopback(h));
    anyhow::ensure!(
        loopback || !hosts.iter().any(|h| is_loopback(h)),
        "a run asks either this machine or remote hosts, not both"
    );
    #[cfg(test)]
    assert!(
        loopback,
        "a test would ask a host that isn't this machine: {hosts:?}"
    );
    let ms = u64::try_from(pace.gap.as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    let per_minute = (60_000 / ms).clamp(1, u64::from(u32::MAX));
    let secs = pace.gap.as_secs() + u64::from(pace.gap.subsec_nanos() > 0);
    let backoff = if cfg!(test) { 0 } else { 30 };
    let text = format!(
        r#"
label = "IIIF harvest"
hosts = ["placeholder.invalid"]
requests_per_minute = {per_minute}
slow_requests_per_minute = 1
slow_for_secs = 86400
min_gap_secs = {secs}
concurrency = 1
attempts = 3
retry_backoff_secs = {backoff}
max_failures_in_a_row = 5
server_error_hold_secs = {backoff}
[push_back]
statuses = [403, 429]
html = true
default_hold_secs = 3600
max_hold_secs = 86400
"#
    );
    let mut p = Policy::parse(POLICY, &text)?;
    p.hosts = hosts.to_vec();
    p.allow_plain_http = loopback;
    Ok(p)
}
