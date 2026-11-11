//! Host onboarding: one gold unit outstanding per untrusted host and campaign.
use super::{
    attempt_root, campaign_roots, capability, query, submitted_attempts, Config, MAX_ATTEMPTS,
};
use crate::{atomic, gold, read, trust::Trust, Lock};
use anyhow::{ensure, Context};
use serde::Serialize;
use std::path::{Path, PathBuf};
use usnm_ocr_contract::boinc;

/// One BOINC result of a failed gold attempt, kept with the attempt as its diagnostics.
#[derive(Debug, PartialEq, Serialize)]
struct FailedResult {
    result: u64,
    outcome: i64,
    client_state: i64,
    exit_status: i64,
    /// The client's uploaded stderr (BOINC `result.stderr_out`), first 64 KiB.
    stderr: String,
}

#[derive(Debug, PartialEq)]
enum GoldStatus {
    /// The workunit row is gone (purged) or never reached the database; leave it alone.
    Unknown,
    Pending,
    Succeeded,
    Failed(Vec<FailedResult>),
}

const SERVER_STATE_OVER: i64 = 5;
const OUTCOME_SUCCESS: i64 = 1;

fn decode_hex(text: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(
        text.is_ascii() && text.len().is_multiple_of(2),
        "invalid hex"
    );
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).context("invalid hex"))
        .collect()
}

/// Rows of `id, server_state, outcome, client_state, exit_status, HEX(stderr)` from a
/// `workunit LEFT JOIN result` query: no rows means no workunit; a NULL id means no result yet.
fn gold_status(text: &str) -> anyhow::Result<GoldStatus> {
    let mut failed = Vec::new();
    let mut any = false;
    let mut pending = false;
    let mut succeeded = false;
    for line in text.lines() {
        any = true;
        let f: Vec<&str> = line.split('\t').collect();
        ensure!(f.len() == 6, "invalid gold status row");
        if f[0] == "NULL" {
            pending = true;
            continue;
        }
        let n = |s: &str| s.parse::<i64>().context("invalid gold status field");
        let (server_state, outcome) = (n(f[1])?, n(f[2])?);
        if server_state != SERVER_STATE_OVER {
            pending = true;
        } else if outcome == OUTCOME_SUCCESS {
            succeeded = true;
        } else {
            let stderr = if f[5] == "NULL" {
                Vec::new()
            } else {
                decode_hex(f[5])?
            };
            failed.push(FailedResult {
                result: f[0].parse().context("invalid result id")?,
                outcome,
                client_state: n(f[3])?,
                exit_status: n(f[4])?,
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            });
        }
    }
    Ok(match (any, pending, succeeded) {
        (false, ..) => GoldStatus::Unknown,
        (_, true, _) => GoldStatus::Pending,
        (_, _, true) => GoldStatus::Succeeded,
        _ => GoldStatus::Failed(failed),
    })
}

async fn query_gold_status(c: &Config, workunit: &str) -> anyhow::Result<GoldStatus> {
    ensure!(
        workunit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "invalid workunit"
    );
    gold_status(&query(c, &format!(
        "SELECT r.id, r.server_state, r.outcome, r.client_state, r.exit_status, HEX(LEFT(r.stderr_out, 65536)) \
         FROM workunit w LEFT JOIN result r ON r.workunitid=w.id WHERE w.name='{workunit}' ORDER BY r.id"
    )).await?)
}

/// Where this host's next gold unit goes, or None while its latest attempt is pending,
/// succeeded or unknown. A failed attempt never parks the host: its BOINC diagnostics are
/// written to `failure.json` (on `--execute`) and the following attempt is planned.
async fn next_attempt(
    c: &Config,
    host_root: &Path,
    execute: bool,
) -> anyhow::Result<Option<PathBuf>> {
    let mut attempt = 0;
    while attempt_root(host_root, attempt + 1)
        .join("units/0000")
        .is_dir()
    {
        attempt += 1;
        ensure!(attempt < MAX_ATTEMPTS, "too many gold attempts");
    }
    let root = attempt_root(host_root, attempt);
    let unit = gold::submit::units_dir(&root).join("0000");
    // Not yet submitted, or uncertain: gold submission's own journal decides (and refuses).
    if !unit.join("submitted").is_file() {
        return Ok(Some(root));
    }
    let job: gold::job::Job =
        serde_json::from_slice(&read(&unit.join(gold::job::JOB_FILE), gold::job::MAX_JSON)?)?;
    let workunit = job.workunit();
    match query_gold_status(c, &workunit).await? {
        GoldStatus::Failed(results) => {
            if execute {
                let failure = serde_json::json!({ "workunit": workunit, "results": results });
                atomic(
                    &root.join("failure.json"),
                    &serde_json::to_vec_pretty(&failure)?,
                )?;
                eprintln!(
                    "gold attempt {attempt} ({workunit}) failed; diagnostics in {}; issuing another",
                    root.join("failure.json").display()
                );
            }
            Ok(Some(attempt_root(host_root, attempt + 1)))
        }
        GoldStatus::Unknown => {
            eprintln!("{workunit} is not in the database; not reissuing");
            Ok(None)
        }
        GoldStatus::Pending | GoldStatus::Succeeded => Ok(None),
    }
}

async fn legacy_blocks_attempt(
    c: &Config,
    legacy_host_root: &Path,
    active_host_root: &Path,
    execute: bool,
) -> anyhow::Result<bool> {
    if legacy_host_root == active_host_root || std::fs::symlink_metadata(legacy_host_root).is_err()
    {
        return Ok(false);
    }
    let mut attempt = 0;
    while attempt_root(legacy_host_root, attempt + 1)
        .join("units/0000")
        .is_dir()
    {
        attempt += 1;
        ensure!(attempt < MAX_ATTEMPTS, "too many gold attempts");
    }
    let root = attempt_root(legacy_host_root, attempt);
    let unit = gold::submit::units_dir(&root).join("0000");
    if !unit.join("submitted").is_file() {
        ensure!(
            std::fs::symlink_metadata(unit.join("submitting")).is_err(),
            "uncertain gold submission in {}; reconcile native campaign markers",
            unit.display()
        );
        return Ok(false);
    }
    if std::fs::symlink_metadata(active_host_root).is_ok() {
        return Ok(false);
    }
    let job: gold::job::Job =
        serde_json::from_slice(&read(&unit.join(gold::job::JOB_FILE), gold::job::MAX_JSON)?)?;
    let workunit = job.workunit();
    match query_gold_status(c, &workunit).await? {
        GoldStatus::Failed(results) => {
            if execute {
                let failure = serde_json::json!({ "workunit": workunit, "results": results });
                atomic(
                    &root.join("failure.json"),
                    &serde_json::to_vec_pretty(&failure)?,
                )?;
                eprintln!(
                    "legacy gold attempt {attempt} ({workunit}) failed; diagnostics in {}; issuing another",
                    root.join("failure.json").display()
                );
            }
            Ok(false)
        }
        GoldStatus::Unknown => {
            eprintln!("{workunit} is not in the database; not reissuing");
            Ok(true)
        }
        GoldStatus::Pending | GoldStatus::Succeeded => Ok(true),
    }
}

/// BOINC's `RESULT_SERVER_STATE_UNSENT`: the result is waiting for a client.
const SERVER_STATE_UNSENT: i64 = 2;
/// BOINC's `RESULT_SERVER_STATE_IN_PROGRESS`: the result has gone to a client.
const SERVER_STATE_IN_PROGRESS: i64 = 4;
/// BOINC's `RESULT_OUTCOME_DIDNT_NEED`: how `cancel_jobs` closes an unsent result.
const OUTCOME_DIDNT_NEED: i64 = 5;
/// BOINC's `WU_ERROR_CANCELLED` bit of `workunit.error_mask`, which `cancel_jobs` sets.
const WU_ERROR_CANCELLED: i64 = 16;
/// `assignment.target_type` of an assignment to one host.
const TARGET_HOST: i64 = 1;

async fn count(c: &Config, sql: &str) -> anyhow::Result<i64> {
    query(c, sql)
        .await?
        .trim()
        .parse()
        .context("invalid count from the database")
}

fn checked_workunit(workunit: &str) -> anyhow::Result<()> {
    ensure!(
        workunit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "invalid workunit"
    );
    Ok(())
}

/// Assignment rows that target `host` with `workunit`.
async fn host_assignments(c: &Config, workunit: &str, host: u64) -> anyhow::Result<i64> {
    checked_workunit(workunit)?;
    count(
        c,
        &format!(
            "SELECT COUNT(*) FROM assignment a JOIN workunit w ON w.id=a.workunitid \
         WHERE w.name='{workunit}' AND a.target_type={TARGET_HOST} AND a.target_id={host}"
        ),
    )
    .await
}

async fn delete_host_assignments(c: &Config, workunit: &str, host: u64) -> anyhow::Result<()> {
    checked_workunit(workunit)?;
    query(
        c,
        &format!(
            "DELETE a FROM assignment a JOIN workunit w ON w.id=a.workunitid \
         WHERE w.name='{workunit}' AND a.target_type={TARGET_HOST} AND a.target_id={host}"
        ),
    )
    .await?;
    Ok(())
}

/// Results of `workunit` with this server state, and (with `outcome`) this outcome.
async fn count_results(
    c: &Config,
    workunit: &str,
    server_state: i64,
    outcome: Option<i64>,
) -> anyhow::Result<i64> {
    checked_workunit(workunit)?;
    let outcome = outcome.map_or(String::new(), |o| format!(" AND r.outcome={o}"));
    count(
        c,
        &format!(
            "SELECT COUNT(*) FROM result r JOIN workunit w ON w.id=r.workunitid \
             WHERE w.name='{workunit}' AND r.server_state={server_state}{outcome}"
        ),
    )
    .await
}

/// What `cancel_jobs --name` does, in two steps that never revoke work a client has: each
/// unsent result becomes over and not needed (one `UPDATE` that only matches a result still
/// unsent, so a result the scheduler sends first stays its client's), then, if no result is in
/// progress, the workunit is marked cancelled so the transitioner makes no more. Returns false,
/// leaving the workunit live, when a result is in progress: a unit sent in the moment since the
/// caller looked is the client's to finish. `cancel_jobs` takes no such condition, so it isn't
/// run here.
async fn cancel_workunit(c: &Config, workunit: &str) -> anyhow::Result<bool> {
    checked_workunit(workunit)?;
    query(
        c,
        &format!(
            "UPDATE result r JOIN workunit w ON w.id=r.workunitid \
             SET r.server_state={SERVER_STATE_OVER}, r.outcome={OUTCOME_DIDNT_NEED} \
             WHERE w.name='{workunit}' AND r.server_state={SERVER_STATE_UNSENT}"
        ),
    )
    .await?;
    if count_results(c, workunit, SERVER_STATE_IN_PROGRESS, None).await? > 0 {
        return Ok(false);
    }
    query(
        c,
        &format!(
            "UPDATE workunit SET error_mask=error_mask|{WU_ERROR_CANCELLED}, \
             transition_time=UNIX_TIMESTAMP() WHERE name='{workunit}'"
        ),
    )
    .await?;
    Ok(true)
}

/// A targeted unit for a host no version of the app can reach never leaves the server: the
/// scheduler logs "App version for assigned WU not found" for its assignment on every request,
/// and the unit holds a place under the safety cap until its deadline (#1418). So when a host
/// is newly skipped for that, this campaign's outstanding unit for it is cancelled and its
/// assignment row deleted. Both belong to this campaign's own attempts, found under its
/// onboarding roots, never to another campaign's units for the host.
///
/// A unit is only retired while none of its results is in progress: one a client holds is left
/// to finish. The workunit is cancelled before the assignment goes, since an unassigned unit
/// would be open to any host. A cancelled unit whose assignment row is still there (the delete
/// failed last time) only has the row deleted. Returns the workunits retired.
async fn retire_unreachable(
    c: &Config,
    roots: &[&Path],
    hosts: &[u64],
    execute: bool,
) -> anyhow::Result<Vec<String>> {
    let mut retired = Vec::new();
    for &host in hosts {
        for root in roots {
            for attempt in submitted_attempts(&root.join(host.to_string()))? {
                let unit = gold::submit::units_dir(&attempt).join("0000");
                let job: gold::job::Job = serde_json::from_slice(&read(
                    &unit.join(gold::job::JOB_FILE),
                    gold::job::MAX_JSON,
                )?)?;
                let workunit = job.workunit();
                if job.assignment.as_ref().map(|a| a.host_id) != Some(host) {
                    eprintln!("{workunit} is not assigned to host {host}; leaving it");
                    continue;
                }
                if retire_unit(c, &workunit, host, execute).await? {
                    retired.push(workunit);
                }
            }
        }
    }
    Ok(retired)
}

async fn retire_unit(c: &Config, workunit: &str, host: u64, execute: bool) -> anyhow::Result<bool> {
    match query_gold_status(c, workunit).await? {
        GoldStatus::Pending => {
            // A unit that has a result in progress, or a success beside a replacement still
            // unsent, is a client's work: left alone.
            if count_results(c, workunit, SERVER_STATE_IN_PROGRESS, None).await? > 0 {
                eprintln!("{workunit} is in progress on host {host}; leaving it");
                return Ok(false);
            }
            if count_results(c, workunit, SERVER_STATE_OVER, Some(OUTCOME_SUCCESS)).await? > 0 {
                eprintln!("{workunit} has a successful result from host {host}; leaving it");
                return Ok(false);
            }
            if !execute {
                eprintln!("would cancel {workunit} and delete its assignment to host {host}");
                return Ok(true);
            }
            // The unit first: with the assignment gone and the unit live, any host could get it.
            if !cancel_workunit(c, workunit).await? {
                eprintln!("{workunit} was sent to host {host} as it was cancelled; leaving it");
                return Ok(false);
            }
            delete_host_assignments(c, workunit, host).await?;
            eprintln!("cancelled {workunit} and deleted its assignment to host {host}");
            Ok(true)
        }
        // Cancelled earlier, its assignment row left behind.
        GoldStatus::Failed(_) => {
            let cancelled = count(
                c,
                &format!(
                    "SELECT COALESCE(MAX(w.error_mask & {WU_ERROR_CANCELLED}), 0) FROM workunit w \
                 WHERE w.name='{workunit}'"
                ),
            )
            .await?;
            if cancelled == 0 || host_assignments(c, workunit, host).await? == 0 {
                return Ok(false);
            }
            if execute {
                delete_host_assignments(c, workunit, host).await?;
                eprintln!("deleted the assignment of cancelled {workunit} to host {host}");
            } else {
                eprintln!("would delete the assignment of cancelled {workunit} to host {host}");
            }
            Ok(true)
        }
        GoldStatus::Unknown | GoldStatus::Succeeded => Ok(false),
    }
}

/// Whether this campaign may reach `host`: every host, unless it's a native campaign.
fn in_campaign(c: &Config, host: &u64) -> bool {
    c.native_hosts.is_empty() || c.native_hosts.contains(host)
}

/// Discover hosts centrally, skip hosts already trusted for this prepared campaign's exact
/// `(language, profile, psm, dpi)`, `manifest_sha256`, and immutable worker image, and keep one
/// gold unit outstanding per untrusted host and campaign: a host whose attempt failed gets a new
/// one. Native gold submission journals handle uncertain dispatch.
pub async fn onboard(c: &Config, execute: bool) -> anyhow::Result<Vec<u64>> {
    c.validate()?;
    let scope = c
        .scope
        .as_ref()
        .context("prepared campaign scope is required for onboarding")?;
    let trust = Trust::load_or_none_trusted(&c.trust)?;
    let prepared = gold::prepare::read(&c.prepared)?;
    gold::submit::job(&prepared, &c.engine, 0)?;
    let app = if c.native_hosts.is_empty() {
        gold::submit::APP
    } else {
        gold::submit::NATIVE_APP
    };
    let inventory = capability::inventory(c).await?;
    let trusted_hosts =
        trust.campaign_trusted_hosts(&c.engine.image, &prepared.manifest_sha256, scope);
    let (campaign_root, legacy_root) = campaign_roots(c, &prepared, scope)?;
    let mut pending: Vec<_> = inventory
        .into_iter()
        .filter(|h| in_campaign(c, &h.id))
        .filter(|h| !trusted_hosts.is_some_and(|hosts| hosts.contains(&h.id)))
        .collect();
    if !pending.is_empty() {
        // A unit for a host that no version can reach waits out its deadline and holds a place
        // under the safety cap (#1418).
        let requirements = capability::requirements(c, app).await?;
        let (able, unable): (Vec<_>, Vec<_>) =
            pending.into_iter().partition(|h| requirements.can_run(h));
        if !unable.is_empty() {
            let mut ids: Vec<_> = unable.iter().take(20).map(|h| h.id.to_string()).collect();
            if unable.len() > ids.len() {
                ids.push("...".into());
            }
            eprintln!(
                "skipping {} host(s) no {app} version can run on: {}",
                unable.len(),
                ids.join(", ")
            );
        }
        pending = able;
        let skipped: Vec<u64> = unable.iter().map(|h| h.id).collect();
        let mut roots = vec![campaign_root.as_path()];
        if legacy_root != campaign_root {
            roots.push(&legacy_root);
        }
        retire_unreachable(c, &roots, &skipped, execute).await?;
    }
    let pending: Vec<u64> = pending.into_iter().map(|h| h.id).collect();
    // Hosts are visited in id order and the pass stops at max_hosts, so hosts with an
    // outstanding unit never starve newly discovered ones.
    let mut plans = Vec::new();
    for host in pending {
        let host_str = host.to_string();
        let active_host_root = campaign_root.join(&host_str);
        if legacy_blocks_attempt(c, &legacy_root.join(&host_str), &active_host_root, execute)
            .await?
        {
            continue;
        }
        if let Some(root) = next_attempt(c, &active_host_root, execute).await? {
            plans.push((host, root));
            if plans.len() == c.max_hosts {
                break;
            }
        }
    }
    if !execute {
        return Ok(plans.into_iter().map(|(host, _)| host).collect());
    }
    let _lock = Lock::take(&c.root)?;
    let shared = campaign_root.join("shared");
    if !plans.is_empty() {
        for name in [gold::prepare::PREPARED, gold::prepare::BUNDLE_COPY] {
            let limit = if name == gold::prepare::PREPARED {
                16 << 20
            } else {
                gold::job::MAX_BUNDLE as usize
            };
            let bytes = read(&c.prepared.join(name), limit)?;
            let dest = shared.join(name);
            if dest.exists() {
                ensure!(
                    read(&dest, limit)? == bytes,
                    "shared campaign input changed"
                );
            } else {
                atomic(&dest, &bytes)?;
            }
        }
        for original in &prepared.remotes {
            ensure!(
                gold::job::remote_name_ok(&original.name),
                "invalid prepared original name"
            );
            let source = c.prepared.join("cache").join(&original.name);
            let bytes = read(&source, gold::job::MAX_REMOTE as usize)?;
            use md5::Digest as _;
            let md5: String = md5::Md5::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            ensure!(
                bytes.len() as u64 == original.bytes
                    && boinc::sha256(&bytes) == original.sha256
                    && md5 == original.md5,
                "prepared original differs from its checksums"
            );
            let dest = shared.join("cache").join(&original.name);
            if dest.exists() {
                ensure!(
                    read(&dest, gold::job::MAX_REMOTE as usize)? == bytes,
                    "shared campaign original changed"
                );
            } else {
                atomic(&dest, &bytes)?;
            }
        }
    }
    let mut issued = Vec::new();
    for (host, root) in plans {
        for name in [gold::prepare::PREPARED, gold::prepare::BUNDLE_COPY] {
            let source = shared.join(name);
            let limit = if name == gold::prepare::PREPARED {
                16 << 20
            } else {
                gold::job::MAX_BUNDLE as usize
            };
            let dest = root.join(name);
            link_input(&source, &dest, limit)?;
        }
        for original in &prepared.remotes {
            let name = Path::new("cache").join(&original.name);
            link_input(
                &shared.join(&name),
                &root.join(&name),
                gold::job::MAX_REMOTE as usize,
            )?;
        }
        let s = gold::submit::run(&gold::submit::Options {
            app,
            root,
            project: c.project.clone(),
            units: 1,
            target_host: Some(host),
            docker_lines_version: None,
            engine: c.engine.clone(),
        })
        .await?;
        ensure!(
            s.uncertain == 0,
            "uncertain gold submission; reconcile native campaign markers"
        );
        issued.push(host);
    }
    Ok(issued)
}

fn link_input(source: &Path, dest: &Path, limit: usize) -> anyhow::Result<()> {
    link_input_with(source, dest, limit, |s, d| std::fs::hard_link(s, d))
}

/// Hard-links where the filesystem can; Azure Files SMB can't (EOPNOTSUPP), so the
/// already verified bytes are written atomically instead.
fn link_input_with(
    source: &Path,
    dest: &Path,
    limit: usize,
    link: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    let bytes = read(source, limit)?;
    if dest.exists() {
        ensure!(
            read(dest, limit)? == bytes,
            "onboarding campaign input changed"
        );
    } else {
        std::fs::create_dir_all(dest.parent().context("input has no parent")?)?;
        match link(source, dest) {
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => atomic(dest, &bytes)?,
            r => r?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_campaign_reaches_only_its_named_hosts() {
        let config = |native_hosts: &[u64]| -> Config {
            serde_json::from_value(serde_json::json!({
                "root": "/r", "project": "/p", "jobs_root": "/j", "mysql_defaults": "/m",
                "mysql_ca": "/c", "database": "d", "trust": "/t", "prepared": "/q",
                "engine": {"base": "sha256:b", "dpi": 300, "image": "sha256:i", "psm": 3,
                           "timeout_secs": 300, "version": "5.5.0"},
                "native_hosts": native_hosts, "max_hosts": 1, "max_spots": 1, "sample_every": 1,
            }))
            .unwrap()
        };
        assert!(in_campaign(&config(&[]), &5));
        assert!(in_campaign(&config(&[13]), &13));
        assert!(!in_campaign(&config(&[13]), &5));
    }

    #[test]
    fn gold_status_retries_only_terminal_failures_and_keeps_diagnostics() {
        assert_eq!(gold_status("").unwrap(), GoldStatus::Unknown);
        assert_eq!(
            gold_status("NULL\tNULL\tNULL\tNULL\tNULL\tNULL\n").unwrap(),
            GoldStatus::Pending
        );
        assert_eq!(
            gold_status("7\t4\t0\t1\t0\tNULL\n").unwrap(),
            GoldStatus::Pending
        );
        assert_eq!(
            gold_status("7\t5\t1\t5\t0\t\n").unwrap(),
            GoldStatus::Succeeded
        );
        // "Error -1" hex-encoded, as HEX(stderr_out) returns it.
        let GoldStatus::Failed(r) = gold_status("7\t5\t3\t5\t255\t4572726F72202D31\n").unwrap()
        else {
            panic!("expected a failure")
        };
        assert_eq!(
            r,
            vec![FailedResult {
                result: 7,
                outcome: 3,
                client_state: 5,
                exit_status: 255,
                stderr: "Error -1".into()
            }]
        );
        assert!(gold_status("7\t5\t3\n").is_err());
        assert!(gold_status("7\t5\t3\t5\t255\t4G\n").is_err());
    }

    #[test]
    fn onboarding_inputs_are_copied_where_hard_links_are_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("shared/bundle");
        atomic(&source, b"bundle").unwrap();
        let unsupported =
            |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
        let dest = dir.path().join("host/7/bundle");
        link_input_with(&source, &dest, 64, unsupported).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"bundle");
        // A retry accepts the identical copy and refuses a changed one.
        link_input_with(&source, &dest, 64, unsupported).unwrap();
        atomic(&dest, b"other").unwrap();
        assert!(link_input_with(&source, &dest, 64, unsupported).is_err());
        // Other link errors still fail.
        let denied =
            |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(link_input_with(&source, &dir.path().join("host/8/bundle"), 64, denied).is_err());
        let linked = dir.path().join("host/9/bundle");
        link_input(&source, &linked, 64).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&linked).unwrap().ino(),
            std::fs::metadata(&source).unwrap().ino()
        );
    }
}
