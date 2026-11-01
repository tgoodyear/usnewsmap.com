//! Host policies: what the fetcher may ask of a host, one file per host.
//!
//! A policy names the hosts it covers (`example.org`, or `*.example.org` for
//! anything under it), their request rate, how many transfers run at once,
//! a byte-rate cap, and what counts as push-back. Only `https://` URLs on a
//! host some policy covers are fetched; anything else is refused, redirect
//! hops included, so a new source needs a policy before its first request.
//!
//! [`Policies::builtin`] holds this crate's `policies/*.toml`; a deployment
//! can point at a directory of its own instead.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use serde::Deserialize;

/// The policy holding LoC's budget, which the scan fetcher shares through the same state file.
pub const LOC: &str = "loc";

fn one() -> u32 {
    1
}

/// One host's rules, as its file says them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// The file's stem.
    #[serde(skip)]
    pub name: String,
    /// How logs and errors name the host.
    pub label: String,
    pub hosts: Vec<String>,
    pub requests_per_minute: u32,
    /// The rate after a push-back, for every process, until an operator resets
    /// it: never lifted automatically. None: a push-back sets nothing lasting.
    #[serde(default)]
    pub slow_requests_per_minute: Option<u32>,
    /// Transfers at once.
    #[serde(default = "one")]
    pub concurrency: u32,
    /// All transfers to this host together, in bytes a second.
    #[serde(default)]
    pub max_bytes_per_sec: Option<u64>,
    /// A transfer under this rate for three minutes is abandoned.
    #[serde(default)]
    pub min_bytes_per_sec: Option<u64>,
    /// This process's own minimum between requests, on top of the shared rate.
    #[serde(default)]
    pub min_gap_secs: u64,
    /// Keep a `.part` across failures and restarts and ask for the rest.
    #[serde(default)]
    pub resume: bool,
    /// Tries per job. The wait grows by `retry_backoff_secs` each time.
    #[serde(default = "one")]
    pub attempts: u32,
    #[serde(default)]
    pub retry_backoff_secs: u64,
    /// This many transient failures in a row end the run's use of the host.
    #[serde(default)]
    pub max_failures_in_a_row: Option<u32>,
    /// A 5xx holds this process's requests to the host for at least this long.
    #[serde(default)]
    pub server_error_hold_secs: u64,
    pub push_back: PushBack,
    #[serde(default)]
    pub serving_probe: Option<ServingProbe>,
    /// Plain `http://`, for a stand-in server in tests. No file can set it.
    #[serde(skip)]
    pub allow_plain_http: bool,
}

/// What counts as the host objecting.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushBack {
    pub statuses: Vec<u16>,
    /// An HTML page instead of the file is a challenge or block page, whatever its status.
    pub html: bool,
    /// The hold without a `Retry-After`.
    pub default_hold_secs: u64,
    /// The longest hold a `Retry-After` can ask for.
    pub max_hold_secs: u64,
}

/// A page that shows whether our own traffic is suffering from this host's transfers.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServingProbe {
    pub url: String,
    pub every_secs: u64,
    /// Five probes with a median over this halve the byte cap.
    pub slow_secs: f64,
    pub floor_bytes_per_sec: u64,
}

/// The gap that keeps to `per_minute`, rounded up so the rate is never beaten.
fn gap(per_minute: u32) -> Duration {
    Duration::from_millis(60_000u64.div_ceil(u64::from(per_minute.max(1))))
}

impl Policy {
    pub fn parse(name: &str, text: &str) -> anyhow::Result<Self> {
        let mut p: Policy = toml::from_str(text).with_context(|| format!("policy {name}"))?;
        p.name = name.to_owned();
        p.check()?;
        Ok(p)
    }

    fn check(&self) -> anyhow::Result<()> {
        let n = &self.name;
        if self.hosts.is_empty() {
            bail!("policy {n}: no hosts");
        }
        if self.requests_per_minute == 0 || self.slow_requests_per_minute == Some(0) {
            bail!("policy {n}: a rate of zero");
        }
        if self.concurrency == 0 || self.attempts == 0 {
            bail!("policy {n}: concurrency and attempts start at 1");
        }
        for h in &self.hosts {
            let bare = h.strip_prefix("*.").unwrap_or(h);
            let ok = !bare.is_empty()
                && bare == bare.to_ascii_lowercase()
                && bare
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
            if !ok {
                bail!("policy {n}: not a host: {h}");
            }
        }
        Ok(())
    }

    /// Between requests in the shared budget.
    pub fn normal_gap(&self) -> Duration {
        gap(self.requests_per_minute)
    }

    /// Between requests after a push-back. Without a slow rate, the normal gap.
    pub fn slow_gap(&self) -> Duration {
        gap(self
            .slow_requests_per_minute
            .unwrap_or(self.requests_per_minute))
    }

    pub fn min_gap(&self) -> Duration {
        Duration::from_secs(self.min_gap_secs)
    }

    fn covers(&self, host: &str) -> bool {
        self.hosts.iter().any(|p| pattern_covers(p, host))
    }
}

fn pattern_covers(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(root) => host
            .strip_suffix(root)
            .is_some_and(|rest| rest.ends_with('.')),
        None => pattern == host,
    }
}

fn patterns_overlap(a: &str, b: &str) -> bool {
    let bare = |p: &'_ str| p.strip_prefix("*.").unwrap_or(p).to_owned();
    a == b || pattern_covers(a, &bare(b)) || pattern_covers(b, &bare(a))
}

/// Every policy in force.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policies {
    list: Vec<Policy>,
}

impl Policies {
    pub fn new(list: Vec<Policy>) -> anyhow::Result<Self> {
        for (i, a) in list.iter().enumerate() {
            for b in &list[i + 1..] {
                if a.name == b.name {
                    bail!("two policies named {}", a.name);
                }
                let shared = a
                    .hosts
                    .iter()
                    .find(|h| b.hosts.iter().any(|o| patterns_overlap(h, o)));
                if let Some(h) = shared {
                    bail!("policies {} and {} both cover {h}", a.name, b.name);
                }
            }
        }
        Ok(Self { list })
    }

    /// The policies built into this binary.
    pub fn builtin() -> Self {
        let list = [
            (LOC, include_str!("../../policies/loc.toml")),
            (
                "huggingface",
                include_str!("../../policies/huggingface.toml"),
            ),
        ]
        .into_iter()
        .map(|(n, t)| Policy::parse(n, t).expect("a built-in policy"))
        .collect();
        Self::new(list).expect("the built-in policies")
    }

    /// One policy per `*.toml` file in `dir`, named by the file's stem.
    pub fn load_dir(dir: &Path) -> anyhow::Result<Self> {
        let mut list = Vec::new();
        for e in std::fs::read_dir(dir).with_context(|| format!("{}", dir.display()))? {
            let path = e?.path();
            if path.extension().is_some_and(|x| x == "toml") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .with_context(|| format!("{}", path.display()))?;
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("{}", path.display()))?;
                list.push(Policy::parse(name, &text)?);
            }
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Self::new(list)
    }

    pub fn named_mut(&mut self, name: &str) -> Option<&mut Policy> {
        self.list.iter_mut().find(|p| p.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Policy> {
        self.list.iter()
    }

    /// The policy for `url`; the error says why the URL is refused.
    pub fn for_url(&self, url: &reqwest::Url) -> Result<&Policy, String> {
        let shown = super::plain(url);
        let Some(host) = url.host_str() else {
            return Err(format!("{shown}: no host"));
        };
        let Some(p) = self.list.iter().find(|p| p.covers(host)) else {
            return Err(format!("{shown}: no policy covers {host}"));
        };
        if url.scheme() != "https" && !p.allow_plain_http {
            return Err(format!("{shown}: not https"));
        }
        Ok(p)
    }
}
