//! The remote hand-off between the OCR node and the BOINC server (#469), without a shared
//! filesystem, `az containerapp exec` or public access to the project share.
//!
//! Out (node to server): `vars` prints a reserved job's `request.json` and `record.json` as
//! compact JSON in base64, small enough to be bridge-job settings. The server's
//! `submit-remote` step decodes them (`decode`), fetches the page image again from the LoC
//! URL in the request, checks it against the request's size and sha256, writes the job
//! directory into its own hand-off root (`install`) and runs the usual `submit`. No page
//! bytes travel in the settings.
//!
//! Back (server to node): the `spool-out` step prints the job's spool files (and, as a
//! separate payload, the gold `trust.json`) to its console as framed, chunked base64 lines
//! (`emit`). The node reads the execution's console log from Log Analytics, and `absorb`
//! reassembles the payloads, checks their sha256 and rebuilds the spool directory and
//! `trust.json`, so `usnm-boinc collect --trust ... --host-id N` runs on the node.
use crate::{atomic, dir, load, read, save, Lock, Record, Stage};
use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read as _, Write},
    path::Path,
};
use usnm_ocr_contract::boinc::{self, Job};

/// The setting that carries the request.
pub const REQUEST_VAR: &str = "HANDOFF_REQUEST";
/// The setting that carries the journal record.
pub const RECORD_VAR: &str = "HANDOFF_RECORD";
/// The longest value `vars` prints for one setting. A request is about 1 KiB in base64
/// (980 characters for the test fixture) and a record a little more (1,104); this
/// leaves room for long URLs while keeping the whole job template far below the Container
/// Apps limits.
pub const MAX_VAR: usize = 8 * 1024;
/// The line marker of the console channel.
pub const MARK: &str = "USNM-HANDOFF";
/// Base64 characters per console line, well under the 16 KiB at which container runtimes
/// split a log line.
pub const CHUNK: usize = 3000;
/// The most encoded bytes one payload may print (a gzipped spool with the largest allowed
/// ALTO fits; Log Analytics query answers stop at 64 MB).
pub const MAX_ENCODED: usize = 32 << 20;
/// The most a payload may inflate to: the three result files at their limits, and slack.
const MAX_INFLATED: u64 = (boinc::MAX_ALTO + boinc::MAX_TEXT + 4 * boinc::MAX_JSON) as u64;
const MAX_TRUST: usize = 16 << 20;
const MAX_MARKER: usize = 64 * 1024;

/// The bridge-job settings for one reserved job: `JOB_ID`, the request and the record.
pub fn vars(root: &Path, id: &str) -> anyhow::Result<Vec<(String, String)>> {
    let record = load(root, id)?;
    ensure!(
        record.stage == Stage::Reserved,
        "job is {:?}, not reserved; only a reserved job can be handed to the server",
        record.stage
    );
    let request: Job = serde_json::from_slice(&read(
        &dir(root, id)?.join("request.json"),
        boinc::MAX_JSON,
    )?)?;
    ensure!(request == record.job, "staged request changed");
    let mut out = vec![("JOB_ID".to_string(), id.to_string())];
    for (name, json) in [
        (REQUEST_VAR, serde_json::to_vec(&request)?),
        (RECORD_VAR, serde_json::to_vec(&record)?),
    ] {
        let value = STANDARD.encode(json);
        ensure!(
            value.len() <= MAX_VAR,
            "{name} would be {} characters, over {MAX_VAR}",
            value.len()
        );
        out.push((name.to_string(), value));
    }
    Ok(out)
}

/// A job as the server receives it, checked against its id.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub request: Job,
    pub record: Record,
}

/// Decodes and checks the two settings: the record must be a reserved journal entry for this
/// very request, and the request's identity must be `id`.
pub fn decode(id: &str, request: &str, record: &str) -> anyhow::Result<Incoming> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    let parse = |name: &str, value: &str| -> anyhow::Result<Vec<u8>> {
        ensure!(
            value.len() <= MAX_VAR,
            "{name} is over {MAX_VAR} characters"
        );
        STANDARD
            .decode(value.trim())
            .with_context(|| format!("{name} is not base64"))
    };
    let request: Job = serde_json::from_slice(&parse(REQUEST_VAR, request)?)
        .with_context(|| format!("{REQUEST_VAR} is not a request"))?;
    let record: Record = serde_json::from_slice(&parse(RECORD_VAR, record)?)
        .with_context(|| format!("{RECORD_VAR} is not a journal record"))?;
    request.validate()?;
    ensure!(
        request.id == id && request.identity()? == id,
        "request is not job {id}"
    );
    ensure!(record.job == request, "record is for another request");
    ensure!(record.stage == Stage::Reserved, "record is not reserved");
    Ok(Incoming { request, record })
}

/// What `install` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The job directory is in place and reserved: submit it.
    Reserved,
    /// The server already had this job, past reservation: leave it.
    Already(Stage),
}

/// Writes the job directory under `root` (the server's hand-off root) from the decoded
/// settings and the re-fetched page. A job the server already has must be the same job; one
/// past reservation is left as it is.
pub fn install(root: &Path, job: &Incoming, scan: &[u8]) -> anyhow::Result<Installed> {
    let request = &job.request;
    ensure!(
        scan.len() as u64 == request.scan.bytes && boinc::sha256(scan) == request.scan.sha256,
        "the page fetched from {} is not the one the node exported (size or sha256 differ)",
        request.scan.url
    );
    let _lock = Lock::take(root)?;
    let d = dir(root, &request.id)?;
    if d.join("record.json").exists() {
        let old = load(root, &request.id)?;
        ensure!(
            old.job == *request && old.plan == job.record.plan && old.stamp == job.record.stamp,
            "the server's journal holds a different entry for this job"
        );
        if old.stage != Stage::Reserved {
            return Ok(Installed::Already(old.stage));
        }
    }
    atomic(&d.join("page"), scan)?;
    atomic(
        &d.join("request.json"),
        &serde_json::to_vec_pretty(request)?,
    )?;
    let mut record = job.record.clone();
    record.error = None;
    save(root, &record)?;
    Ok(Installed::Reserved)
}

/// What a payload carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One job's spool files, named as the assimilator names them.
    Spool,
    /// The gold check's `trust.json`.
    Trust,
}

/// One console payload, before compression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub v: u32,
    pub kind: Kind,
    /// The job, for a spool payload.
    pub job: Option<String>,
    /// The canonical result's host id, when the server could read it.
    pub host_id: Option<u64>,
    /// File name to its bytes in base64.
    pub files: BTreeMap<String, String>,
}

/// The spool names one job can have, with their size limits.
fn spool_names(workunit: &str) -> [(String, usize); 5] {
    [
        (format!("{workunit}__file_result.json"), boinc::MAX_JSON),
        (format!("{workunit}__file_text.txt"), boinc::MAX_TEXT),
        (format!("{workunit}__file_layout.xml"), boinc::MAX_ALTO),
        (format!("{workunit}_error"), MAX_MARKER),
        (format!("{workunit}_no_output_files"), MAX_MARKER),
    ]
}

/// The spool files of job `id` (from the server's journal at `root`) under `spool`.
pub fn spool_payload(
    root: &Path,
    spool: &Path,
    id: &str,
    host_id: Option<u64>,
) -> anyhow::Result<Payload> {
    let workunit = load(root, id)?.job.workunit();
    let mut files = BTreeMap::new();
    for (name, max) in spool_names(&workunit) {
        let path = spool.join(&name);
        if path.exists() {
            files.insert(
                name,
                STANDARD.encode(read(&path, max).with_context(|| path.display().to_string())?),
            );
        }
    }
    ensure!(
        !files.is_empty(),
        "no spool files for {workunit} under {} (not assimilated yet?)",
        spool.display()
    );
    Ok(Payload {
        v: 1,
        kind: Kind::Spool,
        job: Some(id.into()),
        host_id,
        files,
    })
}

/// `trust.json`, checked to be one.
pub fn trust_payload(path: &Path) -> anyhow::Result<Payload> {
    crate::trust::Trust::load(path)?;
    Ok(Payload {
        v: 1,
        kind: Kind::Trust,
        job: None,
        host_id: None,
        files: BTreeMap::from([("trust.json".into(), STANDARD.encode(read(path, MAX_TRUST)?))]),
    })
}

/// Prints `payload` as console lines:
///
/// ```text
/// USNM-HANDOFF v1 begin <sha256 of the gzipped payload> <chunks>
/// USNM-HANDOFF v1 part <sha256> <index from 0> <base64>
/// ```
pub fn emit(payload: &Payload, out: &mut impl Write) -> anyhow::Result<()> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&serde_json::to_vec(payload)?)?;
    let encoded = STANDARD.encode(gz.finish()?);
    ensure!(
        encoded.len() <= MAX_ENCODED,
        "payload is {} encoded bytes, over {MAX_ENCODED}",
        encoded.len()
    );
    let sha = boinc::sha256(encoded.as_bytes());
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(CHUNK)
        .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
        .collect();
    writeln!(out, "{MARK} v1 begin {sha} {}", chunks.len())?;
    for (i, c) in chunks.iter().enumerate() {
        writeln!(out, "{MARK} v1 part {sha} {i} {c}")?;
    }
    out.flush()?;
    Ok(())
}

/// Every complete payload in `log` (console lines in any order, with any prefix before the
/// marker, repeated lines allowed). A payload whose begin line is there but whose parts are
/// not all there, or disagree, is an error.
pub fn absorb(log: &str) -> anyhow::Result<Vec<Payload>> {
    let mut begins: BTreeMap<String, usize> = BTreeMap::new();
    let mut parts: BTreeMap<String, BTreeMap<usize, String>> = BTreeMap::new();
    for line in log.lines() {
        let Some(at) = line.find(MARK) else { continue };
        let f: Vec<&str> = line[at..].split_whitespace().collect();
        match f.as_slice() {
            [_, "v1", "begin", sha, n] if boinc::is_sha256(sha) => {
                let n: usize = n.parse().context("bad chunk count")?;
                ensure!(n > 0 && n <= MAX_ENCODED.div_ceil(CHUNK), "bad chunk count");
                if let Some(old) = begins.insert(sha.to_string(), n) {
                    ensure!(old == n, "payload {sha} has two chunk counts");
                }
            }
            [_, "v1", "part", sha, i, data] if boinc::is_sha256(sha) => {
                let i: usize = i.parse().context("bad chunk index")?;
                let slot = parts.entry(sha.to_string()).or_default();
                if let Some(old) = slot.insert(i, data.to_string()) {
                    ensure!(
                        old == *data,
                        "payload {sha} chunk {i} differs between lines"
                    );
                }
            }
            _ => bail!("unreadable {MARK} line"),
        }
    }
    let mut out = Vec::new();
    for (sha, n) in begins {
        let got = parts.remove(&sha).unwrap_or_default();
        ensure!(
            got.len() == n && got.keys().copied().eq(0..n),
            "payload {sha}: {} of {n} chunks in the log (ingestion can lag a few minutes; read it again)",
            got.len()
        );
        let encoded: String = got.into_values().collect();
        ensure!(
            boinc::sha256(encoded.as_bytes()) == sha,
            "payload {sha}: checksum mismatch"
        );
        let gz = STANDARD.decode(encoded)?;
        let mut json = Vec::new();
        flate2::read::GzDecoder::new(&gz[..])
            .take(MAX_INFLATED + 1)
            .read_to_end(&mut json)?;
        ensure!(
            json.len() as u64 <= MAX_INFLATED,
            "payload {sha} is too large"
        );
        let p: Payload = serde_json::from_slice(&json)?;
        ensure!(p.v == 1, "payload {sha} has version {}", p.v);
        out.push(p);
    }
    ensure!(parts.is_empty(), "chunks with no begin line in the log");
    Ok(out)
}

/// Writes a spool payload for job `id` into `spool`. Only the job's own spool names are
/// accepted.
pub fn write_spool(payload: &Payload, id: &str, spool: &Path) -> anyhow::Result<usize> {
    ensure!(
        payload.kind == Kind::Spool && payload.job.as_deref() == Some(id),
        "not the spool of job {id}"
    );
    ensure!(boinc::is_sha256(id), "invalid job id");
    let names = spool_names(&format!("usnm_{id}"));
    for (name, data) in &payload.files {
        let max = names
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, m)| *m)
            .with_context(|| format!("unexpected spool file name {name:?}"))?;
        let bytes = STANDARD.decode(data)?;
        ensure!(bytes.len() <= max, "{name} is over its size limit");
        atomic(&spool.join(name), &bytes)?;
    }
    Ok(payload.files.len())
}

/// Writes a trust payload to `out`, after checking it parses as `trust.json`.
pub fn write_trust(payload: &Payload, out: &Path) -> anyhow::Result<()> {
    ensure!(payload.kind == Kind::Trust, "not a trust payload");
    ensure!(payload.files.len() == 1, "a trust payload holds one file");
    let bytes = STANDARD.decode(payload.files.get("trust.json").context("no trust.json")?)?;
    ensure!(bytes.len() <= MAX_TRUST, "trust.json is too large");
    let _: crate::trust::Trust = serde_json::from_slice(&bytes)?;
    atomic(out, &bytes)
}
