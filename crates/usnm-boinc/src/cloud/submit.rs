//! `cloud submit`: jobs the controller already exported, to the cloud server's admin API.
//!
//! Only plain first-attempt single-page jobs go: retries, bundled and line jobs, and failed
//! records are skipped. Records are only read (no lock, no save). Each job's two inputs are put
//! under the physical names `submit_unit` stages (`<id>_page`, `<id>_request_json`), then the
//! jobs that uploaded are created in chunks. Safe to repeat: a same-contents upload is
//! idempotent and an existing job name counts as already submitted.
use super::pull::{admin_url, checked_base, http_client};
use crate::{
    dir, load, read, records,
    submit::{limits, max_error_results, schedule},
    Record, Stage,
};
use anyhow::{anyhow, ensure, Context};
use md5::{Digest, Md5};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt, path::Path};
use usnm_ocr_contract::boinc;

const APP: &str = "usnm_ocr";
const INPUT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_ocr_in");
const RESULT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_ocr_out");
/// The server's message for a duplicate workunit name.
const EXISTS: &str = "a job with this name exists";

pub struct Config {
    base: Url,
    token: String,
    batch_size: usize,
    dry_run: bool,
    client: Client,
}

impl Config {
    pub fn new(
        base_url: &str,
        token: String,
        batch_size: u32,
        dry_run: bool,
    ) -> anyhow::Result<Config> {
        Ok(Config {
            base: checked_base(base_url)?,
            token,
            batch_size: batch_size.clamp(1, 500) as usize,
            dry_run,
            client: http_client()?,
        })
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub submitted: usize,
    pub already: usize,
    pub skipped: usize,
    pub failed: usize,
    /// One line per skipped, failed or (dry run) planned job.
    pub lines: Vec<String>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "submitted {}, already there {}, skipped {}, failed {}",
            self.submitted, self.already, self.skipped, self.failed
        )
    }
}

#[derive(Serialize)]
struct InputFile {
    name: String,
}

#[derive(Serialize)]
struct JobSpec {
    name: String,
    input_files: Vec<InputFile>,
    min_quorum: u32,
    target_nresults: u32,
    max_error_results: u32,
    max_total_results: u32,
    max_success_results: u32,
    rsc_fpops_est: f64,
    rsc_fpops_bound: f64,
    rsc_memory_bound: u64,
    rsc_disk_bound: u64,
    delay_bound: u64,
    priority: i64,
}

#[derive(Serialize)]
struct CreateBody<'a> {
    app: &'static str,
    input_template: &'static str,
    result_template: &'static str,
    jobs: &'a [JobSpec],
}

#[derive(Deserialize)]
struct CreateReply {
    jobs: Vec<Created>,
}

#[derive(Deserialize)]
struct Created {
    name: String,
    error: Option<String>,
}

fn spec(record: &Record) -> anyhow::Result<JobSpec> {
    let id = &record.job.id;
    let (delay_bound, priority) = schedule(record.job.message.reason);
    Ok(JobSpec {
        name: record.job.workunit(),
        input_files: [format!("{id}_page"), format!("{id}_request_json")]
            .map(|name| InputFile { name })
            .into(),
        min_quorum: limits::MIN_QUORUM.parse()?,
        target_nresults: limits::TARGET_NRESULTS.parse()?,
        max_error_results: max_error_results(false).parse()?,
        max_total_results: limits::MAX_TOTAL_RESULTS.parse()?,
        max_success_results: limits::MAX_SUCCESS_RESULTS.parse()?,
        rsc_fpops_est: limits::RSC_FPOPS_EST.parse()?,
        rsc_fpops_bound: limits::RSC_FPOPS_BOUND.parse()?,
        rsc_memory_bound: limits::RSC_MEMORY_BOUND.parse()?,
        rsc_disk_bound: limits::RSC_DISK_BOUND.parse()?,
        delay_bound: delay_bound.parse()?,
        priority: priority.parse()?,
    })
}

/// Why this record is not a plain first-attempt single-page job, if so.
fn skip_reason(record: &Record) -> Option<&'static str> {
    if record.retry.is_some() {
        Some("a retry")
    } else if record.bundle.is_some() || record.unbundled.is_some() {
        Some("bundled or unbundled")
    } else if record.job.lines.is_some() {
        Some("a line job")
    } else if record.held.is_some() {
        // A held result is settled without a stage change (trust/gate.rs).
        Some("its result is held")
    } else if !matches!(
        record.stage,
        Stage::Reserved | Stage::Submitting | Stage::Submitted
    ) {
        // `checked_result` takes a result only in these stages: a Prepared record's reservation
        // never finished, and a Published or Failed one is settled.
        Some("not reserved or submitted")
    } else {
        None
    }
}

/// The record's staged inputs as `submit_unit` checks them: (physical name, bytes).
fn inputs(root: &Path, record: &Record) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let id = &record.job.id;
    let input = dir(root, id)?;
    let request = read(&input.join("request.json"), boinc::MAX_REQUEST)?;
    let job: boinc::Job = serde_json::from_slice(&request)?;
    ensure!(job == record.job, "staged request changed");
    let page = read(&input.join("page"), boinc::MAX_SCAN)?;
    ensure!(
        page.len() as u64 == job.scan.bytes && boinc::sha256(&page) == job.scan.sha256,
        "staged scan changed"
    );
    Ok(vec![
        (format!("{id}_page"), page),
        (format!("{id}_request_json"), request),
    ])
}

async fn put(cfg: &Config, name: &str, body: Vec<u8>) -> anyhow::Result<()> {
    let md5: String = Md5::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let resp = cfg
        .client
        .put(admin_url(&cfg.base, &["files", name]))
        .query(&[("md5", md5.as_str()), ("no_delete", "1")])
        .bearer_auth(&cfg.token)
        .body(body)
        .send()
        .await
        .with_context(|| format!("uploading {name}"))?;
    let status = resp.status();
    if status == StatusCode::CONFLICT {
        return Err(anyhow!("the stored file {name} differs from this one"));
    }
    ensure!(status.is_success(), "uploading {name}: HTTP {status}");
    Ok(())
}

async fn create(cfg: &Config, jobs: &[JobSpec]) -> anyhow::Result<CreateReply> {
    let resp = cfg
        .client
        .post(admin_url(&cfg.base, &["jobs"]))
        .bearer_auth(&cfg.token)
        .json(&CreateBody {
            app: APP,
            input_template: INPUT_TEMPLATE,
            result_template: RESULT_TEMPLATE,
            jobs,
        })
        .send()
        .await
        .context("creating jobs")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let text: String = text.chars().take(200).collect();
        return Err(anyhow!("creating jobs: HTTP {status}: {text}"));
    }
    resp.json().await.context("reading the create reply")
}

/// Send `ids` (or every record with `all`).
pub async fn run(cfg: &Config, root: &Path, ids: &[String], all: bool) -> anyhow::Result<Report> {
    let loaded: Vec<(String, anyhow::Result<Record>)> = if all {
        records(root)?
            .into_iter()
            .map(|r| (r.job.id.clone(), Ok(r)))
            .collect()
    } else {
        let mut seen = BTreeSet::new();
        ids.iter()
            .filter(|id| seen.insert(id.as_str()))
            .map(|id| (id.clone(), load(root, id)))
            .collect()
    };
    let mut report = Report::default();
    let mut ready: Vec<JobSpec> = Vec::new();
    for (id, record) in loaded {
        let record = match record {
            Ok(r) => r,
            Err(e) => {
                report.failed += 1;
                report.lines.push(format!("{id}: failed: {e:#}"));
                continue;
            }
        };
        if let Some(why) = skip_reason(&record) {
            report.skipped += 1;
            report.lines.push(format!("{id}: skipped: {why}"));
            continue;
        }
        let uploaded = async {
            let files = inputs(root, &record)?;
            if cfg.dry_run {
                for (name, body) in &files {
                    report
                        .lines
                        .push(format!("{id}: would upload {name} ({} bytes)", body.len()));
                }
            } else {
                for (name, body) in files {
                    put(cfg, &name, body).await?;
                }
            }
            spec(&record)
        }
        .await;
        match uploaded {
            Ok(spec) => ready.push(spec),
            Err(e) => {
                report.failed += 1;
                report.lines.push(format!("{id}: failed: {e:#}"));
            }
        }
    }
    if cfg.dry_run {
        for job in &ready {
            report.lines.push(format!("would create {}", job.name));
        }
        report.submitted = ready.len();
        return Ok(report);
    }
    for chunk in ready.chunks(cfg.batch_size) {
        match create(cfg, chunk).await {
            Err(e) => {
                for job in chunk {
                    report.failed += 1;
                    report.lines.push(format!("{}: failed: {e:#}", job.name));
                }
            }
            Ok(reply) => {
                for job in chunk {
                    match reply.jobs.iter().find(|r| r.name == job.name) {
                        Some(Created { error: None, .. }) => report.submitted += 1,
                        Some(Created { error: Some(e), .. }) if e.contains(EXISTS) => {
                            report.already += 1
                        }
                        Some(Created { error: Some(e), .. }) => {
                            report.failed += 1;
                            report.lines.push(format!("{}: failed: {e}", job.name));
                        }
                        None => {
                            report.failed += 1;
                            report
                                .lines
                                .push(format!("{}: failed: not in the reply", job.name));
                        }
                    }
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{atomic, save};
    use axum::{
        extract::{Path as AxPath, Query, State},
        http::{HeaderMap, StatusCode},
        routing::{post, put as put_route},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::{
        collections::{BTreeMap, HashMap},
        sync::{Arc, Mutex},
    };
    use usnm_ocr_contract::boinc::{Engine, Job, Scan};

    struct Put {
        name: String,
        body: Vec<u8>,
        query: HashMap<String, String>,
        authed: bool,
        content_length: Option<String>,
    }

    #[derive(Default)]
    struct Mock {
        puts: Vec<Put>,
        posts: Vec<Value>,
        post_auth: Vec<bool>,
        /// Names whose upload answers 409.
        conflict: BTreeSet<String>,
        /// Create errors by job name.
        errors: BTreeMap<String, String>,
    }
    type Shared = Arc<Mutex<Mock>>;

    fn authed(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer secret")
    }

    async fn put_file(
        State(s): State<Shared>,
        h: HeaderMap,
        AxPath(name): AxPath<String>,
        Query(q): Query<HashMap<String, String>>,
        body: axum::body::Bytes,
    ) -> StatusCode {
        let mut m = s.lock().unwrap();
        let len = h
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        m.puts.push(Put {
            name: name.clone(),
            body: body.to_vec(),
            query: q,
            authed: authed(&h),
            content_length: len,
        });
        if m.conflict.contains(&name) {
            StatusCode::CONFLICT
        } else {
            StatusCode::OK
        }
    }

    async fn create_jobs(
        State(s): State<Shared>,
        h: HeaderMap,
        Json(b): Json<Value>,
    ) -> Json<Value> {
        let mut m = s.lock().unwrap();
        m.post_auth.push(authed(&h));
        let jobs: Vec<Value> = b["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, j)| {
                let name = j["name"].as_str().unwrap();
                json!({"name": name, "workunit_id": i + 1,
                       "error": m.errors.get(name)})
            })
            .collect();
        m.posts.push(b);
        Json(json!({ "jobs": jobs }))
    }

    async fn serve(m: Mock) -> (Shared, String) {
        let s: Shared = Arc::new(Mutex::new(m));
        let app = Router::new()
            .route("/admin/v1/files/{name}", put_route(put_file))
            .route("/admin/v1/jobs", post(create_jobs))
            .with_state(s.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (s, format!("http://{addr}"))
    }

    fn md5_hex(b: &[u8]) -> String {
        Md5::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    fn record(page: &[u8], seq: u32, mutate: impl FnOnce(&mut Record)) -> Record {
        let mut job = Job {
            retry_of: None,
            lines: None,
            attempt: 0,
            v: boinc::VERSION,
            id: String::new(),
            message: serde_json::from_value(json!({
                "v": 1, "page": format!("sn00000001/1900-01-01/ed-1/seq-{seq}"),
                "batch": "test_pilot", "language": "eng", "reason": "user",
                "queued_at": "2026-10-17T00:00:00Z"
            }))
            .unwrap(),
            batch_version: 1,
            scan: Scan {
                url: "https://tile.loc.gov/test".into(),
                iiif_base: "https://tile.loc.gov/test".into(),
                sha256: boinc::sha256(page),
                bytes: page.len() as u64,
                md5: None,
            },
            engine: Engine {
                kind: Default::default(),
                image: format!("sha256:{}", "a".repeat(64)),
                version: "5.5.0".into(),
                profile: "eng".into(),
                models: "sha256:aaaaaaaaaaaa".into(),
                psm: 3,
                dpi: 300,
                timeout_secs: 60,
            },
        };
        job.id = job.identity().unwrap();
        job.validate().unwrap();
        let mut r = Record {
            terminal: None,
            retry: None,
            job,
            plan: "test".into(),
            stage: Stage::Reserved,
            stamp: "test".into(),
            error: None,
            ledger: None,
            bundle: None,
            unbundled: None,
            held: None,
            origin: None,
        };
        mutate(&mut r);
        r
    }

    /// Write the record and its staged inputs under `root`.
    fn stage(root: &Path, r: &Record, page: &[u8]) {
        let d = root.join("jobs").join(&r.job.id);
        atomic(
            &d.join("request.json"),
            &serde_json::to_vec(&r.job).unwrap(),
        )
        .unwrap();
        atomic(&d.join("page"), page).unwrap();
        save(root, r).unwrap();
    }

    fn cfg(base: &str, batch: u32, dry: bool) -> Config {
        Config::new(base, "secret".into(), batch, dry).unwrap()
    }

    #[tokio::test]
    async fn uploads_then_creates() {
        let root = tempfile::tempdir().unwrap();
        let page = b"page bytes";
        let r = record(page, 1, |_| {});
        stage(root.path(), &r, page);
        let (s, base) = serve(Mock::default()).await;
        let report = run(
            &cfg(&base, 100, false),
            root.path(),
            std::slice::from_ref(&r.job.id),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 1, already there 0, skipped 0, failed 0"
        );
        let m = s.lock().unwrap();
        let id = &r.job.id;
        let names: Vec<&str> = m.puts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, [format!("{id}_page"), format!("{id}_request_json")]);
        assert_eq!(m.puts[0].body, page);
        assert_eq!(m.puts[0].query["md5"], md5_hex(page));
        assert_eq!(m.puts[0].query["no_delete"], "1");
        assert_eq!(m.puts[1].query["md5"], md5_hex(&m.puts[1].body));
        assert!(
            m.puts.iter().all(|p| p.authed),
            "bearer token on every upload"
        );
        assert_eq!(
            m.puts[0].content_length.as_deref(),
            Some(page.len().to_string().as_str())
        );
        assert_eq!(m.post_auth, [true]);
        let b = &m.posts[0];
        assert_eq!(b["app"], "usnm_ocr");
        assert_eq!(b["input_template"], INPUT_TEMPLATE);
        assert_eq!(b["result_template"], RESULT_TEMPLATE);
        assert_eq!(
            b["jobs"][0],
            json!({
                "name": r.job.workunit(),
                "input_files": [{"name": format!("{id}_page")},
                                {"name": format!("{id}_request_json")}],
                "min_quorum": 1, "target_nresults": 1, "max_error_results": 3,
                "max_total_results": 5, "max_success_results": 3,
                "rsc_fpops_est": 1e12, "rsc_fpops_bound": 1e14,
                "rsc_memory_bound": 1610612736u64, "rsc_disk_bound": 1073741824u64,
                "delay_bound": 86400, "priority": 10
            })
        );
    }

    #[tokio::test]
    async fn existing_job_is_already_submitted() {
        let root = tempfile::tempdir().unwrap();
        let page = b"page bytes";
        let r = record(page, 1, |_| {});
        stage(root.path(), &r, page);
        let mut mock = Mock::default();
        mock.errors.insert(
            r.job.workunit(),
            "a job with this name exists already".into(),
        );
        let (_s, base) = serve(mock).await;
        let report = run(
            &cfg(&base, 100, false),
            root.path(),
            std::slice::from_ref(&r.job.id),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 0, already there 1, skipped 0, failed 0"
        );
    }

    #[tokio::test]
    async fn other_create_error_fails_the_job() {
        let root = tempfile::tempdir().unwrap();
        let page = b"page bytes";
        let r = record(page, 1, |_| {});
        stage(root.path(), &r, page);
        let mut mock = Mock::default();
        mock.errors.insert(r.job.workunit(), "boom".into());
        let (_s, base) = serve(mock).await;
        let report = run(
            &cfg(&base, 100, false),
            root.path(),
            std::slice::from_ref(&r.job.id),
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.failed, 1);
        assert!(report.lines[0].contains("boom"));
    }

    #[tokio::test]
    async fn conflict_fails_only_that_job() {
        let root = tempfile::tempdir().unwrap();
        let (p1, p2) = (b"first page".as_slice(), b"second page".as_slice());
        let (a, b) = (record(p1, 1, |_| {}), record(p2, 2, |_| {}));
        stage(root.path(), &a, p1);
        stage(root.path(), &b, p2);
        let mut mock = Mock::default();
        mock.conflict.insert(format!("{}_page", a.job.id));
        let (s, base) = serve(mock).await;
        let report = run(&cfg(&base, 100, false), root.path(), &[], true)
            .await
            .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 1, already there 0, skipped 0, failed 1"
        );
        assert!(report.lines[0].contains("differs"));
        let m = s.lock().unwrap();
        let sent: Vec<&str> = m.posts[0]["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["name"].as_str().unwrap())
            .collect();
        assert_eq!(sent, [b.job.workunit()]);
    }

    #[tokio::test]
    async fn batches_are_chunked() {
        let root = tempfile::tempdir().unwrap();
        let mut ids = vec![];
        for seq in 1..=3u32 {
            let page = format!("page {seq}").into_bytes();
            let r = record(&page, seq, |_| {});
            stage(root.path(), &r, &page);
            ids.push(r.job.id);
        }
        let (s, base) = serve(Mock::default()).await;
        let report = run(&cfg(&base, 2, false), root.path(), &ids, false)
            .await
            .unwrap();
        assert_eq!(report.submitted, 3);
        let sizes: Vec<usize> = s
            .lock()
            .unwrap()
            .posts
            .iter()
            .map(|p| p["jobs"].as_array().unwrap().len())
            .collect();
        assert_eq!(sizes, [2, 1]);
    }

    #[tokio::test]
    async fn skips_failed_and_bundled_and_sends_nothing() {
        let root = tempfile::tempdir().unwrap();
        let page = b"page bytes";
        let failed = record(page, 1, |r| r.stage = Stage::Failed);
        let bundled = record(page, 2, |r| r.bundle = Some("b".repeat(64)));
        let unbundled = record(page, 3, |r| r.unbundled = Some("b".repeat(64)));
        for r in [&failed, &bundled, &unbundled] {
            stage(root.path(), r, page);
        }
        let (s, base) = serve(Mock::default()).await;
        let report = run(&cfg(&base, 100, false), root.path(), &[], true)
            .await
            .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 0, already there 0, skipped 3, failed 0"
        );
        let m = s.lock().unwrap();
        assert!(m.puts.is_empty() && m.posts.is_empty());
    }

    #[test]
    fn skip_reasons_cover_retry_and_lines() {
        let page = b"p";
        let mut r = record(page, 1, |_| {});
        assert_eq!(skip_reason(&r), None);
        r.job.lines = Some(boinc::LineRead {
            language: "eng".into(),
            psm: 7,
            lines: vec![],
        });
        assert_eq!(skip_reason(&r), Some("a line job"));
        let mut r = record(page, 1, |_| {});
        r.retry = Some(crate::retry::Retry {
            previous_job: "a".repeat(64),
            previous_attempt: 0,
            previous_workunit: "wu".into(),
            previous_ledger: crate::LedgerRow {
                batch_version: 1,
                reason: "bulk".into(),
                language: "eng".into(),
                plan: "test".into(),
                attempt: 1,
                request_day: None,
                queued_at: "2026-10-17T00:00:00Z".into(),
                retry_message_at: None,
                retry_run: None,
            },
            terminal: crate::TerminalFailure::from_marker(None),
            previous_error: "e".into(),
            app_version_num: 1,
            reviewed_reason: "r".into(),
            native: None,
            provenance: None,
        });
        assert_eq!(skip_reason(&r), Some("a retry"));
        let held = record(page, 1, |r| r.held = Some("scope".into()));
        assert_eq!(skip_reason(&held), Some("its result is held"));
        for stage in [Stage::Submitting, Stage::Submitted] {
            assert_eq!(skip_reason(&record(page, 1, |r| r.stage = stage)), None);
        }
        for stage in [Stage::Prepared, Stage::Published, Stage::Failed] {
            assert_eq!(
                skip_reason(&record(page, 1, |r| r.stage = stage)),
                Some("not reserved or submitted")
            );
        }
    }

    #[tokio::test]
    async fn dry_run_sends_nothing() {
        let root = tempfile::tempdir().unwrap();
        let page = b"page bytes";
        let r = record(page, 1, |_| {});
        stage(root.path(), &r, page);
        let (s, base) = serve(Mock::default()).await;
        let report = run(&cfg(&base, 100, true), root.path(), &[], true)
            .await
            .unwrap();
        assert_eq!(report.submitted, 1);
        assert!(report.lines.iter().any(|l| l.contains("would upload")));
        let m = s.lock().unwrap();
        assert!(m.puts.is_empty() && m.posts.is_empty());
    }

    #[test]
    fn plain_http_off_loopback_is_refused() {
        assert!(Config::new("http://example.org", "t".into(), 100, false).is_err());
        assert!(Config::new("https://example.org", "t".into(), 100, false).is_ok());
        assert!(Config::new("http://127.0.0.1:1", "t".into(), 100, false).is_ok());
    }
}
