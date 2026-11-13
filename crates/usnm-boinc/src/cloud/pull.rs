//! `cloud pull`: the cloud server's assimilation API into the stock assimilator's spool.
//!
//! Layout written under the spool root, matching what `collect` and `handoff::put_spool`
//! read (`collect` in lib.rs reads `<wu>__file_*`, `<wu>_error`, `<wu>_no_output_files`;
//! `handoff::spool_names` lists the same names):
//!
//! - `results/<batch>/<wu>__file_<open_name>` for each output file,
//! - `results/<batch>/<wu>_error` holding `0x<hex>\n` (rewritten, not appended) when the
//!   workunit failed,
//! - `results/<batch>/<wu>_no_output_files` when it succeeded with no stored files,
//! - `results/<batch>/<wu>__host_id`, the canonical result's host id in decimal, written last
//!   for a success so it also marks the workunit complete.
//!
//! Every file is downloaded into `.incoming/` (same filesystem), checked against the listed
//! size and MD5, fsynced, renamed into place, and the directory fsynced. A workunit is
//! acknowledged only after all of its files are in place, and never with `--no-ack`. A rerun
//! after a crash skips files already in place that verify, so it is safe at any point. Run one
//! puller per spool.
use anyhow::{anyhow, bail, ensure, Context};
use md5::{Digest, Md5};
use reqwest::{Client, Url};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Config {
    base: Url,
    token: String,
    spool: PathBuf,
    ack: bool,
    apps: Vec<String>,
    page_size: u32,
    client: Client,
}

impl Config {
    pub fn new(
        base_url: &str,
        token: String,
        spool: PathBuf,
        ack: bool,
        apps: Vec<String>,
        page_size: u32,
    ) -> anyhow::Result<Config> {
        let mut base = Url::parse(base_url).context("--base-url")?;
        let host = base.host_str().unwrap_or("");
        let loopback = host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .map_or(host.eq_ignore_ascii_case("localhost"), |a| a.is_loopback());
        ensure!(
            base.scheme() == "https" || (base.scheme() == "http" && loopback),
            "--base-url must be https (plain http is allowed only for a loopback host), \
             because every request carries the admin token"
        );
        // Keep a trailing slash off so segments append cleanly.
        base.path_segments_mut()
            .map_err(|()| anyhow!("--base-url cannot carry a path"))?
            .pop_if_empty();
        // No redirects: the token must go only to the server named by --base-url.
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .build()?;
        Ok(Config {
            base,
            token,
            spool,
            ack,
            apps,
            page_size: page_size.clamp(1, 500),
            client,
        })
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut u = self.base.clone();
        {
            let mut s = u.path_segments_mut().expect("checked in new");
            s.pop_if_empty().push("admin").push("v1");
            s.extend(segments);
        }
        u
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub workunits: usize,
    pub files_written: usize,
    pub files_skipped: usize,
    pub acked: usize,
    pub already_acked: usize,
    pub failed: Vec<String>,
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pulled {} workunits ({} files written, {} already present), acknowledged {} ({} already acknowledged), failed {}",
            self.workunits,
            self.files_written,
            self.files_skipped,
            self.acked,
            self.already_acked,
            self.failed.len()
        )?;
        for w in &self.failed {
            write!(f, "\n  failed: {w}")?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Page {
    workunits: Vec<Workunit>,
    has_more: bool,
    next_after: Option<i64>,
}

#[derive(Deserialize)]
struct Workunit {
    id: i64,
    name: String,
    #[serde(default)]
    batch: i64,
    #[serde(default)]
    error_mask: i64,
    canonical_result: Option<Canonical>,
}

#[derive(Deserialize)]
struct Canonical {
    #[serde(default)]
    host_id: i64,
    #[serde(default)]
    files: Vec<OutFile>,
}

#[derive(Deserialize)]
struct OutFile {
    name: String,
    open_name: Option<String>,
    #[serde(default)]
    index: Option<i64>,
    /// Absent on an older server, which lists only files in storage: then true.
    #[serde(default = "yes")]
    present: bool,
    #[serde(default)]
    nbytes: u64,
    #[serde(default)]
    md5: String,
}

fn yes() -> bool {
    true
}

impl OutFile {
    /// The name after `__file_`: the template's `open_name`, or for a file without one the
    /// template position (the server's documented suggestion).
    fn logical_name(&self) -> anyhow::Result<String> {
        match (&self.open_name, self.index) {
            (Some(n), _) => Ok(n.clone()),
            (None, Some(i)) => Ok(i.to_string()),
            (None, None) => bail!("output {:?} has no open_name and no index", self.name),
        }
    }
}

/// Longest file name component on the spool's filesystems.
const MAX_NAME: usize = 255;

/// A bounded staging path: batch plus the MD5 of the destination name (a name, not a secret).
fn staging(incoming: &Path, batch: i64, final_name: &str) -> PathBuf {
    incoming.join(format!("{batch}.{}.part", hex(&Md5::digest(final_name))))
}

/// Names that become path components or URL segments: no separators, no `..`, not empty.
pub fn safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s != "."
        && !s.contains("..")
        && !s.contains(['/', '\\', '\0'])
        && !s.chars().any(char::is_control)
}

/// One pass over every `--app`: pages through the ready list, writes what it can, and
/// acknowledges each page's fully written workunits. A workunit that fails is left
/// unacknowledged and listed in `failed`; the others continue.
pub async fn pull_once(cfg: &Config, limit: Option<usize>) -> anyhow::Result<Stats> {
    ensure!(
        cfg.spool.is_dir(),
        "--spool-dir {} does not exist; create it first (and sync its parent) so it is durable",
        cfg.spool.display()
    );
    let mut stats = Stats::default();
    let mut left = limit.unwrap_or(usize::MAX);
    // No --app: one pass over every app.
    let apps: Vec<Option<&str>> = if cfg.apps.is_empty() {
        vec![None]
    } else {
        cfg.apps.iter().map(|a| Some(a.as_str())).collect()
    };
    for app in apps {
        let mut after = 0i64;
        while left > 0 {
            let n = cfg.page_size.min(u32::try_from(left).unwrap_or(u32::MAX));
            let page = list(cfg, app, after, n).await?;
            let mut done = Vec::new();
            for wu in &page.workunits {
                if left == 0 {
                    break;
                }
                left -= 1;
                stats.workunits += 1;
                match pull_workunit(cfg, wu, &mut stats).await {
                    Ok(()) => done.push(wu.id),
                    Err(e) => stats.failed.push(format!("{}: {e:#}", wu.name)),
                }
            }
            if cfg.ack && !done.is_empty() {
                let r = ack(cfg, &done).await?;
                stats.acked += r.acknowledged.len();
                stats.already_acked += r.already_acked.len();
                eprintln!(
                    "ack: {} acknowledged, {} already acknowledged, {} not ready, {} unknown",
                    r.acknowledged.len(),
                    r.already_acked.len(),
                    r.not_ready.len(),
                    r.unknown.len()
                );
                for id in &r.not_ready {
                    stats.failed.push(format!(
                        "workunit {id}: the server says it is not ready to assimilate"
                    ));
                }
                for id in &r.unknown {
                    stats
                        .failed
                        .push(format!("workunit {id}: the server does not know this id"));
                }
            }
            match (page.has_more, page.next_after) {
                (true, Some(next)) if next > after => after = next,
                (true, _) => bail!("server reported more workunits without a usable next_after"),
                _ => break,
            }
        }
    }
    Ok(stats)
}

async fn list(cfg: &Config, app: Option<&str>, after: i64, limit: u32) -> anyhow::Result<Page> {
    let mut query = vec![
        ("state", "ready".to_owned()),
        ("after", after.to_string()),
        ("limit", limit.to_string()),
    ];
    if let Some(app) = app {
        query.push(("app", app.to_owned()));
    }
    let resp = cfg
        .client
        .get(cfg.url(&["assimilate"]))
        .query(&query)
        .bearer_auth(&cfg.token)
        .send()
        .await
        .context("listing ready workunits")?;
    let status = resp.status();
    ensure!(
        status.is_success(),
        "listing ready workunits: HTTP {status}"
    );
    resp.json().await.context("parsing the ready list")
}

/// The ack reply. An older server sends only `acknowledged`.
#[derive(Deserialize)]
struct Acked {
    // Required: a reply without it is not an ack reply.
    acknowledged: Vec<i64>,
    #[serde(default)]
    already_acked: Vec<i64>,
    #[serde(default)]
    not_ready: Vec<i64>,
    #[serde(default)]
    unknown: Vec<i64>,
}

async fn ack(cfg: &Config, ids: &[i64]) -> anyhow::Result<Acked> {
    let resp = cfg
        .client
        .post(cfg.url(&["assimilate", "ack"]))
        .bearer_auth(&cfg.token)
        .json(&serde_json::json!({ "workunit_ids": ids }))
        .send()
        .await
        .context("acknowledging")?;
    let status = resp.status();
    ensure!(status.is_success(), "acknowledging: HTTP {status}");
    Ok(resp.json::<Acked>().await?)
}

async fn pull_workunit(cfg: &Config, wu: &Workunit, stats: &mut Stats) -> anyhow::Result<()> {
    ensure!(
        safe_component(&wu.name),
        "unsafe workunit name {:?}",
        wu.name
    );
    ensure!(wu.batch >= 0, "negative batch {}", wu.batch);
    let dir = cfg.spool.join("results").join(wu.batch.to_string());
    let incoming = cfg.spool.join(".incoming");
    // Check every name before touching the filesystem.
    let files = match (&wu.canonical_result, wu.error_mask) {
        (_, m) if m != 0 => &[][..],
        (Some(c), _) => &c.files[..],
        (None, _) => bail!("no error and no canonical result"),
    };
    // Whole basenames, so a name that fits as a component is the only kind written.
    let longest_marker = wu.name.len() + "_no_output_files".len();
    ensure!(
        longest_marker <= MAX_NAME,
        "workunit name {} is too long for its marker files",
        wu.name
    );
    // Stock batch_collect_assimilator renames each output that exists and skips the rest; a file
    // the server lists as not present is never fetched or written, and the others keep their names.
    let mut seen = BTreeSet::new();
    let mut named: Vec<(String, &OutFile)> = Vec::new();
    for f in files.iter().filter(|f| f.present) {
        let open = f.logical_name()?;
        ensure!(safe_component(&open), "unsafe open_name {open:?}");
        ensure!(safe_component(&f.name), "unsafe file name {:?}", f.name);
        ensure!(seen.insert(open.clone()), "duplicate open_name {open:?}");
        ensure!(
            wu.name.len() + "__file_".len() + open.len() <= MAX_NAME,
            "output name for {open:?} is longer than {MAX_NAME} bytes"
        );
        ensure!(
            f.md5.len() == 32 && f.md5.chars().all(|c| c.is_ascii_hexdigit()),
            "bad md5 for {:?}",
            f.name
        );
        named.push((open, f));
    }
    // The spool root must exist already; every directory made below is synced into its parent
    // before anything is acknowledged.
    make_dir(&cfg.spool, &incoming)?;
    make_dir(&cfg.spool, &cfg.spool.join("results"))?;
    make_dir(&cfg.spool.join("results"), &dir)?;
    if wu.error_mask != 0 {
        let marker = format!("0x{:x}\n", wu.error_mask);
        durable_write(
            &incoming,
            &dir,
            &format!("{}_error", wu.name),
            marker.as_bytes(),
        )?;
        return Ok(());
    }
    // As the stock assimilator: `_no_output_files` when no output file is there to take, whether
    // the template names none or every one is missing.
    if named.is_empty() {
        durable_write(
            &incoming,
            &dir,
            &format!("{}_no_output_files", wu.name),
            b"",
        )?;
    }
    for (open, f) in named {
        let final_name = format!("{}__file_{}", wu.name, open);
        let dest = dir.join(&final_name);
        if verified(&dest, f)? {
            stats.files_skipped += 1;
            continue;
        }
        let part = staging(&incoming, wu.batch, &final_name);
        download(cfg, f, &part).await?;
        fs::rename(&part, &dest).with_context(|| format!("placing {final_name}"))?;
        sync_dir(&dir)?;
        stats.files_written += 1;
    }
    let host = wu.canonical_result.as_ref().map_or(0, |c| c.host_id);
    durable_write(
        &incoming,
        &dir,
        &format!("{}__host_id", wu.name),
        format!("{host}\n").as_bytes(),
    )
}

/// True when `dest` is a regular file with the listed size and MD5.
fn verified(dest: &Path, f: &OutFile) -> anyhow::Result<bool> {
    let Ok(meta) = fs::symlink_metadata(dest) else {
        return Ok(false);
    };
    if !meta.is_file() || meta.len() != f.nbytes {
        return Ok(false);
    }
    let mut h = Md5::new();
    let mut file = File::open(dest)?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()).eq_ignore_ascii_case(&f.md5))
}

/// Downloads into `part`, checks size and MD5, and fsyncs it. On any failure the part file is
/// removed and nothing is placed.
async fn download(cfg: &Config, f: &OutFile, part: &Path) -> anyhow::Result<()> {
    let result = download_inner(cfg, f, part).await;
    if result.is_err() {
        let _ = fs::remove_file(part);
    }
    result
}

async fn download_inner(cfg: &Config, f: &OutFile, part: &Path) -> anyhow::Result<()> {
    let mut resp = cfg
        .client
        .get(cfg.url(&["outputs", &f.name]))
        .bearer_auth(&cfg.token)
        .send()
        .await
        .with_context(|| format!("fetching {}", f.name))?;
    let status = resp.status();
    ensure!(status.is_success(), "fetching {}: HTTP {status}", f.name);
    // The server's own MD5 for the object, when it sends one: it must be the listed one.
    if let Some(v) = resp.headers().get("x-content-md5") {
        let v = v.to_str().unwrap_or("");
        ensure!(
            v.eq_ignore_ascii_case(&f.md5),
            "{}: X-Content-MD5 {v:?} differs from the listed {}",
            f.name,
            f.md5
        );
    }
    let mut out = File::create(part)?;
    let mut h = Md5::new();
    let mut got = 0u64;
    while let Some(chunk) = resp.chunk().await.with_context(|| f.name.clone())? {
        got += chunk.len() as u64;
        ensure!(
            got <= f.nbytes,
            "{} is larger than the listed {} bytes",
            f.name,
            f.nbytes
        );
        h.update(&chunk);
        out.write_all(&chunk)?;
    }
    ensure!(
        got == f.nbytes,
        "{} is {got} bytes, listed {}",
        f.name,
        f.nbytes
    );
    let digest = hex(&h.finalize());
    ensure!(
        digest.eq_ignore_ascii_case(&f.md5),
        "{} md5 {digest} differs from the listed {}",
        f.name,
        f.md5
    );
    out.sync_all()?;
    Ok(())
}

/// Writes `bytes` to `.incoming`, fsyncs, renames to `dir/name`, fsyncs `dir`.
fn durable_write(incoming: &Path, dir: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let batch = dir.file_name().unwrap().to_string_lossy();
    let tmp = incoming.join(format!("{batch}.{}.part", hex(&Md5::digest(name))));
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

/// Creates `dir` (whose parent exists) if missing and syncs it and its parent, so the new
/// entry survives a power loss.
fn make_dir(parent: &Path, dir: &Path) -> anyhow::Result<()> {
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
    }
    sync_dir(dir)?;
    sync_dir(parent)
}

pub(super) fn sync_dir(dir: &Path) -> anyhow::Result<()> {
    // Windows can't open a directory as a file; the spool is on a Linux server.
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path as AxPath, Query, State},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::{get, post},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::{
        collections::{BTreeMap, HashMap},
        sync::{Arc, Mutex},
    };

    #[derive(Default)]
    struct Mock {
        /// Ready workunits, as the API's JSON.
        ready: Vec<Value>,
        outputs: BTreeMap<String, Vec<u8>>,
        /// Output names that answer 500.
        failing: BTreeSet<String>,
        acked: Vec<i64>,
        fetched: Vec<String>,
        bad_auth: usize,
        /// Raw query strings of the list calls.
        queries: Vec<String>,
        /// Ack answers: ids reported already acknowledged / not ready / unknown (nothing is
        /// recorded for them), and whether the reply has only `acknowledged` (older server).
        already: BTreeSet<i64>,
        not_ready: BTreeSet<i64>,
        unknown: BTreeSet<i64>,
        old_ack_shape: bool,
        /// `X-Content-MD5` per output name: true md5 by default, this overrides it, and
        /// `no_md5_header` leaves it off (older server).
        md5_header: BTreeMap<String, String>,
        no_md5_header: bool,
    }
    type Shared = Arc<Mutex<Mock>>;

    fn md5_of(b: &[u8]) -> String {
        hex(&Md5::digest(b))
    }

    fn authed(m: &mut Mock, h: &HeaderMap) -> bool {
        let ok = h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer secret");
        if !ok {
            m.bad_auth += 1;
        }
        ok
    }

    async fn assimilate(
        State(s): State<Shared>,
        h: HeaderMap,
        Query(q): Query<HashMap<String, String>>,
        uri: axum::http::Uri,
    ) -> Response {
        let mut m = s.lock().unwrap();
        if !authed(&mut m, &h) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        m.queries.push(uri.query().unwrap_or("").to_owned());
        if let Some(app) = q.get("app") {
            assert_eq!(app, "usnm_test");
        }
        assert_eq!(q["state"], "ready");
        let after: i64 = q["after"].parse().unwrap();
        let limit: usize = q["limit"].parse().unwrap();
        let rest: Vec<&Value> = m
            .ready
            .iter()
            .filter(|w| {
                w["id"].as_i64().unwrap() > after && !m.acked.contains(&w["id"].as_i64().unwrap())
            })
            .collect();
        let page: Vec<&Value> = rest.iter().take(limit).copied().collect();
        Json(json!({
            "workunits": page,
            "has_more": rest.len() > limit,
            "next_after": page.last().map(|w| w["id"].clone()),
        }))
        .into_response()
    }

    async fn output(
        State(s): State<Shared>,
        h: HeaderMap,
        AxPath(name): AxPath<String>,
    ) -> Response {
        let mut m = s.lock().unwrap();
        if !authed(&mut m, &h) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        m.fetched.push(name.clone());
        if m.failing.contains(&name) {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        match m.outputs.get(&name) {
            Some(b) => {
                let mut headers = HeaderMap::new();
                if !m.no_md5_header {
                    let v = m
                        .md5_header
                        .get(&name)
                        .cloned()
                        .unwrap_or_else(|| md5_of(b));
                    headers.insert("x-content-md5", v.parse().unwrap());
                }
                (headers, b.clone()).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn ack_route(State(s): State<Shared>, h: HeaderMap, Json(b): Json<Value>) -> Response {
        let mut m = s.lock().unwrap();
        if !authed(&mut m, &h) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let ids: Vec<i64> = b["workunit_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        let (mut changed, mut already, mut not_ready, mut unknown) =
            (vec![], vec![], vec![], vec![]);
        for id in ids {
            if m.unknown.contains(&id) {
                unknown.push(id);
            } else if m.not_ready.contains(&id) {
                not_ready.push(id);
            } else if m.already.contains(&id) {
                already.push(id);
            } else {
                m.acked.push(id);
                changed.push(id);
            }
        }
        if m.old_ack_shape {
            return Json(json!({ "acknowledged": changed })).into_response();
        }
        Json(json!({"acknowledged": changed, "already_acked": already,
                    "not_ready": not_ready, "unknown": unknown}))
        .into_response()
    }

    async fn serve(m: Mock) -> (Shared, String) {
        let s: Shared = Arc::new(Mutex::new(m));
        let app = Router::new()
            .route("/admin/v1/assimilate", get(assimilate))
            .route("/admin/v1/assimilate/ack", post(ack_route))
            .route("/admin/v1/outputs/{name}", get(output))
            .with_state(s.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (s, format!("http://{addr}"))
    }

    fn wu_ok(m: &mut Mock, id: i64, batch: i64, files: &[(&str, Option<&str>, &[u8])]) {
        let listed: Vec<Value> = files
            .iter()
            .enumerate()
            .map(|(i, (name, open, body))| {
                m.outputs.insert((*name).into(), body.to_vec());
                json!({"name": name, "open_name": open, "index": i, "present": true,
                       "nbytes": body.len(), "md5": md5_of(body)})
            })
            .collect();
        m.ready.push(json!({
            "id": id, "name": format!("wu_{id}"), "batch": batch, "error_mask": 0,
            "canonical_result": {"id": id * 10, "name": format!("wu_{id}_0"), "host_id": 40 + id,
                                 "user_id": 1, "elapsed_time": 1.0, "granted_credit": 0.5,
                                 "files": listed}
        }));
    }

    fn cfg(base: &str, spool: &Path, ack: bool, page: u32) -> Config {
        Config::new(
            base,
            "secret".into(),
            spool.to_owned(),
            ack,
            vec!["usnm_test".into()],
            page,
        )
        .unwrap()
    }

    fn read(p: PathBuf) -> Vec<u8> {
        fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn incoming_is_empty(spool: &Path) -> bool {
        fs::read_dir(spool.join(".incoming"))
            .unwrap()
            .next()
            .is_none()
    }

    #[tokio::test]
    async fn writes_the_stock_layout_and_acknowledges() {
        let mut m = Mock::default();
        wu_ok(
            &mut m,
            1,
            5,
            &[
                ("r_0", Some("result.json"), b"{}"),
                ("r_1", Some("text.txt"), b"hi"),
            ],
        );
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.workunits, st.files_written, st.acked), (1, 2, 1));
        let d = spool.path().join("results/5");
        assert_eq!(read(d.join("wu_1__file_result.json")), b"{}");
        assert_eq!(read(d.join("wu_1__file_text.txt")), b"hi");
        assert_eq!(read(d.join("wu_1__host_id")), b"41\n");
        assert!(incoming_is_empty(spool.path()));
        assert_eq!(s.lock().unwrap().acked, [1]);
        assert_eq!(s.lock().unwrap().bad_auth, 0);
    }

    #[tokio::test]
    async fn pages_through_has_more() {
        let mut m = Mock::default();
        for id in 1..=5 {
            wu_ok(
                &mut m,
                id,
                1,
                &[(&format!("o{id}"), Some("result.json"), b"x")],
            );
        }
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), false, 2), None)
            .await
            .unwrap();
        assert_eq!(st.workunits, 5);
        for id in 1..=5 {
            assert!(spool
                .path()
                .join(format!("results/1/wu_{id}__file_result.json"))
                .exists());
        }
        assert!(s.lock().unwrap().acked.is_empty());
        let st = pull_once(&cfg(&base, spool.path(), false, 2), Some(3))
            .await
            .unwrap();
        assert_eq!(st.workunits, 3);
    }

    #[tokio::test]
    async fn errored_and_empty_workunits_leave_markers() {
        let mut m = Mock::default();
        m.ready.push(json!({"id": 1, "name": "wu_1", "batch": 2, "error_mask": 34, "canonical_result": null}));
        m.ready
            .push(json!({"id": 2, "name": "wu_2", "batch": 2, "error_mask": 0,
            "canonical_result": {"id": 20, "name": "wu_2_0", "host_id": 7, "files": []}}));
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let d = spool.path().join("results/2");
        fs::create_dir_all(&d).unwrap();
        // A stale marker from an earlier run is replaced, not appended to.
        fs::write(d.join("wu_1_error"), "0x2\n").unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!(st.acked, 2);
        assert_eq!(read(d.join("wu_1_error")), b"0x22\n");
        assert!(d.join("wu_2_no_output_files").exists());
        assert_eq!(read(d.join("wu_2__host_id")), b"7\n");
        assert!(!d.join("wu_2_error").exists());
        assert_eq!(s.lock().unwrap().acked, [1, 2]);
    }

    #[tokio::test]
    async fn an_md5_mismatch_places_nothing_and_acks_nothing() {
        let mut m = Mock::default();
        wu_ok(&mut m, 1, 1, &[("o1", Some("result.json"), b"good")]);
        m.outputs.insert("o1".into(), b"evil".to_vec());
        // No header, so it is the body's MD5 that catches this.
        m.no_md5_header = true;
        wu_ok(&mut m, 2, 1, &[("o2", Some("result.json"), b"fine")]);
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!(st.failed.len(), 1);
        assert!(st.failed[0].contains("md5"), "{:?}", st.failed);
        let d = spool.path().join("results/1");
        assert!(!d.join("wu_1__file_result.json").exists());
        assert!(!d.join("wu_1__host_id").exists());
        assert!(incoming_is_empty(spool.path()));
        assert_eq!(s.lock().unwrap().acked, [2]);
    }

    #[tokio::test]
    async fn a_crash_midway_is_repaired_by_a_rerun() {
        let mut m = Mock::default();
        wu_ok(
            &mut m,
            1,
            1,
            &[
                ("a", Some("result.json"), b"AAA"),
                ("b", Some("text.txt"), b"BBB"),
            ],
        );
        m.failing.insert("b".into());
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.failed.len(), st.acked), (1, 0));
        let d = spool.path().join("results/1");
        assert!(d.join("wu_1__file_result.json").exists());
        assert!(!d.join("wu_1__host_id").exists());
        // A torn staging file and a corrupt placed file are both handled on rerun.
        fs::write(
            staging(&spool.path().join(".incoming"), 1, "wu_1__file_text.txt"),
            "junk",
        )
        .unwrap();
        {
            let mut g = s.lock().unwrap();
            g.failing.clear();
            g.fetched.clear();
        }
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!(
            (
                st.failed.len(),
                st.acked,
                st.files_skipped,
                st.files_written
            ),
            (0, 1, 1, 1)
        );
        assert_eq!(
            s.lock().unwrap().fetched,
            ["b"],
            "the verified file is not fetched again"
        );
        assert_eq!(read(d.join("wu_1__file_text.txt")), b"BBB");
        assert!(d.join("wu_1__host_id").exists());
        assert!(incoming_is_empty(spool.path()));
        // Corrupt a placed file: the next run fetches it again.
        fs::write(d.join("wu_1__file_result.json"), "AAX").unwrap();
        s.lock().unwrap().acked.clear();
        s.lock().unwrap().fetched.clear();
        pull_once(&cfg(&base, spool.path(), false, 100), None)
            .await
            .unwrap();
        assert_eq!(read(d.join("wu_1__file_result.json")), b"AAA");
    }

    #[tokio::test]
    async fn no_ack_never_acknowledges() {
        let mut m = Mock::default();
        wu_ok(&mut m, 1, 1, &[("o", Some("result.json"), b"x")]);
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), false, 100), None)
            .await
            .unwrap();
        assert_eq!((st.workunits, st.acked), (1, 0));
        assert!(s.lock().unwrap().acked.is_empty());
        assert!(spool
            .path()
            .join("results/1/wu_1__file_result.json")
            .exists());
    }

    #[tokio::test]
    async fn unsafe_names_are_refused_before_any_file_is_made() {
        let mut m = Mock::default();
        wu_ok(&mut m, 1, 1, &[("o1", Some("../escape"), b"x")]);
        wu_ok(&mut m, 2, 1, &[("o2", Some("a/b"), b"x")]);
        wu_ok(&mut m, 3, 1, &[("o3", Some(""), b"x")]);
        wu_ok(&mut m, 5, 1, &[("../o5", Some("ok"), b"x")]);
        m.ready.push(json!({"id": 6, "name": "../wu", "batch": 1, "error_mask": 2, "canonical_result": null}));
        m.ready.push(
            json!({"id": 7, "name": "", "batch": 1, "error_mask": 2, "canonical_result": null}),
        );
        m.ready.push(
            json!({"id": 8, "name": "a\\b", "batch": 1, "error_mask": 2, "canonical_result": null}),
        );
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.failed.len(), st.acked), (7, 0));
        assert!(s.lock().unwrap().fetched.is_empty());
        assert!(s.lock().unwrap().acked.is_empty());
        assert!(!spool.path().join("escape").exists());
        assert!(!spool.path().join("results/escape").exists());
        assert!(!spool.path().join("results").exists());
        assert!(!spool.path().join(".incoming").exists());
    }

    /// A server with one ready workunit whose ack reply is the given raw JSON.
    async fn serve_raw_ack(reply: &'static str) -> String {
        let mut m = Mock::default();
        m.ready.push(wu_json(1, vec![]));
        let s: Shared = Arc::new(Mutex::new(m));
        let app = Router::new()
            .route("/admin/v1/assimilate", get(assimilate))
            .route(
                "/admin/v1/assimilate/ack",
                post(move || async move {
                    ([("content-type", "application/json")], reply).into_response()
                }),
            )
            .with_state(s);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn cfg_apps(base: &str, spool: &Path, apps: &[&str]) -> Config {
        Config::new(
            base,
            "secret".into(),
            spool.to_owned(),
            true,
            apps.iter().map(|a| (*a).to_owned()).collect(),
            100,
        )
        .unwrap()
    }

    fn file_json(name: &str, open: Option<&str>, index: usize, body: Option<&[u8]>) -> Value {
        match body {
            Some(b) => json!({"name": name, "open_name": open, "index": index, "present": true,
                              "nbytes": b.len(), "md5": md5_of(b)}),
            None => json!({"name": name, "open_name": open, "index": index, "present": false,
                           "nbytes": 0, "md5": ""}),
        }
    }

    fn wu_json(id: i64, files: Vec<Value>) -> Value {
        json!({"id": id, "name": format!("wu_{id}"), "app": "usnm_test", "batch": 3,
               "error_mask": 0,
               "canonical_result": {"id": id * 10, "name": "r", "host_id": 9, "files": files}})
    }

    #[tokio::test]
    async fn a_file_that_is_not_present_is_neither_fetched_nor_written() {
        let mut m = Mock::default();
        m.outputs.insert("a".into(), b"AAA".to_vec());
        m.ready.push(wu_json(
            1,
            vec![
                file_json("a", Some("result.json"), 0, Some(b"AAA")),
                file_json("b", Some("text.txt"), 1, None),
            ],
        ));
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.failed.len(), st.acked, st.files_written), (0, 1, 1));
        let d = spool.path().join("results/3");
        assert_eq!(read(d.join("wu_1__file_result.json")), b"AAA");
        assert!(!d.join("wu_1__file_text.txt").exists());
        // Some output was taken, so no `_no_output_files`, as batch_collect_assimilator.
        assert!(!d.join("wu_1_no_output_files").exists());
        assert!(d.join("wu_1__host_id").exists());
        assert_eq!(s.lock().unwrap().fetched, ["a"]);
    }

    #[tokio::test]
    async fn a_workunit_whose_files_are_all_missing_leaves_the_no_output_marker() {
        let mut m = Mock::default();
        m.ready.push(wu_json(
            1,
            vec![
                file_json("a", Some("result.json"), 0, None),
                file_json("b", Some("text.txt"), 1, None),
            ],
        ));
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.failed.len(), st.acked), (0, 1));
        let d = spool.path().join("results/3");
        assert!(d.join("wu_1_no_output_files").exists());
        assert!(!d.join("wu_1_error").exists());
        assert_eq!(read(d.join("wu_1__host_id")), b"9\n");
        assert!(s.lock().unwrap().fetched.is_empty());
    }

    #[tokio::test]
    async fn an_older_server_without_present_or_new_headers_still_works() {
        let mut m = Mock::default();
        m.outputs.insert("a".into(), b"AAA".to_vec());
        let mut f = file_json("a", Some("result.json"), 0, Some(b"AAA"));
        f.as_object_mut().unwrap().remove("present");
        m.ready.push(wu_json(1, vec![f]));
        m.old_ack_shape = true;
        m.no_md5_header = true;
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.failed.len(), st.acked, st.already_acked), (0, 1, 0));
        assert_eq!(
            read(spool.path().join("results/3/wu_1__file_result.json")),
            b"AAA"
        );
        assert_eq!(s.lock().unwrap().acked, [1]);
    }

    #[tokio::test]
    async fn no_app_lists_every_app_and_apps_are_repeatable() {
        let mut m = Mock::default();
        m.ready.push(wu_json(1, vec![]));
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        pull_once(&cfg_apps(&base, spool.path(), &[]), None)
            .await
            .unwrap();
        {
            let g = s.lock().unwrap();
            assert_eq!(g.queries.len(), 1);
            assert!(!g.queries[0].contains("app="), "{}", g.queries[0]);
        }
        s.lock().unwrap().queries.clear();
        s.lock().unwrap().acked.clear();
        pull_once(
            &cfg_apps(&base, spool.path(), &["usnm_test", "usnm_test"]),
            None,
        )
        .await
        .unwrap();
        let g = s.lock().unwrap();
        assert_eq!(g.queries.len(), 2);
        assert!(g.queries.iter().all(|q| q.contains("app=usnm_test")));
    }

    #[tokio::test]
    async fn a_file_without_an_open_name_is_named_by_its_index() {
        let mut m = Mock::default();
        m.outputs.insert("a".into(), b"AAA".to_vec());
        m.ready
            .push(wu_json(1, vec![file_json("a", None, 2, Some(b"AAA"))]));
        let (_s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!(st.failed.len(), 0, "{:?}", st.failed);
        assert_eq!(read(spool.path().join("results/3/wu_1__file_2")), b"AAA");
    }

    #[tokio::test]
    async fn the_servers_md5_header_must_match_the_listing() {
        let mut m = Mock::default();
        m.outputs.insert("a".into(), b"AAA".to_vec());
        m.md5_header.insert("a".into(), md5_of(b"other"));
        m.ready.push(wu_json(
            1,
            vec![file_json("a", Some("result.json"), 0, Some(b"AAA"))],
        ));
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.failed.len(), st.acked), (1, 0));
        assert!(st.failed[0].contains("X-Content-MD5"), "{:?}", st.failed);
        assert!(!spool
            .path()
            .join("results/3/wu_1__file_result.json")
            .exists());
        assert!(!spool.path().join("results/3/wu_1__host_id").exists());
        assert!(incoming_is_empty(spool.path()));
        assert!(s.lock().unwrap().acked.is_empty());
    }

    #[tokio::test]
    async fn the_ack_report_is_read() {
        let mut m = Mock::default();
        for id in 1..=4 {
            m.ready.push(wu_json(id, vec![]));
        }
        m.already.insert(2);
        m.not_ready.insert(3);
        m.unknown.insert(4);
        let (_s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.acked, st.already_acked), (1, 1));
        assert_eq!(st.failed.len(), 2, "{:?}", st.failed);
        assert!(st
            .failed
            .iter()
            .any(|f| f.contains("not ready") && f.contains('3')));
        assert!(st
            .failed
            .iter()
            .any(|f| f.contains("does not know") && f.contains('4')));
        // One entry per workunit.
        let mut m = Mock::default();
        for id in 1..=5 {
            m.ready.push(wu_json(id, vec![]));
        }
        m.not_ready.extend([1, 2, 3]);
        m.unknown.extend([4, 5]);
        let (_s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!(st.failed.len(), 5, "{:?}", st.failed);
        for id in 1..=5 {
            let prefix = format!("workunit {id}:");
            assert_eq!(
                st.failed.iter().filter(|f| f.starts_with(&prefix)).count(),
                1
            );
        }
        // A reply without `acknowledged` is refused.
        let base = serve_raw_ack("{}").await;
        let spool = tempfile::tempdir().unwrap();
        let err = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("acknowledged"), "{err:#}");
        // already_acked alone is success.
        let mut m = Mock::default();
        m.ready.push(wu_json(1, vec![]));
        m.already.insert(1);
        let (_s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        assert_eq!((st.acked, st.already_acked, st.failed.len()), (0, 1, 0));
    }

    fn new_cfg(url: &str) -> anyhow::Result<Config> {
        Config::new(
            url,
            "t".into(),
            PathBuf::from("/x"),
            true,
            vec!["a".into()],
            10,
        )
    }

    #[test]
    fn plain_http_is_only_for_loopback() {
        for ok in [
            "https://boinc.example.org",
            "http://127.0.0.1:8080",
            "http://localhost:1",
            "http://[::1]:9",
        ] {
            assert!(new_cfg(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://boinc.example.org",
            "http://203.0.113.5",
            "http://localhost.example.org",
            "ftp://127.0.0.1",
        ] {
            let e = new_cfg(bad).err().unwrap_or_else(|| panic!("{bad}"));
            assert!(e.to_string().contains("https"), "{bad}: {e}");
        }
    }

    #[tokio::test]
    async fn redirects_are_not_followed_with_the_token() {
        let (other, other_base) = serve(Mock::default()).await;
        let target = format!("{other_base}/admin/v1/assimilate");
        let app = Router::new().fallback(move || {
            let t = target.clone();
            async move { (StatusCode::FOUND, [("location", t)]).into_response() }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let spool = tempfile::tempdir().unwrap();
        let err = pull_once(
            &cfg(&format!("http://{addr}"), spool.path(), true, 10),
            None,
        )
        .await
        .expect_err("a redirect is an error");
        assert!(err.to_string().contains("302"), "{err:#}");
        assert_eq!(other.lock().unwrap().bad_auth, 0);
        assert!(other.lock().unwrap().fetched.is_empty());
    }

    #[tokio::test]
    async fn a_missing_spool_root_is_refused_before_any_request() {
        let (s, base) = serve(Mock::default()).await;
        let t = tempfile::tempdir().unwrap();
        let missing = t.path().join("nope/spool");
        let err = pull_once(&cfg(&base, &missing, true, 10), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err:#}");
        assert!(!t.path().join("nope").exists());
        assert!(s.lock().unwrap().acked.is_empty());
    }

    #[tokio::test]
    async fn long_names_are_bounded_or_refused_before_any_write() {
        let mut m = Mock::default();
        // The reviewer's example: 220 + 7 + 20 = 247 fits; a staging name built from it with
        // the batch prefix would not.
        let wu = "w".repeat(220);
        let open = "o".repeat(20);
        m.outputs.insert("f1".into(), b"data".to_vec());
        m.ready.push(json!({"id": 1, "name": wu, "batch": 2147483647, "error_mask": 0,
            "canonical_result": {"id": 10, "name": "r", "host_id": 3, "files": [
                {"name": "f1", "open_name": open, "index": 0, "nbytes": 4, "md5": md5_of(b"data")}]}}));
        m.ready.push(
            json!({"id": 2, "name": "x".repeat(239), "batch": 2147483647,
            "error_mask": 2, "canonical_result": null}),
        );
        m.ready.push(json!({"id": 3, "name": "y".repeat(250), "batch": 1,
            "error_mask": 0, "canonical_result": {"id": 30, "name": "r", "host_id": 3, "files": []}}));
        m.ready.push(json!({"id": 4, "name": "z".repeat(235), "batch": 1,
            "error_mask": 0, "canonical_result": {"id": 40, "name": "r", "host_id": 3, "files": [
                {"name": "f1", "open_name": "o".repeat(20), "index": 0, "nbytes": 4, "md5": md5_of(b"data")}]}}));
        let (s, base) = serve(m).await;
        let spool = tempfile::tempdir().unwrap();
        let st = pull_once(&cfg(&base, spool.path(), true, 100), None)
            .await
            .unwrap();
        // 1 and 2 fit; 3 (marker too long) and 4 (output name too long) are refused.
        assert_eq!((st.failed.len(), st.acked), (2, 2), "{:?}", st.failed);
        assert_eq!(s.lock().unwrap().acked, [1, 2]);
        let d = spool.path().join("results/2147483647");
        assert_eq!(read(d.join(format!("{wu}__file_{open}"))), b"data");
        assert!(d.join(format!("{}_error", "x".repeat(239))).exists());
        assert!(!spool.path().join("results/1").exists());
        assert!(incoming_is_empty(spool.path()));
    }

    #[test]
    fn safe_component_rules() {
        for ok in ["result.json", "wu_1", "a-b.c"] {
            assert!(safe_component(ok), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a\\b", "a..b", "a\0b", "a\nb"] {
            assert!(!safe_component(bad), "{bad:?}");
        }
    }
}
