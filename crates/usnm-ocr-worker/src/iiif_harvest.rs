//! A harvester for a IIIF collection (#1623), run by a person: `iiif-harvest --action plan`
//! walks the collection and its manifests and writes the list of issues, canvases and image
//! URLs; `--action fetch` fetches the images from that plan.
//!
//! It goes through the fetcher (#1413) as `ghn-fetch` does, with one policy: a single
//! connection, at most one request every 3 seconds in total (a hard minimum; a flag can only
//! slow it), `Retry-After` honoured, a push-back or repeated failures ending the run. It
//! reads each host's `robots.txt` first and refuses a path that file bars for its user agent
//! or for `*`. It has no built-in site: the collection URL is a flag. Nothing is ever deleted
//! or written over.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::fetcher::{
    Engine, EngineOptions, Fail, FileBudgets, Job, Policies, Policy, Reservation,
};
use crate::loc::USER_AGENT;

pub mod fetch;
pub mod iiif;
pub mod pace;
pub mod plan;
pub mod robots;
#[cfg(test)]
mod tests;

use iiif::Citation;
use pace::Pace;
use robots::Robots;

/// The most a JSON document may be.
const MAX_DOC: u64 = 64 << 20;
/// The most `robots.txt` read.
const MAX_ROBOTS: usize = 512 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Plan,
    Fetch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoJson {
    /// Only for a service whose manifest entry gives no profile.
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub action: Action,
    /// The collection's top document, for `plan`.
    pub collection: Option<String>,
    pub out: PathBuf,
    /// Where the pacer and run lock live, shared by every harvest.
    pub state_dir: PathBuf,
    /// The plan `fetch` reads; `<out>/plan.json` by default.
    pub plan: Option<PathBuf>,
    pub pace: Pace,
    /// Hosts besides the collection's that its documents may send the run to.
    pub allow_hosts: Vec<String>,
    pub info_json: InfoJson,
    /// Ask for images no bigger than this on the longer side.
    pub max_dimension: Option<u64>,
    /// `plan`: at most this many issues. `fetch`: at most this many images this run.
    pub limit: Option<usize>,
    pub max_image_bytes: u64,
}

/// What a run did.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub action: String,
    pub issues: usize,
    pub images_planned: usize,
    pub fetched: usize,
    pub already_have: usize,
    /// On disk without a matching sidecar: kept, not fetched again.
    pub kept_unverified: usize,
    pub failed: Vec<String>,
    /// Why the run stopped early, if it did.
    pub stopped: Option<String>,
    pub estimated_requests: usize,
    pub estimated_seconds: u64,
}

impl Summary {
    /// Whether the run did everything it set out to.
    pub fn clean(&self) -> bool {
        self.failed.is_empty() && self.stopped.is_none()
    }
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// What is recorded beside every file fetched, and for the top-level list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sidecar {
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
    pub fetched_at: String,
    /// The access date and time (UTC), as a citation gives it.
    pub accessed: String,
    pub user_agent: String,
    /// `image`, `manifest`, `collection`, `info`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation: Option<Citation>,
    /// What the file's first bytes look like (`jpeg`, `png`, `jp2`, `tiff`, `json`, `unknown`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sniffed: Option<String>,
}

/// The first bytes of an image, named.
pub fn sniff(head: &[u8]) -> &'static str {
    match head {
        [0xFF, 0xD8, 0xFF, ..] => "jpeg",
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0, 0, 0, 0x0C, b'j', b'P', ..] => "jp2",
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => "tiff",
        [b'G', b'I', b'F', ..] => "gif",
        _ => "unknown",
    }
}

/// Whether the file at `path` is a whole image of the kind its extension says: it starts
/// like one and, for JPEG and PNG, ends like one. The kind is returned.
pub(crate) fn whole_image(path: &Path) -> Result<&'static str, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut head = [0u8; 16];
    let n = f.read(&mut head).map_err(|e| e.to_string())?;
    let kind = sniff(&head[..n]);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let want = match ext {
        "jpg" | "jpeg" => Some("jpeg"),
        "png" => Some("png"),
        "tif" | "tiff" => Some("tiff"),
        "jp2" => Some("jp2"),
        "gif" => Some("gif"),
        _ => None,
    };
    if want.is_some_and(|w| w != kind) {
        return Err(format!("the bytes look like {kind}, not {ext}"));
    }
    let tail_len = len.min(64);
    f.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(|e| e.to_string())?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail).map_err(|e| e.to_string())?;
    let end = tail.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    match kind {
        "jpeg" if !tail[..end].ends_with(&[0xFF, 0xD9]) => Err("no end-of-image marker".to_owned()),
        "png" if !tail[..end].ends_with(b"IEND\xaeB`\x82") => Err("no IEND chunk".to_owned()),
        _ => Ok(kind),
    }
}

pub(crate) fn sidecar_path(file: &Path) -> PathBuf {
    let mut p = file.as_os_str().to_owned();
    p.push(".json");
    PathBuf::from(p)
}

/// The engine, the robots rules and the output directory, for one run.
pub struct Client {
    engine: Engine,
    stop: watch::Receiver<bool>,
    pub out: PathBuf,
    gap_secs: f64,
    robots: tokio::sync::Mutex<HashMap<String, Robots>>,
    allowed: Vec<String>,
    log: Mutex<()>,
    pub terms_url: Mutex<Option<String>>,
    _lock: std::fs::File,
    _host_lock: std::fs::File,
}

pub(crate) fn why(f: Fail) -> anyhow::Error {
    match f {
        Fail::Blocked(m) => anyhow::anyhow!("the host pushed back, stopping: {m}"),
        Fail::Transient(m) | Fail::Refused(m) | Fail::Checksum(m) => anyhow::anyhow!("{m}"),
        Fail::Stopped => anyhow::anyhow!("stopped"),
        Fail::Slowed(e) => e,
        Fail::RangeRefused | Fail::Closed => {
            anyhow::anyhow!("the run has stopped asking this host")
        }
    }
}

/// Whether a failure ends the run rather than one file.
pub fn ends_run(f: &Fail) -> bool {
    matches!(
        f,
        Fail::Blocked(_) | Fail::Stopped | Fail::Slowed(_) | Fail::Closed
    )
}

impl Client {
    /// `hosts` are every host the run may ask; one policy and one budget file cover them.
    pub fn new(
        out: &Path,
        state_dir: &Path,
        hosts: &[String],
        pace: Pace,
        stop: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(out.join("_state"))?;
        let lock = crate::bulk::hold_lock(&out.join("_state").join("run.lock"))
            .context("one run at a time per output directory")?;
        // The pacer and its lock belong to the machine's user, not the output directory or
        // one host: every harvest shares one pace and one push-back hold, and takes turns,
        // whatever hosts its documents name.
        std::fs::create_dir_all(state_dir)?;
        let host_lock = crate::bulk::hold_lock(&state_dir.join("iiif-harvest.run.lock"))
            .context("one harvest at a time")?;
        let pacer_file = state_dir.join("iiif-harvest.pacer.json");
        let policy: Policy = pace::policy(hosts, pace)?;
        let budgets = FileBudgets::new(None).with_file(pace::POLICY, pacer_file, None);
        let engine = Engine::new(
            Policies::new(vec![policy])?,
            Box::new(budgets),
            EngineOptions {
                who: "iiif-harvest".to_owned(),
                rate_file: None,
            },
        )?;
        Ok(Self {
            engine,
            stop,
            out: out.to_owned(),
            gap_secs: pace.gap().as_secs_f64(),
            robots: tokio::sync::Mutex::new(HashMap::new()),
            allowed: hosts.to_vec(),
            log: Mutex::new(()),
            terms_url: Mutex::new(None),
            _lock: lock,
            _host_lock: host_lock,
        })
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(365 * 24 * 3600)
    }

    /// Refuses a budget slowed by a push-back, until it lifts.
    pub async fn check_not_slowed(&self, url: &str) -> anyhow::Result<()> {
        self.engine.check_not_slowed(url).await
    }

    /// Reads the host's `robots.txt` once and returns an error if `url`'s path is barred for
    /// this tool's user agent or for `*`.
    pub async fn check_robots(&self, url: &str) -> anyhow::Result<()> {
        let u = reqwest::Url::parse(url).with_context(|| url.to_owned())?;
        let host = u.host_str().unwrap_or_default().to_owned();
        if !self.allowed.contains(&host) {
            bail!(
                "{url}: the host {host} isn't one this run may ask; add --allow-host {host} \
                 after reading its robots.txt and terms"
            );
        }
        let key = match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.clone(),
        };
        let mut map = self.robots.lock().await;
        if !map.contains_key(&key) {
            let r = self.load_robots(&u, &key).await?;
            map.insert(key.clone(), r);
        }
        let rules = &map[&key];
        let token = USER_AGENT.split('/').next().unwrap_or(USER_AGENT);
        let target = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        };
        if !rules.allows(token, &target) {
            bail!("{key}'s robots.txt bars {target} for {token} or for *: not fetching it");
        }
        Ok(())
    }

    async fn load_robots(&self, u: &reqwest::Url, key: &str) -> anyhow::Result<Robots> {
        let robots_url = format!("{}://{key}/robots.txt", u.scheme());
        let opened = self
            .engine
            .open(&robots_url, None, &self.stop, Self::deadline())
            .await;
        let rules = match opened {
            Ok(resp) => {
                let body = self
                    .engine
                    .read_capped(&robots_url, resp, MAX_ROBOTS, &self.stop)
                    .await
                    .map_err(why)?;
                let dir = self.out.join("_iiif").join("robots");
                std::fs::create_dir_all(&dir)?;
                let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
                let file = dir.join(format!("{}-{stamp}.txt", key.replace(':', "_")));
                crate::ghn::write_atomic(&file, &body)?;
                Robots::parse(&String::from_utf8_lossy(&body))
            }
            Err(Fail::Refused(m)) if m.ends_with("HTTP 404") || m.ends_with("HTTP 410") => {
                Robots::allow_all()
            }
            Err(f) => bail!(
                "{robots_url}: can't read robots.txt, so not fetching: {}",
                why(f)
            ),
        };
        let token = USER_AGENT.split('/').next().unwrap_or(USER_AGENT);
        let delay = rules.delay_secs(token);
        if delay > self.gap_secs + 0.001 {
            bail!(
                "{key}'s robots.txt asks for {delay} s between requests; run again with \
                 --min-gap-secs {}",
                delay.ceil()
            );
        }
        Ok(rules)
    }

    /// Appends to the top-level list of everything fetched.
    pub fn record(&self, entry: &Sidecar) -> anyhow::Result<()> {
        let _g = self.log.lock().expect("log lock");
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.out.join("fetched.jsonl"))?;
        std::io::Write::write_all(&mut f, &line)?;
        Ok(())
    }

    /// Fetches `url` to `dest` unless `dest` already exists (then it is read, never asked
    /// for again). Returns the bytes. A new file gets a sidecar and a line in `fetched.jsonl`.
    pub async fn doc(&self, url: &str, dest: &Path, tmpl: Sidecar) -> anyhow::Result<Vec<u8>> {
        if let Ok(bytes) = std::fs::read(dest) {
            // Reused only when its sidecar says it is this URL's and the bytes match; a
            // different collection's output directory is kept and reported, not read.
            let recorded = std::fs::read(sidecar_path(dest))
                .ok()
                .and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok());
            return match recorded {
                Some(s) if s.url == url && s.sha256 == crate::ghn::sha256_hex(&bytes) => Ok(bytes),
                _ => bail!(
                    "{} is not the saved copy of {url} (no sidecar, another URL, or other \
                     bytes); it is kept as it is. Use a fresh --out for a different collection",
                    dest.display()
                ),
            };
        }
        self.check_robots(url).await?;
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let got = self.get(url, dest, MAX_DOC, tmpl).await.map_err(why)?;
        let _ = got;
        std::fs::read(dest).with_context(|| dest.display().to_string())
    }

    /// One file through the fetcher, then its sidecar. The caller has checked `robots.txt`
    /// and that `dest` doesn't exist.
    pub async fn get(
        &self,
        url: &str,
        dest: &Path,
        max: u64,
        tmpl: Sidecar,
    ) -> Result<Sidecar, Fail> {
        let part_dir = self.out.join(".part");
        std::fs::create_dir_all(&part_dir).map_err(|e| Fail::Refused(e.to_string()))?;
        let name = dest
            .strip_prefix(&self.out)
            .unwrap_or(dest)
            .to_string_lossy()
            .replace(['/', '\\'], "__");
        let job = Job {
            name: name.clone(),
            size: max,
            sha256: None,
            dest: dest.to_owned(),
            sources: vec![url.to_owned()],
        };
        let total = std::sync::atomic::AtomicU64::new(0);
        let room = Reservation::new(&total);
        room.set(max);
        let got = self
            .engine
            .fetch(
                &job,
                url,
                &part_dir.join(format!("{name}.part")),
                &self.stop,
                Self::deadline(),
                &room,
            )
            .await?;
        let mut sc = tmpl;
        sc.url = url.to_owned();
        sc.bytes = got.size;
        sc.sha256 = got.sha256;
        sc.fetched_at = now();
        sc.accessed = sc.fetched_at.clone();
        sc.user_agent = USER_AGENT.to_owned();
        if sc.kind == "image" {
            match whole_image(dest) {
                Ok(kind) => sc.sniffed = Some(kind.to_owned()),
                Err(why) => {
                    // Kept beside the name, never deleted; the name is free for a later run.
                    let kept = crate::fetcher::set_aside(dest)
                        .and_then(|aside| std::fs::remove_file(dest).map(|()| aside));
                    return Err(Fail::Refused(match kept {
                        Ok(a) => {
                            format!("{url}: not a whole image ({why}); kept as {}", a.display())
                        }
                        Err(e) => format!("{url}: not a whole image ({why}); left in place: {e}"),
                    }));
                }
            }
        } else {
            sc.sniffed = Some("json".to_owned());
        }
        let write = crate::ghn::write_atomic(
            &sidecar_path(dest),
            &serde_json::to_vec_pretty(&sc).map_err(|e| Fail::Refused(e.to_string()))?,
        );
        write.map_err(|e| Fail::Refused(format!("{}: {e}", dest.display())))?;
        self.record(&sc).map_err(|e| Fail::Refused(e.to_string()))?;
        Ok(sc)
    }
}

/// A blank sidecar of `kind` to fill in.
pub fn template(kind: &str, terms: Option<&str>) -> Sidecar {
    Sidecar {
        url: String::new(),
        bytes: 0,
        sha256: String::new(),
        fetched_at: String::new(),
        accessed: String::new(),
        user_agent: String::new(),
        kind: kind.to_owned(),
        issue: None,
        canvas_index: None,
        terms_url: terms.map(str::to_owned),
        citation: None,
        sniffed: None,
    }
}

/// The hosts a run may ask: the collection's and the `--allow-host` ones.
pub fn hosts_of(url: &str, extra: &[String]) -> anyhow::Result<Vec<String>> {
    let u = reqwest::Url::parse(url).with_context(|| format!("not a URL: {url}"))?;
    let mut hosts = vec![u.host_str().context("no host")?.to_owned()];
    for h in extra {
        if !hosts.contains(h) {
            hosts.push(h.clone());
        }
    }
    Ok(hosts)
}

/// Runs the action in `opts`.
pub async fn run(opts: Options, stop: watch::Receiver<bool>) -> anyhow::Result<Summary> {
    match opts.action {
        Action::Plan => plan::run(opts, stop).await,
        Action::Fetch => fetch::run(opts, stop).await,
    }
}

pub(crate) fn json_of(bytes: &[u8], what: &str) -> anyhow::Result<Value> {
    serde_json::from_slice(bytes).with_context(|| format!("{what} isn't JSON"))
}
