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
        ensure!(
            matches!(base.scheme(), "http" | "https"),
            "--base-url must be http or https"
        );
        // Keep a trailing slash off so segments append cleanly.
        base.path_segments_mut()
            .map_err(|()| anyhow!("--base-url cannot carry a path"))?
            .pop_if_empty();
        let client = Client::builder()
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
    pub failed: Vec<String>,
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pulled {} workunits ({} files written, {} already present), acknowledged {}, failed {}",
            self.workunits,
            self.files_written,
            self.files_skipped,
            self.acked,
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
    nbytes: u64,
    md5: String,
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
    let mut stats = Stats::default();
    let mut left = limit.unwrap_or(usize::MAX);
    for app in &cfg.apps {
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
                stats.acked += ack(cfg, &done).await?;
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

async fn list(cfg: &Config, app: &str, after: i64, limit: u32) -> anyhow::Result<Page> {
    let resp = cfg
        .client
        .get(cfg.url(&["assimilate"]))
        .query(&[
            ("app", app),
            ("state", "ready"),
            ("after", &after.to_string()),
            ("limit", &limit.to_string()),
        ])
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

async fn ack(cfg: &Config, ids: &[i64]) -> anyhow::Result<usize> {
    #[derive(Deserialize)]
    struct Acked {
        acknowledged: Vec<i64>,
    }
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
    Ok(resp.json::<Acked>().await?.acknowledged.len())
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
    let mut seen = BTreeSet::new();
    for f in files {
        let open = f
            .open_name
            .as_deref()
            .ok_or_else(|| anyhow!("output {:?} has no open_name", f.name))?;
        ensure!(safe_component(open), "unsafe open_name {open:?}");
        ensure!(safe_component(&f.name), "unsafe file name {:?}", f.name);
        ensure!(seen.insert(open), "duplicate open_name {open:?}");
        ensure!(
            f.md5.len() == 32 && f.md5.chars().all(|c| c.is_ascii_hexdigit()),
            "bad md5 for {:?}",
            f.name
        );
    }
    fs::create_dir_all(&incoming)?;
    fs::create_dir_all(&dir)?;
    sync_dir(&cfg.spool)?;
    sync_dir(&cfg.spool.join("results"))?;
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
    if files.is_empty() {
        durable_write(
            &incoming,
            &dir,
            &format!("{}_no_output_files", wu.name),
            b"",
        )?;
        return Ok(());
    }
    for f in files {
        let final_name = format!("{}__file_{}", wu.name, f.open_name.as_deref().unwrap());
        let dest = dir.join(&final_name);
        if verified(&dest, f)? {
            stats.files_skipped += 1;
            continue;
        }
        let part = incoming.join(format!("{}.{}.part", wu.batch, final_name));
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
    let tmp = incoming.join(format!(
        "{}.{name}.part",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> anyhow::Result<()> {
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
    ) -> Response {
        let mut m = s.lock().unwrap();
        if !authed(&mut m, &h) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        assert_eq!(q["app"], "usnm_test");
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
            Some(b) => b.clone().into_response(),
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
        m.acked.extend(&ids);
        Json(json!({ "acknowledged": ids })).into_response()
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
                json!({"name": name, "open_name": open, "index": i,
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
        assert!(!d.join("wu_2_error").exists());
        assert_eq!(s.lock().unwrap().acked, [1, 2]);
    }

    #[tokio::test]
    async fn an_md5_mismatch_places_nothing_and_acks_nothing() {
        let mut m = Mock::default();
        wu_ok(&mut m, 1, 1, &[("o1", Some("result.json"), b"good")]);
        m.outputs.insert("o1".into(), b"evil".to_vec());
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
            spool.path().join(".incoming/1.wu_1__file_text.txt.part"),
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
        wu_ok(&mut m, 4, 1, &[("o4", None, b"x")]);
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
        assert_eq!((st.failed.len(), st.acked), (8, 0));
        assert!(s.lock().unwrap().fetched.is_empty());
        assert!(s.lock().unwrap().acked.is_empty());
        assert!(!spool.path().join("escape").exists());
        assert!(!spool.path().join("results/escape").exists());
        assert!(!spool.path().join("results").exists());
        assert!(!spool.path().join(".incoming").exists());
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
