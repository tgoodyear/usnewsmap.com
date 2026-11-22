//! `cloud submit`: jobs the controller already exported, to the cloud server's admin API.
//!
//! Three apps: `usnm_ocr` (the default), `usnm_ndlocr` and `usnm_quality`, whose staged units
//! (`<root>/units/<doc>/`) go the same way: their inputs put, then the jobs created in chunks. A
//! unit already marked `submitted` (or `submitting`) on-prem is skipped, because this command
//! only reads the on-prem state.
//!
//! An input over the admin API's body cap (a quality page is 230 MB to 1 GB) is put straight to
//! the R2 bucket with a single-part SigV4 `PUT` and then registered with
//! `POST /admin/v1/files/<name>/register`, which takes the size and MD5 from the stored object.
//!
//! For `usnm_ocr`, only plain first-attempt single-page jobs go: retries, bundled and line jobs, and failed
//! records are skipped. Records are only read (no lock, no save). Each job's two inputs are put
//! under the physical names `submit_unit` stages (`<id>_page`, `<id>_request_json`), then the
//! jobs that uploaded are created in chunks. Safe to repeat: a same-contents upload is
//! idempotent and an existing job name counts as already submitted.
use super::pull::{admin_url, checked_base, http_client};
use super::sigv4;
use crate::{
    dir, load, ndlocr, quality, read, records,
    submit::{limits, max_error_results, schedule},
    Record, Stage,
};
use anyhow::{anyhow, ensure, Context};
use md5::{Digest, Md5};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};
use usnm_ocr_contract::{boinc, quality::Job as QualityJob};

const INPUT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_ocr_in");
const RESULT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_ocr_out");
const NDLOCR_INPUT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_ndlocr_in");
const NDLOCR_RESULT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_ndlocr_out");
const QUALITY_INPUT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_quality_in");
const QUALITY_QE_INPUT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_quality_qe_in");
const QUALITY_RESULT_TEMPLATE: &str = include_str!("../../../../boinc/apps/usnm_quality_out");
/// The zone plan's request body limit on Free and Pro, which caps one admin upload.
const MAX_UPLOAD: usize = 100 * 1000 * 1000;

/// The app a run submits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum App {
    #[default]
    #[value(name = "usnm_ocr")]
    Ocr,
    #[value(name = "usnm_ndlocr")]
    Ndlocr,
    #[value(name = "usnm_quality")]
    Quality,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Ocr => "usnm_ocr",
            App::Ndlocr => ndlocr::APP,
            App::Quality => quality::APP,
        }
    }
    fn result_template(self) -> &'static str {
        match self {
            App::Ocr => RESULT_TEMPLATE,
            App::Ndlocr => NDLOCR_RESULT_TEMPLATE,
            App::Quality => QUALITY_RESULT_TEMPLATE,
        }
    }
}

/// One unit to send: what `--root` holds for the app.
enum Item {
    Ocr(Box<Record>),
    /// The job and its unit directory.
    Ndlocr(Box<ndlocr::Job>, PathBuf),
    Quality(Box<QualityJob>, PathBuf),
}

impl Item {
    fn skip_reason(&self) -> Option<&'static str> {
        match self {
            Item::Ocr(record) => skip_reason(record),
            Item::Ndlocr(_, unit) | Item::Quality(_, unit) => unit_skip_reason(unit),
        }
    }
    /// The files to put, checked as the on-prem submission checks them. A file over `cap` bytes
    /// is not held in memory: it is read again from its path when it goes up.
    fn inputs(&self, root: &Path, cap: usize) -> anyhow::Result<Vec<Upload>> {
        match self {
            Item::Ocr(record) => inputs(root, record, cap),
            Item::Ndlocr(job, unit) => ndlocr_inputs(job, unit, cap),
            Item::Quality(job, unit) => quality_inputs(job, unit, cap),
        }
    }
    fn spec(&self) -> anyhow::Result<JobSpec> {
        match self {
            Item::Ocr(record) => spec(record),
            Item::Ndlocr(job, _) => ndlocr_spec(job),
            Item::Quality(job, _) => quality_spec(job),
        }
    }
}

/// One input file on its way to the server.
struct Upload {
    name: String,
    len: u64,
    md5: String,
    body: Body,
}

enum Body {
    Bytes(Vec<u8>),
    /// Streamed from disk, for a file over the admin cap.
    File(PathBuf),
}

impl Upload {
    fn of(name: String, bytes: Vec<u8>, path: &Path, cap: usize) -> Upload {
        let md5 = hex(&Md5::digest(&bytes));
        let len = bytes.len() as u64;
        let body = if bytes.len() > cap {
            Body::File(path.to_owned())
        } else {
            Body::Bytes(bytes)
        };
        Upload {
            name,
            len,
            md5,
            body,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// R2's S3 API region.
const R2_REGION: &str = "auto";
/// The most one single-part S3 upload takes.
const MAX_SINGLE_PUT: u64 = 5 * 1000 * 1000 * 1000;
/// The server's message for a duplicate workunit name.
const EXISTS: &str = "a job with this name exists";

pub struct Config {
    base: Url,
    token: String,
    batch_size: usize,
    dry_run: bool,
    client: Client,
    app: App,
    /// The largest body the admin upload takes; a bigger input goes through `r2`.
    max_upload: usize,
    r2: Option<R2>,
}

/// The R2 bucket behind the server's files, for inputs over the admin upload cap.
pub struct R2 {
    endpoint: Url,
    bucket: String,
    credentials: sigv4::Credentials,
    /// No total timeout: a gigabyte takes as long as the link does.
    client: Client,
}

impl R2 {
    /// `endpoint` is the S3 API root (`https://<account>.r2.cloudflarestorage.com`). The
    /// credentials file holds the access key id on its first line and the secret on its second.
    pub fn new(endpoint: &str, bucket: &str, credentials_file: &Path) -> anyhow::Result<R2> {
        let text = std::fs::read_to_string(credentials_file)
            .with_context(|| format!("reading {}", credentials_file.display()))?;
        let mut lines = text.lines().map(str::trim);
        let (Some(id), Some(secret)) = (lines.next(), lines.next()) else {
            anyhow::bail!(
                "the R2 credentials file must hold the access key id on line 1 and the secret on line 2"
            );
        };
        ensure!(
            !id.is_empty() && !secret.is_empty(),
            "the R2 credentials file has an empty access key id or secret"
        );
        ensure!(!bucket.is_empty(), "--r2-bucket is empty");
        let mut endpoint = checked_endpoint(endpoint)?;
        endpoint
            .path_segments_mut()
            .map_err(|()| anyhow!("--r2-endpoint cannot carry a path"))?
            .pop_if_empty();
        ensure!(
            endpoint.path().is_empty() || endpoint.path() == "/",
            "--r2-endpoint must be the server root, without a path"
        );
        Ok(R2 {
            endpoint,
            bucket: bucket.to_owned(),
            credentials: sigv4::Credentials {
                access_key_id: id.to_owned(),
                secret_access_key: secret.to_owned(),
            },
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .build()?,
        })
    }
}

/// https, or plain http for a loopback host only (the signature is the only secret on the wire,
/// but the object is still the campaign's data).
fn checked_endpoint(endpoint: &str) -> anyhow::Result<Url> {
    let url = Url::parse(endpoint).context("--r2-endpoint")?;
    let host = url.host_str().unwrap_or("");
    let loopback = host
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .map_or(host.eq_ignore_ascii_case("localhost"), |a| a.is_loopback());
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "--r2-endpoint must be https (plain http is allowed only for a loopback host)"
    );
    Ok(url)
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
            app: App::default(),
            max_upload: MAX_UPLOAD,
            r2: None,
        })
    }

    /// Put inputs over the admin upload cap straight to this bucket.
    pub fn with_r2(mut self, r2: R2) -> Config {
        self.r2 = Some(r2);
        self
    }

    #[cfg(test)]
    fn with_max_upload(mut self, max: usize) -> Config {
        self.max_upload = max;
        self
    }

    /// Submit this app's units instead of `usnm_ocr`'s.
    pub fn with_app(mut self, app: App) -> Config {
        self.app = app;
        self
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
    /// The input template this job is created with (the request carries it, not each job).
    #[serde(skip)]
    input_template: &'static str,
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
        input_template: INPUT_TEMPLATE,
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

/// Why this ndlocr or quality unit is not sent: the on-prem submit has already taken it.
fn unit_skip_reason(unit: &Path) -> Option<&'static str> {
    if unit.join("submitted").exists() {
        Some("already submitted on-prem (cloud submit only reads the on-prem state)")
    } else if unit.join("submitting").exists() {
        Some("on-prem submission uncertain (submitting marker; cloud submit only reads the on-prem state)")
    } else {
        None
    }
}

/// The unit's staged inputs as `ndlocr::submit` checks them.
fn ndlocr_inputs(job: &ndlocr::Job, unit: &Path, cap: usize) -> anyhow::Result<Vec<Upload>> {
    let id = &job.id;
    let scan = read(&unit.join("page"), boinc::MAX_SCAN)?;
    ensure!(
        scan.len() as u64 == job.scan.bytes && boinc::sha256(&scan) == job.scan.sha256,
        "staged scan changed"
    );
    let meta = read(&unit.join(ndlocr::JOB_FILE), boinc::MAX_JSON)?;
    Ok(vec![
        Upload::of(format!("{id}_page"), scan, &unit.join("page"), cap),
        Upload::of(
            format!("{id}_ndlocr_json"),
            meta,
            &unit.join(ndlocr::JOB_FILE),
            cap,
        ),
    ])
}

/// The unit's staged inputs, checked as `quality::submit` checks them, under the physical names
/// it stages them as (the places and quality-measure files are shared by name).
fn quality_inputs(job: &QualityJob, unit: &Path, cap: usize) -> anyhow::Result<Vec<Upload>> {
    let checked = quality::submit::load_checked(unit)?;
    ensure!(checked.id == job.id, "the staged job changed");
    quality::submit::physical_names(job)
        .into_iter()
        .map(|(name, file)| {
            let path = unit.join(file);
            let bytes = read(&path, quality::submit::limit(file))?;
            Ok(Upload::of(name, bytes, &path, cap))
        })
        .collect()
}

/// The value after `flag` in `create_work`'s arguments.
fn flag_value<'a>(args: &'a [String], flag: &str) -> anyhow::Result<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
        .with_context(|| format!("create_work_args has no {flag}"))
}

/// The job as `quality::submit::create_work_args` gives `create_work`, read from those very
/// arguments so the two cannot drift. The sticky inputs (places, quality measure) are marked in
/// the input template, as on-prem.
fn quality_spec(job: &QualityJob) -> anyhow::Result<JobSpec> {
    let args = quality::submit::create_work_args(job);
    let value = |flag| flag_value(&args, flag);
    Ok(JobSpec {
        input_template: if job.qe.is_some() {
            QUALITY_QE_INPUT_TEMPLATE
        } else {
            QUALITY_INPUT_TEMPLATE
        },
        name: job.workunit(),
        input_files: quality::submit::physical_names(job)
            .into_iter()
            .map(|(name, _)| InputFile { name })
            .collect(),
        min_quorum: value("--min_quorum")?.parse()?,
        target_nresults: value("--target_nresults")?.parse()?,
        max_error_results: value("--max_error_results")?.parse()?,
        max_total_results: value("--max_total_results")?.parse()?,
        max_success_results: value("--max_success_results")?.parse()?,
        rsc_fpops_est: value("--rsc_fpops_est")?.parse()?,
        rsc_fpops_bound: value("--rsc_fpops_bound")?.parse()?,
        rsc_memory_bound: value("--rsc_memory_bound")?.parse()?,
        rsc_disk_bound: value("--rsc_disk_bound")?.parse()?,
        delay_bound: value("--delay_bound")?.parse()?,
        // `create_work` is given no priority, so its default.
        priority: 0,
    })
}

/// The job as `ndlocr::create_work_args` gives `create_work`, from the same constants.
fn ndlocr_spec(job: &ndlocr::Job) -> anyhow::Result<JobSpec> {
    use ndlocr::limits;
    let id = &job.id;
    Ok(JobSpec {
        input_template: NDLOCR_INPUT_TEMPLATE,
        name: job.workunit(),
        input_files: [format!("{id}_page"), format!("{id}_ndlocr_json")]
            .map(|name| InputFile { name })
            .into(),
        min_quorum: limits::MIN_QUORUM.parse()?,
        target_nresults: limits::TARGET_NRESULTS.parse()?,
        max_error_results: limits::MAX_ERROR_RESULTS.parse()?,
        max_total_results: limits::MAX_TOTAL_RESULTS.parse()?,
        max_success_results: limits::MAX_SUCCESS_RESULTS.parse()?,
        rsc_fpops_est: ndlocr::FPOPS_EST.parse()?,
        rsc_fpops_bound: limits::RSC_FPOPS_BOUND.parse()?,
        rsc_memory_bound: ndlocr::MEMORY_BOUND.parse()?,
        rsc_disk_bound: limits::RSC_DISK_BOUND.parse()?,
        delay_bound: limits::DELAY_BOUND.parse()?,
        // `create_work` is given no priority, so its default.
        priority: 0,
    })
}

/// A unit directory name that stays under `<root>/units/`.
fn check_unit_name(doc: &str) -> anyhow::Result<()> {
    ensure!(
        !doc.is_empty()
            && doc != "."
            && doc != ".."
            && doc
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')),
        "not a unit name: {doc:?}"
    );
    Ok(())
}

/// Load one ndlocr or quality unit under `<root>/units/<doc>`. A quality unit's inputs are
/// checked when they are read for upload, not here, so a whole campaign loads fast.
fn load_unit(app: App, root: &Path, doc: &str) -> anyhow::Result<Item> {
    let unit = root.join("units").join(doc);
    if app == App::Quality {
        check_unit_name(doc)?;
        let job: QualityJob = serde_json::from_slice(&read(
            &unit.join(quality::JOB_FILE),
            usnm_ocr_contract::quality::MAX_JOB_JSON,
        )?)?;
        job.validate()?;
        return Ok(Item::Quality(Box::new(job), unit));
    }
    crate::assess::check_doc_id(doc)?;
    let job = ndlocr::load_job(&unit)?;
    ensure!(
        usnm_core::ids::PageKey::from_doc_id(doc)?.to_string() == job.page,
        "unit/page mismatch"
    );
    Ok(Item::Ndlocr(Box::new(job), unit))
}

/// The items to send: `ids`, or everything under `root` with `all`.
fn load_items(
    app: App,
    root: &Path,
    ids: &[String],
    all: bool,
) -> anyhow::Result<Vec<(String, anyhow::Result<Item>)>> {
    Ok(match (app, all) {
        (App::Ocr, true) => records(root)?
            .into_iter()
            .map(|r| (r.job.id.clone(), Ok(Item::Ocr(Box::new(r)))))
            .collect(),
        (App::Ocr, false) => {
            let mut seen = BTreeSet::new();
            ids.iter()
                .filter(|id| seen.insert(id.as_str()))
                .map(|id| (id.clone(), load(root, id).map(|r| Item::Ocr(Box::new(r)))))
                .collect()
        }
        (App::Ndlocr | App::Quality, true) => {
            let dir = root.join("units");
            let mut docs = Vec::new();
            for entry in
                std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
            {
                let entry = entry?;
                // A quality unit is a directory with a job record, as on-prem.
                if entry.file_type()?.is_dir()
                    && (app != App::Quality || entry.path().join(quality::JOB_FILE).exists())
                {
                    docs.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
            docs.sort();
            docs.into_iter()
                .map(|doc| {
                    let item = load_unit(app, root, &doc);
                    (doc, item)
                })
                .collect()
        }
        (App::Ndlocr | App::Quality, false) => {
            let mut seen = BTreeSet::new();
            ids.iter()
                .filter(|id| seen.insert(id.as_str()))
                .map(|id| (id.clone(), load_unit(app, root, id)))
                .collect()
        }
    })
}

/// The record's staged inputs as `submit_unit` checks them: (physical name, bytes).
fn inputs(root: &Path, record: &Record, cap: usize) -> anyhow::Result<Vec<Upload>> {
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
        Upload::of(format!("{id}_page"), page, &input.join("page"), cap),
        Upload::of(
            format!("{id}_request_json"),
            request,
            &input.join("request.json"),
            cap,
        ),
    ])
}

async fn put(cfg: &Config, up: Upload) -> anyhow::Result<()> {
    let name = &up.name;
    let body = match up.body {
        Body::Bytes(bytes) => bytes,
        Body::File(_) => unreachable!("a file over the cap goes through put_large"),
    };
    let resp = cfg
        .client
        .put(admin_url(&cfg.base, &["files", name]))
        .query(&[("md5", up.md5.as_str()), ("no_delete", "1")])
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

#[derive(Deserialize)]
struct Registered {
    nbytes: u64,
    md5: String,
}

/// An input over the admin cap: a single-part `PUT` to `download/<name>` in the bucket (R2 keeps
/// the body's MD5 as the ETag of a single-part object, up to 5 GB), then the register call,
/// which records the stored object's size and MD5 under the name.
async fn put_large(cfg: &Config, r2: &R2, up: &Upload) -> anyhow::Result<()> {
    let Body::File(path) = &up.body else {
        unreachable!("only a file goes through put_large")
    };
    let name = &up.name;
    ensure!(
        up.len <= MAX_SINGLE_PUT,
        "{name} is {} bytes, over the 5 GB a single-part R2 upload takes",
        up.len
    );
    let key = format!("/{}/download/{name}", r2.bucket);
    // The object may be there already (the shared places and quality-measure files are, after the
    // first unit): same contents, nothing to send; other contents, never overwrite.
    match r2_head(r2, &key).await? {
        Some(etag) if etag == up.md5 => {}
        Some(_) => {
            return Err(anyhow!(
            "the stored object for {name} differs from this file (or was not a single-part upload)"
        ))
        }
        None => r2_put(r2, &key, path, up.len)
            .await
            .with_context(|| format!("uploading {name} to the bucket"))?,
    }
    let resp = cfg
        .client
        .post(admin_url(&cfg.base, &["files", name, "register"]))
        .bearer_auth(&cfg.token)
        .json(&serde_json::json!({ "no_delete": true }))
        .send()
        .await
        .with_context(|| format!("registering {name}"))?;
    let status = resp.status();
    ensure!(status.is_success(), "registering {name}: HTTP {status}");
    let reg: Registered = resp
        .json()
        .await
        .with_context(|| format!("reading the register reply for {name}"))?;
    ensure!(
        reg.md5 == up.md5 && reg.nbytes == up.len,
        "the server registered {name} as {} bytes with MD5 {}, not {} bytes with MD5 {}",
        reg.nbytes,
        reg.md5,
        up.len,
        up.md5
    );
    Ok(())
}

/// The signed headers of one request to the bucket. Nothing here is logged.
fn r2_signed(
    r2: &R2,
    method: &str,
    key: &str,
    len: Option<u64>,
) -> anyhow::Result<(Url, Vec<(&'static str, String)>)> {
    let mut url = r2.endpoint.clone();
    url.set_path(key);
    let host = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        _ => anyhow::bail!("--r2-endpoint has no host"),
    };
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let authorization = sigv4::authorization(
        &r2.credentials,
        R2_REGION,
        "s3",
        &amz_date,
        &sigv4::Request {
            method,
            path: key,
            query: &[],
            headers: &[
                ("host", &host),
                ("x-amz-content-sha256", sigv4::UNSIGNED_PAYLOAD),
                ("x-amz-date", &amz_date),
            ],
            payload_hash: sigv4::UNSIGNED_PAYLOAD,
        },
    );
    let mut headers = vec![
        ("x-amz-content-sha256", sigv4::UNSIGNED_PAYLOAD.to_owned()),
        ("x-amz-date", amz_date),
        ("authorization", authorization),
    ];
    if let Some(len) = len {
        headers.push(("content-length", len.to_string()));
    }
    Ok((url, headers))
}

/// The object's MD5, from its ETag, or `None` when there is no such object.
async fn r2_head(r2: &R2, key: &str) -> anyhow::Result<Option<String>> {
    let (url, headers) = r2_signed(r2, "HEAD", key, None)?;
    let mut rq = r2.client.head(url);
    for (k, v) in headers {
        rq = rq.header(k, v);
    }
    let resp = rq.send().await.context("checking the bucket")?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    ensure!(status.is_success(), "checking the bucket: HTTP {status}");
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim_matches('"')
        .to_ascii_lowercase();
    Ok(Some(etag))
}

async fn r2_put(r2: &R2, key: &str, path: &Path, len: u64) -> anyhow::Result<()> {
    let (url, headers) = r2_signed(r2, "PUT", key, Some(len))?;
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut rq = r2.client.put(url).body(file);
    for (k, v) in headers {
        rq = rq.header(k, v);
    }
    let resp = rq.send().await?;
    let status = resp.status();
    ensure!(status.is_success(), "HTTP {status}");
    Ok(())
}

/// Create `jobs`, which share one input template.
async fn create(cfg: &Config, jobs: &[JobSpec]) -> anyhow::Result<CreateReply> {
    let resp = cfg
        .client
        .post(admin_url(&cfg.base, &["jobs"]))
        .bearer_auth(&cfg.token)
        .json(&CreateBody {
            app: cfg.app.name(),
            input_template: jobs.first().map_or("", |j| j.input_template),
            result_template: cfg.app.result_template(),
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

/// Send `ids` (or every record, or for ndlocr every unit, with `all`).
pub async fn run(cfg: &Config, root: &Path, ids: &[String], all: bool) -> anyhow::Result<Report> {
    let loaded = load_items(cfg.app, root, ids, all)?;
    let mut report = Report::default();
    let mut ready: Vec<JobSpec> = Vec::new();
    // Names already in the bucket or on the server from this run: units share some inputs.
    let mut sent: BTreeSet<String> = BTreeSet::new();
    for (id, record) in loaded {
        let item = match record {
            Ok(r) => r,
            Err(e) => {
                report.failed += 1;
                report.lines.push(format!("{id}: failed: {e:#}"));
                continue;
            }
        };
        if let Some(why) = item.skip_reason() {
            report.skipped += 1;
            report.lines.push(format!("{id}: skipped: {why}"));
            continue;
        }
        let uploaded = async {
            let files = item.inputs(root, cfg.max_upload)?;
            for up in &files {
                if matches!(up.body, Body::File(_)) && cfg.r2.is_none() {
                    anyhow::bail!(
                        "{} is {} bytes, over the {}-byte admin upload cap; give --r2-endpoint, \
                         --r2-bucket and --r2-credentials-file to put it in the bucket and \
                         register it (POST /admin/v1/files/{}/register)",
                        up.name,
                        up.len,
                        cfg.max_upload,
                        up.name
                    );
                }
            }
            if cfg.dry_run {
                for up in &files {
                    let how = if matches!(up.body, Body::File(_)) {
                        " to the bucket"
                    } else {
                        ""
                    };
                    report.lines.push(format!(
                        "{id}: would upload {}{how} ({} bytes)",
                        up.name, up.len
                    ));
                }
            } else {
                for up in files {
                    if sent.contains(&up.name) {
                        continue;
                    }
                    let name = up.name.clone();
                    match (&up.body, &cfg.r2) {
                        (Body::File(_), Some(r2)) => put_large(cfg, r2, &up).await?,
                        _ => put(cfg, up).await?,
                    }
                    sent.insert(name);
                }
            }
            item.spec()
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
    // One request has one input template, and a quality job's depends on its quality measure.
    let mut by_template: BTreeMap<&str, Vec<JobSpec>> = BTreeMap::new();
    for job in ready {
        by_template.entry(job.input_template).or_default().push(job);
    }
    for chunk in by_template.values().flat_map(|g| g.chunks(cfg.batch_size)) {
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
        response::IntoResponse,
        routing::{head, post, put as put_route},
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
        /// Objects in the mock bucket, by key (`<bucket>/download/<name>`).
        bucket: BTreeMap<String, Vec<u8>>,
        /// The headers of each bucket PUT or HEAD: (method, authorization, content hash, length).
        bucket_calls: Vec<(String, String, String, Option<String>)>,
        /// Names the register route was called for, with the body.
        registered: Vec<(String, Value)>,
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

    fn bucket_call(m: &mut Mock, method: &str, h: &HeaderMap) {
        let get = |k: &str| {
            h.get(k)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned()
        };
        m.bucket_calls.push((
            method.to_owned(),
            get("authorization"),
            get("x-amz-content-sha256"),
            h.get("content-length")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        ));
    }

    async fn bucket_put(
        State(s): State<Shared>,
        h: HeaderMap,
        AxPath((bucket, name)): AxPath<(String, String)>,
        body: axum::body::Bytes,
    ) -> StatusCode {
        let mut m = s.lock().unwrap();
        bucket_call(&mut m, "PUT", &h);
        m.bucket
            .insert(format!("{bucket}/download/{name}"), body.to_vec());
        StatusCode::OK
    }

    async fn bucket_head(
        State(s): State<Shared>,
        h: HeaderMap,
        AxPath((bucket, name)): AxPath<(String, String)>,
    ) -> axum::response::Response {
        let mut m = s.lock().unwrap();
        bucket_call(&mut m, "HEAD", &h);
        match m.bucket.get(&format!("{bucket}/download/{name}")) {
            Some(b) => (StatusCode::OK, [("etag", format!("\"{}\"", md5_hex(b)))]).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn register(
        State(s): State<Shared>,
        AxPath(name): AxPath<String>,
        Json(b): Json<Value>,
    ) -> axum::response::Response {
        let mut m = s.lock().unwrap();
        m.registered.push((name.clone(), b));
        let key = format!("bucket/download/{name}");
        match m.bucket.get(&key) {
            Some(o) => {
                Json(json!({"name": name, "nbytes": o.len(), "md5": md5_hex(o)})).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn serve(m: Mock) -> (Shared, String) {
        let s: Shared = Arc::new(Mutex::new(m));
        let app = Router::new()
            .route("/admin/v1/files/{name}", put_route(put_file))
            .route("/admin/v1/files/{name}/register", post(register))
            .route(
                "/{bucket}/download/{name}",
                head(bucket_head).put(bucket_put),
            )
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

    /// An ndlocr unit staged under `<root>/units/<doc>/`: (doc id, job).
    fn ndlocr_unit(root: &Path, page: &[u8], mark: Option<&str>) -> (String, ndlocr::Job) {
        let key: usnm_core::ids::PageKey = "sn00000001/1900-01-01/ed-1/seq-1".parse().unwrap();
        let mut job = ndlocr::Job {
            v: 1,
            id: String::new(),
            page: key.to_string(),
            batch: "test_pilot".into(),
            batch_version: 1,
            loc_text: "missing".into(),
            route: usnm_ocr_worker::engine::candidates("jpn").remove(0),
            engine: ndlocr::engine(format!("sha256:{}", "a".repeat(64)), 600),
            scan: Scan {
                url: "https://tile.loc.gov/test".into(),
                iiif_base: "https://tile.loc.gov/test".into(),
                sha256: boinc::sha256(page),
                bytes: page.len() as u64,
                md5: None,
            },
        };
        job.id = job.identity().unwrap();
        job.validate().unwrap();
        let doc = key.doc_id().to_string();
        let unit = root.join("units").join(&doc);
        atomic(&unit.join("page"), page).unwrap();
        atomic(
            &unit.join(ndlocr::JOB_FILE),
            &serde_json::to_vec_pretty(&job).unwrap(),
        )
        .unwrap();
        if let Some(mark) = mark {
            atomic(&unit.join(mark), b"x").unwrap();
        }
        (doc, job)
    }

    fn ndlocr_cfg(base: &str, dry: bool) -> Config {
        cfg(base, 100, dry).with_app(App::Ndlocr)
    }

    #[tokio::test]
    async fn ndlocr_unit_uploads_both_inputs_and_creates_the_job() {
        let root = tempfile::tempdir().unwrap();
        let page = b"jpeg bytes";
        let (doc, job) = ndlocr_unit(root.path(), page, None);
        let (s, base) = serve(Mock::default()).await;
        let report = run(&ndlocr_cfg(&base, false), root.path(), &[doc], false)
            .await
            .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 1, already there 0, skipped 0, failed 0"
        );
        let m = s.lock().unwrap();
        let id = &job.id;
        let names: Vec<&str> = m.puts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, [format!("{id}_page"), format!("{id}_ndlocr_json")]);
        assert_eq!(m.puts[0].body, page);
        assert_eq!(m.puts[0].query["md5"], md5_hex(page));
        assert_eq!(m.puts[1].query["md5"], md5_hex(&m.puts[1].body));
        assert!(m.puts.iter().all(|p| p.authed));
        let b = &m.posts[0];
        assert_eq!(b["app"], "usnm_ndlocr");
        assert_eq!(b["input_template"], NDLOCR_INPUT_TEMPLATE);
        assert_eq!(b["result_template"], NDLOCR_RESULT_TEMPLATE);
        assert_eq!(
            b["jobs"][0],
            json!({
                "name": job.workunit(),
                "input_files": [{"name": format!("{id}_page")},
                                {"name": format!("{id}_ndlocr_json")}],
                "min_quorum": 2, "target_nresults": 2, "max_error_results": 3,
                "max_total_results": 5, "max_success_results": 3,
                "rsc_fpops_est": 1e12, "rsc_fpops_bound": 1e14,
                "rsc_memory_bound": 4294967296u64, "rsc_disk_bound": 1073741824u64,
                "delay_bound": 259200, "priority": 0
            })
        );
    }

    #[tokio::test]
    async fn ndlocr_submitted_unit_is_skipped_and_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let (doc, _) = ndlocr_unit(root.path(), b"jpeg bytes", Some("submitted"));
        let (s, base) = serve(Mock::default()).await;
        let report = run(&ndlocr_cfg(&base, false), root.path(), &[], true)
            .await
            .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 0, already there 0, skipped 1, failed 0"
        );
        assert!(report.lines[0].contains("already submitted on-prem"));
        let m = s.lock().unwrap();
        assert!(m.puts.is_empty() && m.posts.is_empty());
        assert!(root
            .path()
            .join("units")
            .join(doc)
            .join("submitted")
            .exists());
    }

    #[tokio::test]
    async fn ndlocr_dry_run_sends_nothing() {
        let root = tempfile::tempdir().unwrap();
        ndlocr_unit(root.path(), b"jpeg bytes", None);
        let (s, base) = serve(Mock::default()).await;
        let report = run(&ndlocr_cfg(&base, true), root.path(), &[], true)
            .await
            .unwrap();
        assert_eq!(report.submitted, 1);
        assert!(report.lines.iter().any(|l| l.contains("would upload")));
        let m = s.lock().unwrap();
        assert!(m.puts.is_empty() && m.posts.is_empty());
    }

    #[tokio::test]
    async fn an_input_over_the_cap_without_the_bucket_flags_fails_naming_them() {
        let root = tempfile::tempdir().unwrap();
        let (doc, _) = ndlocr_unit(root.path(), &[0u8; 64], None);
        let (s, base) = serve(Mock::default()).await;
        let c = ndlocr_cfg(&base, false).with_max_upload(32);
        let report = run(&c, root.path(), &[doc], false).await.unwrap();
        assert_eq!(report.failed, 1);
        assert!(
            report.lines[0].contains("--r2-endpoint"),
            "{:?}",
            report.lines
        );
        assert!(report.lines[0].contains("/register"));
        let m = s.lock().unwrap();
        assert!(m.puts.is_empty() && m.posts.is_empty());
    }

    /// A quality unit staged under `<root>/units/<doc>/`, with a quality measure when `qe`.
    fn quality_unit(root: &Path, seq: u32, qe: bool) -> QualityJob {
        let doc = format!("sn1_1900-01-01_ed-1_seq-{seq}");
        let dir = root.join("units").join(&doc);
        let mut job = crate::quality::work::tests::unit(&dir);
        job.doc_id = doc;
        if qe {
            atomic(&dir.join(quality::QE_FILE), b"qe resources").unwrap();
            job.qe = Some(usnm_ocr_contract::quality::QeInput {
                input: usnm_ocr_contract::quality::Input::of(b"qe resources", "lake:qe/eng.qe"),
                language: "eng".into(),
            });
        }
        job.id = job.identity().unwrap();
        atomic(
            &dir.join(quality::JOB_FILE),
            &serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        job
    }

    fn quality_cfg(base: &str) -> Config {
        cfg(base, 100, false).with_app(App::Quality)
    }

    #[tokio::test]
    async fn quality_unit_uploads_its_inputs_and_creates_the_job() {
        let root = tempfile::tempdir().unwrap();
        let job = quality_unit(root.path(), 1, false);
        let (s, base) = serve(Mock::default()).await;
        let report = run(
            &quality_cfg(&base),
            root.path(),
            std::slice::from_ref(&job.doc_id),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 1, already there 0, skipped 0, failed 0"
        );
        let m = s.lock().unwrap();
        let names: Vec<String> = quality::submit::physical_names(&job)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names.len(), 5);
        let sent: Vec<&str> = m.puts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(sent, names);
        assert!(m.puts.iter().all(|p| p.authed));
        for p in &m.puts {
            assert_eq!(p.query["md5"], md5_hex(&p.body));
            assert_eq!(p.query["no_delete"], "1");
        }
        let b = &m.posts[0];
        assert_eq!(b["app"], "usnm_quality");
        assert_eq!(b["input_template"], QUALITY_INPUT_TEMPLATE);
        assert_eq!(b["result_template"], QUALITY_RESULT_TEMPLATE);
        let input_files: Vec<Value> = names.iter().map(|n| json!({ "name": n })).collect();
        assert_eq!(
            b["jobs"][0],
            json!({
                "name": job.workunit(),
                "input_files": input_files,
                "min_quorum": 2, "target_nresults": 2, "max_error_results": 3,
                "max_total_results": 4, "max_success_results": 3,
                "rsc_fpops_est": 1e12, "rsc_fpops_bound": 1e14,
                "rsc_memory_bound": 1610612736u64, "rsc_disk_bound": 1073741824u64,
                "delay_bound": 259200, "priority": 0
            })
        );
        assert!(m.bucket_calls.is_empty());
    }

    #[tokio::test]
    async fn quality_units_with_and_without_the_measure_are_created_with_their_own_template() {
        let root = tempfile::tempdir().unwrap();
        let plain = quality_unit(root.path(), 1, false);
        let measured = quality_unit(root.path(), 2, true);
        let (s, base) = serve(Mock::default()).await;
        let report = run(&quality_cfg(&base), root.path(), &[], true)
            .await
            .unwrap();
        assert_eq!(report.submitted, 2, "{:?}", report.lines);
        let m = s.lock().unwrap();
        assert_eq!(m.posts.len(), 2);
        let template_of = |job: &QualityJob| {
            m.posts
                .iter()
                .find(|p| p["jobs"][0]["name"] == job.workunit())
                .unwrap()["input_template"]
                .clone()
        };
        assert_eq!(template_of(&plain), QUALITY_INPUT_TEMPLATE);
        assert_eq!(template_of(&measured), QUALITY_QE_INPUT_TEMPLATE);
        let post = m
            .posts
            .iter()
            .find(|p| p["jobs"][0]["name"] == measured.workunit())
            .unwrap();
        assert_eq!(post["jobs"][0]["input_files"].as_array().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn quality_units_share_one_upload_of_their_places_file() {
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (
            quality_unit(root.path(), 1, false),
            quality_unit(root.path(), 2, false),
        );
        let (s, base) = serve(Mock::default()).await;
        run(&quality_cfg(&base), root.path(), &[], true)
            .await
            .unwrap();
        let m = s.lock().unwrap();
        let places = format!("usnm_quality_places_v2_{}", a.inputs.places.sha256);
        assert_eq!(a.inputs.places.sha256, b.inputs.places.sha256);
        assert_eq!(m.puts.iter().filter(|p| p.name == places).count(), 1);
        assert_eq!(m.puts.len(), 9);
    }

    #[tokio::test]
    async fn quality_unit_marked_on_prem_is_skipped_and_a_changed_input_fails() {
        let root = tempfile::tempdir().unwrap();
        let done = quality_unit(root.path(), 1, false);
        let uncertain = quality_unit(root.path(), 2, false);
        let changed = quality_unit(root.path(), 3, false);
        let unit = |j: &QualityJob| root.path().join("units").join(&j.doc_id);
        atomic(&unit(&done).join("submitted"), b"x").unwrap();
        atomic(&unit(&uncertain).join("submitting"), b"x").unwrap();
        atomic(&unit(&changed).join(quality::TEXTS_FILE), b"{}").unwrap();
        let (s, base) = serve(Mock::default()).await;
        let report = run(&quality_cfg(&base), root.path(), &[], true)
            .await
            .unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 0, already there 0, skipped 2, failed 1"
        );
        assert!(report
            .lines
            .iter()
            .any(|l| l.contains("texts.json changed")));
        let m = s.lock().unwrap();
        assert!(m.puts.is_empty() && m.posts.is_empty());
        assert!(unit(&done).join("submitted").exists());
        assert!(!unit(&changed).join("submitted").exists());
    }

    fn r2_for(base: &str, dir: &Path) -> R2 {
        let creds = dir.join("r2-credentials");
        std::fs::write(&creds, "AKIDEXAMPLE\nsecret-example-value\n").unwrap();
        R2::new(base, "bucket", &creds).unwrap()
    }

    #[tokio::test]
    async fn an_input_over_the_cap_is_put_to_the_bucket_and_registered() {
        let root = tempfile::tempdir().unwrap();
        let job = quality_unit(root.path(), 1, false);
        let page =
            std::fs::read(root.path().join("units").join(&job.doc_id).join("page.jpg")).unwrap();
        // Only the page is over the cap.
        let cap = page.len() - 1;
        let small_max = [&job.inputs.alto, &job.inputs.texts, &job.inputs.places]
            .iter()
            .map(|i| i.bytes as usize)
            .max()
            .unwrap();
        assert!(
            small_max <= cap,
            "the fixture's page must be its largest input"
        );
        let (s, base) = serve(Mock::default()).await;
        let c = quality_cfg(&base)
            .with_max_upload(cap)
            .with_r2(r2_for(&base, root.path()));
        let ids = [job.doc_id.clone()];
        let report = run(&c, root.path(), &ids, false).await.unwrap();
        assert_eq!(
            report.to_string(),
            "submitted 1, already there 0, skipped 0, failed 0",
            "{:?}",
            report.lines
        );
        let page_name = format!("{}_page_jpg", job.id);
        {
            let m = s.lock().unwrap();
            assert_eq!(m.bucket[&format!("bucket/download/{page_name}")], page);
            assert!(m.puts.iter().all(|p| p.name != page_name));
            assert_eq!(m.puts.len(), 4);
            let calls: Vec<&str> = m.bucket_calls.iter().map(|c| c.0.as_str()).collect();
            assert_eq!(calls, ["HEAD", "PUT"]);
            let put = &m.bucket_calls[1];
            assert!(
                put.1
                    .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"),
                "{}",
                put.1
            );
            assert!(put.1.contains("/auto/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="));
            assert!(!put.1.contains("secret-example-value"));
            assert_eq!(put.2, "UNSIGNED-PAYLOAD");
            assert_eq!(put.3.as_deref(), Some(page.len().to_string().as_str()));
            assert_eq!(m.registered.len(), 1);
            assert_eq!(m.registered[0].0, page_name);
            assert_eq!(m.registered[0].1, json!({"no_delete": true}));
            assert_eq!(m.posts.len(), 1);
        }
        // A rerun finds the object there with the same MD5 and sends it no more.
        let c = quality_cfg(&base)
            .with_max_upload(cap)
            .with_r2(r2_for(&base, root.path()));
        let report = run(&c, root.path(), &ids, false).await.unwrap();
        assert_eq!(report.submitted, 1);
        let m = s.lock().unwrap();
        let calls: Vec<&str> = m.bucket_calls.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(calls, ["HEAD", "PUT", "HEAD"]);
        assert_eq!(m.registered.len(), 2);
    }

    #[tokio::test]
    async fn a_stored_object_with_other_contents_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let job = quality_unit(root.path(), 1, false);
        let page_name = format!("{}_page_jpg", job.id);
        let mut mock = Mock::default();
        mock.bucket
            .insert(format!("bucket/download/{page_name}"), b"other".to_vec());
        let (s, base) = serve(mock).await;
        let c = quality_cfg(&base)
            .with_max_upload(1000)
            .with_r2(r2_for(&base, root.path()));
        let report = run(&c, root.path(), std::slice::from_ref(&job.doc_id), false)
            .await
            .unwrap();
        assert_eq!(report.failed, 1);
        assert!(report.lines[0].contains("differs"), "{:?}", report.lines);
        let m = s.lock().unwrap();
        assert_eq!(m.bucket[&format!("bucket/download/{page_name}")], b"other");
        assert!(m.registered.is_empty() && m.posts.is_empty());
    }

    #[tokio::test]
    async fn a_large_input_in_a_dry_run_is_planned_not_sent() {
        let root = tempfile::tempdir().unwrap();
        let job = quality_unit(root.path(), 1, false);
        let (s, base) = serve(Mock::default()).await;
        let c = cfg(&base, 100, true)
            .with_app(App::Quality)
            .with_max_upload(1000)
            .with_r2(r2_for(&base, root.path()));
        let report = run(&c, root.path(), std::slice::from_ref(&job.doc_id), false)
            .await
            .unwrap();
        assert_eq!(report.submitted, 1);
        assert!(report.lines.iter().any(|l| l.contains("to the bucket")));
        let m = s.lock().unwrap();
        assert!(m.puts.is_empty() && m.bucket_calls.is_empty() && m.posts.is_empty());
    }

    #[test]
    fn the_credentials_file_needs_two_lines_and_is_not_echoed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds");
        std::fs::write(&path, "only-the-id-AKID999\n").unwrap();
        let err = format!(
            "{:#}",
            R2::new("https://acct.r2.cloudflarestorage.com", "b", &path)
                .err()
                .unwrap()
        );
        assert!(err.contains("line 2") && !err.contains("AKID999"), "{err}");
        std::fs::write(&path, "id\nsecret\n").unwrap();
        assert!(R2::new("https://acct.r2.cloudflarestorage.com", "b", &path).is_ok());
        assert!(R2::new("http://acct.example.org", "b", &path).is_err());
        assert!(R2::new("https://acct.example.org/x", "b", &path).is_err());
        assert!(R2::new("http://127.0.0.1:9", "b", &path).is_ok());
    }

    #[test]
    fn plain_http_off_loopback_is_refused() {
        assert!(Config::new("http://example.org", "t".into(), 100, false).is_err());
        assert!(Config::new("https://example.org", "t".into(), 100, false).is_ok());
        assert!(Config::new("http://127.0.0.1:1", "t".into(), 100, false).is_ok());
    }
}
