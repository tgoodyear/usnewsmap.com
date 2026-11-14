//! `cloud validatord`: the cloud server's validation webhook for the gold apps. Dev only; nothing
//! in the running bridge or the stock validators calls it. See docs/boinc/server/bridge-steps.md,
//! "Validate gold results for the cloud server".
//!
//! The cloud server POSTs `/v1/validate/<app>` with a JSON body (see cloud-boinc
//! `cloud/docs/architecture.md`, "Webhook call") and `Authorization: Bearer <WEBHOOK_TOKEN>`,
//! and reads back a verdict per result. For `usnm_gold` and `usnm_gold_native` this runs what the
//! stock `script_validator --init_script usnm_gold_check` runs: `gold::validate::check`, the
//! function behind `usnm-boinc gold check --file <result> --expected-dir <gold dir>`. Exit 0 of
//! that command is `valid`; any other exit is the validate error (`invalid`). The apps have no
//! compare script, so every valid result matches every other: one constant fingerprint.
//!
//! `usnm_ndlocr` and `usnm_quality` have an init and a compare script and answer in the server's
//! pairwise `matches` form instead; see `pairwise`.
mod pairwise;
use super::pull::{self, OutFile};
use crate::gold::{job::MAX_RESULT, validate};
use anyhow::{bail, ensure, Context};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path as AxPath, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use pairwise::PAIRWISE_APPS;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// The apps this service validates, all with the gold check.
pub const GOLD_APPS: [&str; 2] = ["usnm_gold", "usnm_gold_native"];
/// Every valid gold result matches every other (the stock apps have no compare script).
const FINGERPRINT: &str = "gold-ok";
const MAX_RESULTS: usize = 64;
const MAX_FILES: usize = 16;

pub struct Config {
    /// The webhook bearer token the cloud server sends (its `WEBHOOK_TOKEN`).
    pub secret: String,
    pub state_dir: PathBuf,
    pub gold_dir: PathBuf,
    pub fetch: pull::Config,
    pub max_body: usize,
    /// How long one call may take; the server's own limit is `<webhook_timeout_secs>`.
    pub call_timeout: Duration,
}

struct Shared {
    cfg: Config,
    /// One lock per call id, so concurrent duplicates run once.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

pub fn router(cfg: Config) -> anyhow::Result<Router> {
    std::fs::create_dir_all(cfg.state_dir.join("calls"))?;
    std::fs::create_dir_all(cfg.state_dir.join("work"))?;
    let max = cfg.max_body;
    let shared = Arc::new(Shared {
        cfg,
        locks: Mutex::new(HashMap::new()),
    });
    Ok(Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/v1/validate/{app}", post(validate_route))
        .layer(DefaultBodyLimit::max(max))
        .with_state(shared))
}

/// Compares in time that does not depend on where the strings differ.
fn token_matches(given: &str, want: &str) -> bool {
    let (a, b) = (
        Sha256::digest(given.as_bytes()),
        Sha256::digest(want.as_bytes()),
    );
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn reply(status: StatusCode, message: &str) -> Response {
    (status, json!({ "error": message }).to_string()).into_response()
}

async fn validate_route(
    State(s): State<Arc<Shared>>,
    AxPath(app): AxPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Authentication first; the body was already bounded by the body limit.
    if !bearer(&headers).is_some_and(|t| token_matches(t, &s.cfg.secret)) {
        return reply(StatusCode::UNAUTHORIZED, "missing or wrong bearer token");
    }
    if !GOLD_APPS.contains(&app.as_str()) && !PAIRWISE_APPS.contains(&app.as_str()) {
        return reply(StatusCode::NOT_FOUND, "no validator for this app");
    }
    let req: Request = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return reply(StatusCode::BAD_REQUEST, &format!("bad request: {e}")),
    };
    if let Err(e) = req.check(&app) {
        return reply(StatusCode::BAD_REQUEST, &format!("{e:#}"));
    }
    let digest: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let lock = {
        let mut m = s.locks.lock().unwrap();
        m.entry(req.call_id.clone()).or_default().clone()
    };
    // One budget for waiting behind a duplicate and for the work, so a call is cut off within
    // `call_timeout` of arriving, not after each part in turn.
    let out = tokio::time::timeout(s.cfg.call_timeout, async {
        let _guard = lock.lock().await;
        once(&s.cfg, &req, &digest).await
    })
    .await;
    {
        let mut m = s.locks.lock().unwrap();
        if Arc::strong_count(&lock) <= 2 {
            m.remove(&req.call_id);
        }
    }
    match out {
        Err(_) => reply(StatusCode::SERVICE_UNAVAILABLE, "the call took too long"),
        Ok(Err(e)) => match e.downcast_ref::<Conflict>() {
            Some(_) => reply(StatusCode::CONFLICT, &format!("{e:#}")),
            None => reply(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
        },
        Ok(Ok(v)) => ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response(),
    }
}

#[derive(Debug)]
struct Conflict;
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this call_id was already answered for a different request")
    }
}
impl std::error::Error for Conflict {}

#[derive(Deserialize)]
struct Request {
    call_id: String,
    /// `check_set` or `check_pair`; only the pairwise apps read it.
    #[serde(default)]
    mode: String,
    #[serde(default)]
    canonical_result_id: Option<i64>,
    workunit: Wu,
    results: Vec<Res>,
}

#[derive(Deserialize)]
struct Wu {
    app: String,
    #[serde(default)]
    min_quorum: i64,
}

#[derive(Deserialize)]
struct Res {
    id: i64,
    #[serde(default)]
    files: Vec<File>,
}

#[derive(Deserialize)]
struct File {
    name: String,
    #[serde(default)]
    md5: String,
    #[serde(default)]
    nbytes: u64,
}

impl Request {
    fn check(&self, app: &str) -> anyhow::Result<()> {
        ensure!(
            !self.call_id.is_empty()
                && self.call_id.len() <= 128
                && self
                    .call_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "call_id must be 1 to 128 of [A-Za-z0-9_-]"
        );
        ensure!(
            self.workunit.app == app,
            "the body's app {:?} is not the path's {app:?}",
            self.workunit.app
        );
        ensure!(
            !self.results.is_empty() && self.results.len() <= MAX_RESULTS,
            "1 to {MAX_RESULTS} results"
        );
        let mut ids = std::collections::BTreeSet::new();
        for r in &self.results {
            ensure!(ids.insert(r.id), "result {} appears twice", r.id);
            ensure!(
                r.files.len() <= MAX_FILES,
                "at most {MAX_FILES} files a result"
            );
        }
        if PAIRWISE_APPS.contains(&app) {
            pairwise::check_request(self)?;
        }
        Ok(())
    }
}

/// Reads a cache entry: `None` only when it does not exist. Any other error (a directory in its
/// place, permissions, I/O) is returned and the entry is left as it is.
fn read_entry(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The reply for a call id. The request's digest is saved under the id, durably, before any
/// work, so the id belongs to that request whether or not a verdict was ever stored: the same
/// request may run again, a different one is a `Conflict`. A verdict is stored only when every
/// result has a final one (no `retry`), and a repeat of that request gets it with no new work.
async fn once(cfg: &Config, req: &Request, digest: &str) -> anyhow::Result<Value> {
    let id: String = Sha256::digest(req.call_id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let calls = cfg.state_dir.join("calls");
    let claim = calls.join(format!("{id}.request"));
    match read_entry(&claim)? {
        Some(saved) if saved == digest.as_bytes() => {}
        Some(_) => return Err(Conflict.into()),
        // `atomic` fsyncs the file and its directory before it returns.
        None => crate::atomic(&claim, digest.as_bytes())?,
    }
    let path = calls.join(format!("{id}.json"));
    if let Some(bytes) = read_entry(&path)? {
        let stored: Value = serde_json::from_slice(&bytes).context("stored verdict")?;
        if stored["request_sha256"] == digest {
            return Ok(stored["reply"].clone());
        }
        return Err(Conflict.into());
    }
    let (reply, final_) = evaluate(cfg, req).await?;
    if final_ {
        crate::atomic(
            &path,
            &serde_json::to_vec(&json!({ "request_sha256": digest, "reply": reply }))?,
        )?;
    }
    Ok(reply)
}

/// A verdict per result. The bool says whether every result has a final verdict.
async fn evaluate(cfg: &Config, req: &Request) -> anyhow::Result<(Value, bool)> {
    if PAIRWISE_APPS.contains(&req.workunit.app.as_str()) {
        return pairwise::evaluate(cfg, req).await;
    }
    let work = tempfile::tempdir_in(cfg.state_dir.join("work"))?;
    let mut out = Vec::new();
    let mut all_final = true;
    for r in &req.results {
        let verdict = result_verdict(cfg, r, work.path()).await;
        if verdict.get("retry").is_some() {
            all_final = false;
        }
        out.push(verdict);
    }
    Ok((json!({ "results": out }), all_final))
}

async fn result_verdict(cfg: &Config, r: &Res, work: &Path) -> Value {
    let ok = json!({ "id": r.id, "fingerprint": FINGERPRINT });
    let invalid = json!({ "id": r.id, "invalid": true });
    let retry = json!({ "id": r.id, "retry": true });
    // `usnm_gold_check` takes exactly one file and exits 2 otherwise; script_validator reads
    // any non-zero exit but 3 as the validate error.
    let [file] = &r.files[..] else {
        return invalid;
    };
    // `check` reads at most MAX_RESULT bytes and fails on a larger file, an invalid result.
    if file.nbytes > MAX_RESULT as u64 {
        return invalid;
    }
    let dir = work.join(r.id.to_string());
    if std::fs::create_dir_all(&dir).is_err() {
        return retry;
    }
    // The file keeps its name: `check` takes the unit from `usnm_gold_<job id>_<replica>_<file>`.
    if file.name.is_empty() || file.name.contains(['/', '\\']) || file.name.contains("..") {
        return invalid;
    }
    let path = dir.join(&file.name);
    let listed = OutFile::listed(&file.name, file.nbytes, &file.md5);
    if pull::download(&cfg.fetch, &listed, &path).await.is_err() {
        // The stock validator never fetches; a fetch that fails says nothing about the result.
        return retry;
    }
    let gold = cfg.gold_dir.clone();
    let verdict = tokio::task::spawn_blocking(move || validate::check(&path, &gold)).await;
    match verdict {
        Ok(Ok(())) => ok,
        Ok(Err(_)) => invalid,
        Err(_) => retry,
    }
}

/// Serves until the process is stopped.
pub async fn serve(cfg: Config, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let app = router(cfg)?;
    axum::serve(listener, app).await?;
    bail!("the server ended")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gold::validate::tests::{report, setup};
    use axum::extract::State as St;
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use usnm_ocr_survey::bundle;

    mod pairwise;

    #[derive(Default)]
    struct Mock {
        outputs: Mutex<BTreeMap<String, Vec<u8>>>,
        failing: Mutex<bool>,
        fetches: AtomicUsize,
        /// Output reads that never finish.
        stall: Mutex<bool>,
        /// What the body is changed to on the way out (an md5 mismatch).
        corrupt: Mutex<bool>,
    }

    async fn output(St(m): St<Arc<Mock>>, AxPath(name): AxPath<String>, h: HeaderMap) -> Response {
        if bearer(&h) != Some("adm") {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        m.fetches.fetch_add(1, Ordering::SeqCst);
        if *m.stall.lock().unwrap() {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        if *m.failing.lock().unwrap() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        match m.outputs.lock().unwrap().get(&name) {
            Some(b) if *m.corrupt.lock().unwrap() => {
                let mut b = b.clone();
                b[0] ^= 1;
                b.into_response()
            }
            Some(b) => b.clone().into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn listen(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    struct Fx {
        gold: tempfile::TempDir,
        state: tempfile::TempDir,
        mock: Arc<Mock>,
        mock_base: String,
        e: crate::gold::validate::Expectations,
        id: String,
    }

    impl Fx {
        async fn new() -> Fx {
            let gold = tempfile::tempdir().unwrap();
            let (e, id) = setup(gold.path());
            let mock = Arc::new(Mock::default());
            let app = Router::new()
                .route("/admin/v1/outputs/{name}", get(output))
                .with_state(mock.clone());
            let mock_base = listen(app).await;
            Fx {
                gold,
                state: tempfile::tempdir().unwrap(),
                mock,
                mock_base,
                e,
                id,
            }
        }

        fn config(&self, max_body: usize) -> Config {
            Config {
                secret: "hook".into(),
                state_dir: self.state.path().to_owned(),
                gold_dir: self.gold.path().to_owned(),
                fetch: pull::Config::new(
                    &self.mock_base,
                    "adm".into(),
                    self.state.path().to_owned(),
                    false,
                    vec![],
                    1,
                )
                .unwrap(),
                max_body,
                call_timeout: Duration::from_secs(10),
            }
        }

        async fn serve(&self) -> String {
            listen(router(self.config(1 << 20)).unwrap()).await
        }

        /// Stores `bytes` as an output named `name`; returns its request entry.
        fn output(&self, name: &str, bytes: &[u8]) -> Value {
            self.mock
                .outputs
                .lock()
                .unwrap()
                .insert(name.into(), bytes.to_vec());
            json!({"name": name, "open_name": "gold-result.json", "index": 0,
                   "md5": md5_hex(bytes), "nbytes": bytes.len()})
        }
    }

    fn md5_hex(b: &[u8]) -> String {
        use md5::{Digest as _, Md5};
        Md5::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    fn body(call_id: &str, app: &str, results: Vec<Value>) -> Value {
        json!({"call_id": call_id, "mode": "check_set",
               "workunit": {"id": 1, "name": "usnm_gold_x", "app": app, "batch": 0, "min_quorum": 1},
               "canonical_result_id": null, "results": results})
    }

    fn res(id: i64, files: Vec<Value>) -> Value {
        json!({"id": id, "name": format!("r{id}"), "host_id": 7, "canonical": false, "files": files})
    }

    async fn post(base: &str, app: &str, token: Option<&str>, body: &Value) -> (u16, Value) {
        let mut rq = reqwest::Client::new()
            .post(format!("{base}/v1/validate/{app}"))
            .json(body);
        if let Some(t) = token {
            rq = rq.bearer_auth(t);
        }
        let r = rq.send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    fn verdict(reply: &Value, id: i64) -> &'static str {
        let v = reply["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap();
        if v["invalid"] == true {
            "invalid"
        } else if v["retry"] == true {
            "retry"
        } else if v["fingerprint"].is_string() {
            "valid"
        } else {
            "none"
        }
    }

    #[tokio::test]
    async fn the_service_agrees_with_the_stock_gold_check_on_every_fixture() {
        let fx = Fx::new().await;
        let good = report(&fx.e, &fx.id);
        let ser = |r: &crate::gold::Report| serde_json::to_vec(r).unwrap();
        let mut cases: Vec<(&str, String, Vec<u8>)> = vec![("good", fx.id.clone(), ser(&good))];
        cases.push(("replayed into another unit", "f".repeat(64), ser(&good)));
        let mut v = serde_json::to_value(&good).unwrap();
        v["crops"][0]["text"] = "page text".into();
        cases.push((
            "extra field",
            fx.id.clone(),
            serde_json::to_vec(&v).unwrap(),
        ));
        let dup =
            String::from_utf8(ser(&good))
                .unwrap()
                .replacen('{', "{\"completed_at\":\"x\",", 1);
        cases.push(("duplicate key", fx.id.clone(), dup.into_bytes()));
        let mut r = good.clone();
        r.crops[0].result.counts.ref_chars += 1;
        cases.push(("wrong transcription", fx.id.clone(), ser(&r)));
        let mut r = good.clone();
        let c = r.crops.pop().unwrap();
        r.summary = bundle::summarize(&[r.crops[0].result.clone()]);
        cases.push(("a crop missing", fx.id.clone(), ser(&r)));
        r.skipped.push(bundle::Skipped {
            crop_id: c.result.crop_id,
            reason: "no-model:deu".into(),
        });
        cases.push(("a crop skipped for a model", fx.id.clone(), ser(&r)));
        let mut r = good.clone();
        r.crops[0].result.counts.char_edits = 0;
        cases.push(("summary edited", fx.id.clone(), ser(&r)));
        let mut r = good.clone();
        r.boinc.as_mut().unwrap().job_id = "f".repeat(64);
        cases.push(("unknown unit", fx.id.clone(), ser(&r)));
        let mut r = good.clone();
        r.engine.version = "5.4.0".into();
        cases.push(("other engine", fx.id.clone(), ser(&r)));
        let mut r = good.clone();
        r.limit = Some(3);
        cases.push(("partial run", fx.id.clone(), ser(&r)));
        cases.push(("not json", fx.id.clone(), b"not json".to_vec()));
        let mut results = Vec::new();
        let mut want = Vec::new();
        for (i, (label, unit, bytes)) in cases.iter().enumerate() {
            let name = format!("usnm_gold_{unit}_{i}_0");
            // The stock path: the library entry behind `usnm-boinc gold check`.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(&name);
            std::fs::write(&path, bytes).unwrap();
            want.push((label, validate::check(&path, fx.gold.path()).is_ok()));
            results.push(res(i as i64 + 1, vec![fx.output(&name, bytes)]));
        }
        let base = fx.serve().await;
        let (status, reply) = post(
            &base,
            "usnm_gold",
            Some("hook"),
            &body("wu1-rev1-check_set", "usnm_gold", results),
        )
        .await;
        assert_eq!(status, 200, "{reply}");
        for (i, (label, ok)) in want.iter().enumerate() {
            let got = verdict(&reply, i as i64 + 1);
            assert_eq!(got, if *ok { "valid" } else { "invalid" }, "{label}");
        }
        assert!(want.iter().any(|(_, ok)| *ok) && want.iter().any(|(_, ok)| !*ok));
    }

    #[tokio::test]
    async fn the_file_count_and_size_rules_match_the_init_script() {
        let fx = Fx::new().await;
        let good = serde_json::to_vec(&report(&fx.e, &fx.id)).unwrap();
        let name = |n: u32| format!("usnm_gold_{}_0_{n}", fx.id);
        let one = fx.output(&name(0), &good);
        let two = fx.output(&name(1), &good);
        // Too large for `check`'s read limit: invalid without a fetch.
        let big_name = name(2);
        let big = json!({"name": big_name, "open_name": null, "index": 0,
                         "md5": "0".repeat(32), "nbytes": MAX_RESULT + 1});
        let base = fx.serve().await;
        let results = vec![
            res(1, vec![]),
            res(2, vec![one.clone(), two]),
            res(3, vec![big]),
            res(4, vec![one]),
        ];
        let (_, reply) = post(
            &base,
            "usnm_gold",
            Some("hook"),
            &body("c1", "usnm_gold", results),
        )
        .await;
        assert_eq!(
            [1, 2, 3, 4].map(|i| verdict(&reply, i)),
            ["invalid", "invalid", "invalid", "valid"]
        );
        // Only result 4's file was fetched.
        assert_eq!(fx.mock.fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn requests_need_the_bearer_token_and_a_known_app() {
        let fx = Fx::new().await;
        let base = fx.serve().await;
        let b = body("c1", "usnm_gold", vec![res(1, vec![])]);
        assert_eq!(post(&base, "usnm_gold", None, &b).await.0, 401);
        assert_eq!(post(&base, "usnm_gold", Some("nope"), &b).await.0, 401);
        assert_eq!(post(&base, "usnm_gold", Some("hook"), &b).await.0, 200);
        assert_eq!(
            post(
                &base,
                "usnm_gold_native",
                Some("hook"),
                &body("c2", "usnm_gold_native", vec![res(1, vec![])])
            )
            .await
            .0,
            200
        );
        assert_eq!(post(&base, "usnm_other", Some("hook"), &b).await.0, 404);
        // The body's app must be the path's.
        assert_eq!(
            post(&base, "usnm_gold_native", Some("hook"), &b).await.0,
            400
        );
        for bad in ["", "a b", &"x".repeat(129)] {
            let b = body(bad, "usnm_gold", vec![res(1, vec![])]);
            assert_eq!(
                post(&base, "usnm_gold", Some("hook"), &b).await.0,
                400,
                "{bad:?}"
            );
        }
        let dup = body("c3", "usnm_gold", vec![res(1, vec![]), res(1, vec![])]);
        assert_eq!(post(&base, "usnm_gold", Some("hook"), &dup).await.0, 400);
        let none = body("c4", "usnm_gold", vec![]);
        assert_eq!(post(&base, "usnm_gold", Some("hook"), &none).await.0, 400);
        let r = reqwest::get(format!("{base}/healthz")).await.unwrap();
        assert_eq!(r.status(), 200);
        let r = reqwest::Client::new()
            .post(format!("{base}/v1/validate/usnm_gold"))
            .bearer_auth("hook")
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused() {
        let fx = Fx::new().await;
        let base = listen(router(fx.config(2048)).unwrap()).await;
        let big = body(
            "c1",
            "usnm_gold",
            vec![res(
                1,
                vec![json!({"name": "x".repeat(4000), "md5": "", "nbytes": 1})],
            )],
        );
        assert_eq!(post(&base, "usnm_gold", Some("hook"), &big).await.0, 413);
    }

    #[tokio::test]
    async fn a_repeated_call_id_gets_the_same_verdict_without_new_work() {
        let fx = Fx::new().await;
        let good = serde_json::to_vec(&report(&fx.e, &fx.id)).unwrap();
        let entry = fx.output(&format!("usnm_gold_{}_0_0", fx.id), &good);
        let base = fx.serve().await;
        let b = body(
            "wu9-rev1-check_set",
            "usnm_gold",
            vec![res(1, vec![entry.clone()])],
        );
        let first = post(&base, "usnm_gold", Some("hook"), &b).await;
        assert_eq!(verdict(&first.1, 1), "valid");
        let fetched = fx.mock.fetches.load(Ordering::SeqCst);
        let again = post(&base, "usnm_gold", Some("hook"), &b).await;
        assert_eq!(again, first);
        assert_eq!(fx.mock.fetches.load(Ordering::SeqCst), fetched);
        // After a restart (a new router on the same state directory) too.
        let base2 = fx.serve().await;
        assert_eq!(post(&base2, "usnm_gold", Some("hook"), &b).await, first);
        assert_eq!(fx.mock.fetches.load(Ordering::SeqCst), fetched);
        // The same id with a different request is refused, not answered.
        let other = body("wu9-rev1-check_set", "usnm_gold", vec![res(2, vec![entry])]);
        assert_eq!(post(&base, "usnm_gold", Some("hook"), &other).await.0, 409);
    }

    #[tokio::test]
    async fn concurrent_duplicates_run_once() {
        let fx = Fx::new().await;
        let good = serde_json::to_vec(&report(&fx.e, &fx.id)).unwrap();
        let entry = fx.output(&format!("usnm_gold_{}_0_0", fx.id), &good);
        let base = fx.serve().await;
        let b = body(
            "wu10-rev3-check_set",
            "usnm_gold",
            vec![res(1, vec![entry])],
        );
        let calls: Vec<_> = (0..8)
            .map(|_| {
                let (base, b) = (base.clone(), b.clone());
                tokio::spawn(async move { post(&base, "usnm_gold", Some("hook"), &b).await })
            })
            .collect();
        let mut replies = Vec::new();
        for c in calls {
            replies.push(c.await.unwrap());
        }
        assert!(
            replies.iter().all(|r| r == &replies[0] && r.0 == 200),
            "{replies:?}"
        );
        assert_eq!(fx.mock.fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_fetch_is_a_retry_and_is_not_remembered() {
        let fx = Fx::new().await;
        let good = serde_json::to_vec(&report(&fx.e, &fx.id)).unwrap();
        let entry = fx.output(&format!("usnm_gold_{}_0_0", fx.id), &good);
        let base = fx.serve().await;
        let b = body(
            "wu11-rev1-check_set",
            "usnm_gold",
            vec![res(1, vec![entry])],
        );
        *fx.mock.failing.lock().unwrap() = true;
        assert_eq!(
            verdict(&post(&base, "usnm_gold", Some("hook"), &b).await.1, 1),
            "retry"
        );
        // The body not matching its MD5 is a fetch failure too.
        *fx.mock.failing.lock().unwrap() = false;
        *fx.mock.corrupt.lock().unwrap() = true;
        assert_eq!(
            verdict(&post(&base, "usnm_gold", Some("hook"), &b).await.1, 1),
            "retry"
        );
        *fx.mock.corrupt.lock().unwrap() = false;
        assert_eq!(
            verdict(&post(&base, "usnm_gold", Some("hook"), &b).await.1, 1),
            "valid"
        );
        let leftovers = std::fs::read_dir(fx.state.path().join("work"))
            .unwrap()
            .count();
        assert_eq!(leftovers, 0, "work directories are removed");
    }

    #[tokio::test]
    async fn a_call_id_belongs_to_its_first_request_even_without_a_verdict() {
        let fx = Fx::new().await;
        let good = serde_json::to_vec(&report(&fx.e, &fx.id)).unwrap();
        let entry = fx.output(&format!("usnm_gold_{}_0_0", fx.id), &good);
        let other_entry = fx.output(&format!("usnm_gold_{}_1_0", fx.id), &good);
        let base = fx.serve().await;
        let a = body(
            "wu12-rev1-check_set",
            "usnm_gold",
            vec![res(1, vec![entry])],
        );
        *fx.mock.failing.lock().unwrap() = true;
        assert_eq!(
            verdict(&post(&base, "usnm_gold", Some("hook"), &a).await.1, 1),
            "retry"
        );
        // A different request under the same id is refused although no verdict was stored.
        let b = body(
            "wu12-rev1-check_set",
            "usnm_gold",
            vec![res(2, vec![other_entry])],
        );
        assert_eq!(post(&base, "usnm_gold", Some("hook"), &b).await.0, 409);
        // The same request is evaluated again.
        let before = fx.mock.fetches.load(Ordering::SeqCst);
        *fx.mock.failing.lock().unwrap() = false;
        assert_eq!(
            verdict(&post(&base, "usnm_gold", Some("hook"), &a).await.1, 1),
            "valid"
        );
        assert!(fx.mock.fetches.load(Ordering::SeqCst) > before);
    }

    #[tokio::test]
    async fn an_unreadable_cache_entry_is_an_error_and_is_left_alone() {
        let fx = Fx::new().await;
        let base = fx.serve().await;
        let b = body("wu13-rev1-check_set", "usnm_gold", vec![res(1, vec![])]);
        let id: String = Sha256::digest(b"wu13-rev1-check_set")
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        // A directory where the entry should be: reading it fails with something but NotFound.
        let entry = fx.state.path().join("calls").join(format!("{id}.request"));
        std::fs::create_dir_all(entry.join("keep")).unwrap();
        assert_eq!(post(&base, "usnm_gold", Some("hook"), &b).await.0, 500);
        assert!(entry.join("keep").is_dir());
    }

    #[tokio::test(start_paused = true)]
    async fn one_timeout_covers_waiting_for_a_duplicate_and_the_work() {
        let fx = Fx::new().await;
        let good = serde_json::to_vec(&report(&fx.e, &fx.id)).unwrap();
        let entry = fx.output(&format!("usnm_gold_{}_0_0", fx.id), &good);
        *fx.mock.stall.lock().unwrap() = true;
        let base = fx.serve().await;
        let b = body(
            "wu14-rev1-check_set",
            "usnm_gold",
            vec![res(1, vec![entry])],
        );
        let start = tokio::time::Instant::now();
        let calls: Vec<_> = (0..2)
            .map(|_| {
                let (base, b) = (base.clone(), b.clone());
                tokio::spawn(async move { post(&base, "usnm_gold", Some("hook"), &b).await })
            })
            .collect();
        for c in calls {
            assert_eq!(c.await.unwrap().0, 503);
        }
        // Budget 10 s. The second call waiting its turn and then running for its own 10 s would
        // take about 20 s.
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "{:?}",
            start.elapsed()
        );
    }
}
