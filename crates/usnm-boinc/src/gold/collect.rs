//! Turn the gold units' canonical results into per-host report tables.
//!
//! Reads the unit directories under the campaign root and the spool that stock
//! `batch_collect_assimilator` writes (`<workunit>__file_gold-result.json`), checks each result
//! again against the bundle's expectations, and writes to `--out`:
//!
//! - `reports.jsonl`: one line per validated canonical result, keyed by bundle, challenge and
//!   engine identity, with scoped quality counts and (when native collection is enabled) the
//!   matching server result ID, host and receive time;
//! - `crops.jsonl`: one line per scored crop with counts only;
//! - `native-outcomes.jsonl`: bounded BOINC result metadata with no OCR text or stderr, including
//!   the finalized workunit's assimilation state and complete result-row count.
//!
//! No page text or transcription is written to these files.
use super::{submit, validate};
use crate::gold::job::{Job, JOB_FILE, MAX_JSON, RESULT_FILE};
use crate::{atomic, read};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub project: PathBuf,
    pub spool: PathBuf,
    pub out: PathBuf,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub collected: usize,
    pub pending: usize,
    pub failed: usize,
}

const MAX_NATIVE_JOBS: usize = 1_000;
const MAX_NATIVE_RESULTS: usize = 10_000;
const MAX_NATIVE_BYTES: usize = 16 << 20;
pub const BOINC_SERVER_STATE_OVER: i64 = 5;
pub const BOINC_ASSIMILATE_STATE_DONE: i64 = 2;
pub const BOINC_OUTCOME_SUCCESS: i64 = 1;
pub const BOINC_OUTCOME_CLIENT_ERROR: i64 = 3;
pub const BOINC_EXIT_ABORTED_BY_PROJECT: i64 = 202;
pub const BOINC_EXIT_ABORTED_VIA_GUI: i64 = 203;

/// Metadata-only snapshot of one BOINC server result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeResult {
    pub schema_version: u32,
    pub app: String,
    pub app_version_id: u64,
    pub bundle_version: u32,
    pub canonical: bool,
    pub dpi: u16,
    pub engine_image: String,
    pub host_id: Option<u64>,
    pub language: String,
    pub manifest_sha256: String,
    pub outcome: i64,
    pub exit_status: i64,
    pub profile: String,
    pub psm: u8,
    pub received_at: Option<i64>,
    pub result_id: u64,
    pub sent_at: Option<i64>,
    pub server_state: i64,
    pub workunit: String,
    pub workunit_assimilate_state: i64,
    pub workunit_result_count: u32,
}

#[derive(Debug, Clone)]
pub struct NativeConfig {
    pub mysql_defaults_file: PathBuf,
    pub mysql_ca_file: PathBuf,
    pub database: String,
    pub language: String,
    pub profile: String,
}

impl NativeConfig {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.database.is_empty()
                && self.database.len() <= 64
                && self
                    .database
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid database"
        );
        anyhow::ensure!(
            self.language.len() == 3 && self.language.bytes().all(|b| b.is_ascii_lowercase()),
            "calibrated source language must be a three-letter lower-case code"
        );
        anyhow::ensure!(
            !self.profile.is_empty()
                && self.profile.len() <= 64
                && self
                    .profile
                    .bytes()
                    .all(|b| { b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-') }),
            "invalid calibrated model profile"
        );
        let defaults = std::fs::symlink_metadata(&self.mysql_defaults_file)?;
        anyhow::ensure!(
            defaults.file_type().is_file(),
            "mysql defaults must be a regular file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            anyhow::ensure!(
                defaults.permissions().mode() & 0o077 == 0,
                "mysql defaults must be private"
            );
        }
        let ca = std::fs::symlink_metadata(&self.mysql_ca_file)?;
        anyhow::ensure!(ca.file_type().is_file(), "mysql CA must be a regular file");
        Ok(())
    }
}

/// Read bounded metadata for only this campaign's submitted workunits.
pub async fn native_results(
    root: &Path,
    config: &NativeConfig,
) -> anyhow::Result<Vec<NativeResult>> {
    config.validate()?;
    let dir = submit::units_dir(root);
    let mut jobs = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let unit = entry?.path();
        if !unit.join("submitted").exists() {
            continue;
        }
        anyhow::ensure!(
            jobs.len() < MAX_NATIVE_JOBS,
            "too many submitted Gold units"
        );
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JSON)?)?;
        job.validate()?;
        jobs.insert(job.workunit(), job);
    }
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    let names = jobs
        .keys()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT a.name,w.name,r.id,r.hostid,r.server_state,r.outcome,r.exit_status,r.sent_time,r.received_time,r.app_version_id,COALESCE(w.canonical_resultid,0),w.assimilate_state,(SELECT COUNT(*) FROM result all_results WHERE all_results.workunitid=w.id) FROM workunit w JOIN app a ON a.id=w.appid JOIN result r ON r.workunitid=w.id WHERE w.name IN ({names}) AND w.assimilate_state=2 AND r.app_version_id>0 ORDER BY r.id LIMIT {}",
        MAX_NATIVE_RESULTS + 1
    );
    let mut command = Command::new("mysql");
    let child = command
        .arg(format!(
            "--defaults-extra-file={}",
            config.mysql_defaults_file.display()
        ))
        .args(["--ssl", "--ssl-verify-server-cert"])
        .arg(format!("--ssl-ca={}", config.mysql_ca_file.display()))
        .args(["--batch", "--raw", "--skip-column-names"])
        .arg(format!("--database={}", config.database))
        .args(["--execute", &sql])
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), child)
        .await
        .context("Gold native result query timed out")??;
    anyhow::ensure!(
        output.status.success(),
        "read-only Gold result query failed"
    );
    anyhow::ensure!(
        output.stdout.len() <= MAX_NATIVE_BYTES,
        "Gold native result metadata exceeds its size limit"
    );
    parse_native_rows(
        std::str::from_utf8(&output.stdout).context("Gold native result metadata is not UTF-8")?,
        &jobs,
        config,
    )
}

fn parse_native_rows(
    text: &str,
    jobs: &BTreeMap<String, Job>,
    config: &NativeConfig,
) -> anyhow::Result<Vec<NativeResult>> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        anyhow::ensure!(fields.len() == 13, "invalid Gold native result row");
        let number = |s: &str| s.parse::<i64>().context("invalid Gold native result field");
        let result_id: u64 = fields[2].parse().context("invalid Gold result id")?;
        let canonical_id: u64 = match fields[10] {
            "NULL" => 0,
            value => value.parse().context("invalid canonical result id")?,
        };
        let host_id = match fields[3] {
            "NULL" | "0" => None,
            value => Some(value.parse().context("invalid Gold result host id")?),
        };
        let timestamp = |s: &str| -> anyhow::Result<Option<i64>> {
            match s {
                "NULL" | "0" => Ok(None),
                value => {
                    let epoch = value.parse::<i64>().context("invalid server timestamp")?;
                    anyhow::ensure!(epoch > 0, "invalid server timestamp");
                    Ok(Some(epoch))
                }
            }
        };
        let workunit = fields[1].to_owned();
        let job = jobs
            .get(&workunit)
            .context("unknown Gold workunit in native result")?;
        anyhow::ensure!(
            fields[0] == submit::APP,
            "native result belongs to another BOINC app"
        );
        rows.push(NativeResult {
            schema_version: 3,
            app: fields[0].to_owned(),
            app_version_id: number(fields[9])?
                .try_into()
                .context("invalid app version id")?,
            bundle_version: job.bundle_version,
            canonical: result_id == canonical_id,
            dpi: job.engine.dpi,
            engine_image: job.engine.image.clone(),
            host_id,
            language: config.language.clone(),
            manifest_sha256: job.manifest_sha256.clone(),
            outcome: number(fields[5])?,
            exit_status: number(fields[6])?,
            profile: config.profile.clone(),
            psm: job.engine.psm,
            received_at: timestamp(fields[8])?,
            result_id,
            sent_at: timestamp(fields[7])?,
            server_state: number(fields[4])?,
            workunit,
            workunit_assimilate_state: number(fields[11])?,
            workunit_result_count: number(fields[12])?
                .try_into()
                .context("invalid workunit result count")?,
        });
        anyhow::ensure!(
            rows.len() <= MAX_NATIVE_RESULTS,
            "too many Gold native results"
        );
    }
    validate_complete_result_sets(&rows)?;
    Ok(rows)
}

pub(crate) fn validate_complete_result_sets(rows: &[NativeResult]) -> anyhow::Result<()> {
    let mut observed = BTreeMap::<&str, (i64, u32, usize)>::new();
    for row in rows {
        anyhow::ensure!(
            row.schema_version == 3
                && row.workunit_assimilate_state == BOINC_ASSIMILATE_STATE_DONE
                && row.server_state == BOINC_SERVER_STATE_OVER
                && row.workunit_result_count > 0,
            "{}: native result snapshot is not finalized",
            row.workunit
        );
        let entry = observed.entry(&row.workunit).or_insert((
            row.workunit_assimilate_state,
            row.workunit_result_count,
            0,
        ));
        anyhow::ensure!(
            entry.0 == row.workunit_assimilate_state && entry.1 == row.workunit_result_count,
            "{}: inconsistent finalized workunit snapshot",
            row.workunit
        );
        entry.2 += 1;
    }
    for (workunit, (state, expected, observed)) in observed {
        anyhow::ensure!(
            state == BOINC_ASSIMILATE_STATE_DONE && observed == expected as usize,
            "{workunit}: captured {observed} of {expected} finalized BOINC results"
        );
    }
    Ok(())
}

pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    run_inner(opts, None)
}

pub fn run_with_native(
    opts: &Options,
    native: &[NativeResult],
    config: &NativeConfig,
) -> anyhow::Result<Summary> {
    run_inner(opts, Some((native, config)))
}

fn run_inner(
    opts: &Options,
    native: Option<(&[NativeResult], &NativeConfig)>,
) -> anyhow::Result<Summary> {
    let mut summary = Summary::default();
    let (mut reports, mut crops, mut outcomes) = (String::new(), String::new(), String::new());
    let dir = submit::units_dir(&opts.root);
    let mut units: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("submitted").exists())
        .collect();
    units.sort();
    let mut native_by_workunit = BTreeMap::<String, Vec<NativeResult>>::new();
    if let Some((rows, config)) = native {
        let jobs = units
            .iter()
            .map(|unit| {
                let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JSON)?)?;
                Ok::<_, anyhow::Error>((job.workunit(), job))
            })
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
        let mut by_result = BTreeMap::<(String, u64), NativeResult>::new();
        let previous = opts.out.join("native-outcomes.jsonl");
        if previous.exists() {
            let bytes = read(&previous, MAX_NATIVE_BYTES)?;
            for (line_number, line) in std::str::from_utf8(&bytes)?.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let saved: NativeResult = serde_json::from_str(line)
                    .with_context(|| format!("{}:{}", previous.display(), line_number + 1))?;
                let key = (saved.workunit.clone(), saved.result_id);
                anyhow::ensure!(
                    by_result.insert(key, saved).is_none(),
                    "duplicate saved native BOINC result"
                );
                anyhow::ensure!(
                    by_result.len() <= MAX_NATIVE_RESULTS,
                    "too many saved Gold native results"
                );
            }
        }
        validate_complete_result_sets(&by_result.values().cloned().collect::<Vec<_>>())?;
        let mut fresh = BTreeSet::new();
        for row in rows {
            let key = (row.workunit.clone(), row.result_id);
            anyhow::ensure!(fresh.insert(key.clone()), "duplicate native BOINC result");
            if let Some(saved) = by_result.get(&key) {
                anyhow::ensure!(
                    saved.workunit_assimilate_state == row.workunit_assimilate_state
                        && saved.workunit_result_count == row.workunit_result_count,
                    "{}: finalized native workunit snapshot changed",
                    row.workunit
                );
            }
            by_result.insert(key, row.clone());
        }
        let mut ordered: Vec<_> = by_result.into_values().collect();
        ordered.sort_by(|a, b| (&a.workunit, a.result_id).cmp(&(&b.workunit, b.result_id)));
        let mut seen = BTreeSet::new();
        for row in ordered {
            let job = jobs.get(&row.workunit).with_context(|| {
                format!("unknown Gold workunit {} in native results", row.workunit)
            })?;
            anyhow::ensure!(
                row.schema_version == 3
                    && row.workunit_assimilate_state == BOINC_ASSIMILATE_STATE_DONE
                    && row.workunit_result_count > 0
                    && row.app == submit::APP
                    && row.bundle_version == job.bundle_version
                    && row.engine_image == job.engine.image
                    && row.manifest_sha256 == job.manifest_sha256
                    && row.psm == job.engine.psm
                    && row.dpi == job.engine.dpi
                    && row.language == config.language
                    && row.profile == config.profile
                    && row.result_id > 0
                    && row.app_version_id > 0
                    && row.host_id.is_none_or(|host| host > 0)
                    && (0..=16).contains(&row.server_state)
                    && (0..=16).contains(&row.outcome)
                    && (-4096..=4096).contains(&row.exit_status)
                    && row.sent_at.is_none_or(|time| time > 0)
                    && row.received_at.is_none_or(|time| time > 0),
                "{}: native result identity or scope mismatch",
                row.workunit
            );
            anyhow::ensure!(
                seen.insert((row.workunit.clone(), row.result_id)),
                "duplicate native BOINC result"
            );
            outcomes.push_str(&serde_json::to_string(&row)?);
            outcomes.push('\n');
            native_by_workunit
                .entry(row.workunit.clone())
                .or_default()
                .push(row);
        }
        anyhow::ensure!(
            outcomes.len() <= MAX_NATIVE_BYTES,
            "Gold native outcome metadata exceeds its size limit"
        );
        validate_complete_result_sets(
            &native_by_workunit
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>(),
        )?;
    }
    let expected_dir = validate::expected_dir(&opts.project);
    for unit in units {
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JSON)?)?;
        let wu = job.workunit();
        if opts.spool.join(format!("{wu}_error")).exists()
            || opts.spool.join(format!("{wu}_no_output_files")).exists()
        {
            if native.is_some() {
                anyhow::ensure!(
                    native_by_workunit.get(&wu).is_some_and(|results| {
                        !results.is_empty()
                            && results
                                .iter()
                                .all(|result| result.server_state == BOINC_SERVER_STATE_OVER)
                    }),
                    "{wu}: terminal assimilator marker has unresolved BOINC results"
                );
            }
            summary.failed += 1;
            continue;
        }
        let file = opts.spool.join(format!("{wu}__file_{RESULT_FILE}"));
        if !file.exists() {
            summary.pending += 1;
            continue;
        }
        let r = validate::load(&file)?;
        let e = validate::load_expected(&expected_dir, &r.manifest_sha256)?;
        validate::check_report(&r, &e).with_context(|| format!("canonical result {wu}"))?;
        let b = r.boinc.as_ref().context("not a BOINC report")?;
        anyhow::ensure!(
            b.job_id == job.id,
            "{wu}: result does not belong to its job"
        );
        let key = json!({
            "bundle_version": r.bundle_version,
            "engine_base": b.engine_base,
            "engine_image": b.engine_image,
            "engine_version": r.engine.version,
            "host_id": b.host_id,
            "manifest_sha256": r.manifest_sha256,
            "workunit": wu,
        });
        let mut row = key.clone();
        row["completed_at"] = json!(r.completed_at);
        if let Some((_, config)) = native {
            let candidates = native_by_workunit
                .get(&wu)
                .context("missing native Gold results for canonical report")?;
            let canonical: Vec<_> = candidates
                .iter()
                .filter(|candidate| candidate.canonical)
                .collect();
            anyhow::ensure!(
                canonical.len() == 1,
                "{wu}: missing or ambiguous canonical native result"
            );
            let result = canonical[0];
            anyhow::ensure!(
                result.outcome == BOINC_OUTCOME_SUCCESS
                    && result.server_state == BOINC_SERVER_STATE_OVER,
                "{wu}: assimilated canonical result disagrees with BOINC server outcome"
            );
            anyhow::ensure!(
                result.host_id.is_some() && result.host_id == b.host_id,
                "{wu}: client and server host identities disagree"
            );
            let received_at = result
                .received_at
                .context("canonical BOINC result lacks server receive time")?;
            row["host_id"] = json!(result.host_id);
            row["native_result_id"] = json!(result.result_id);
            row["server_received_at"] = json!(received_at);
            row["language"] = json!(config.language);
            row["profile"] = json!(config.profile);
        }
        row["crops"] = json!(r.crops.len());
        row["skipped"] = json!(r.skipped.len());
        row["profiles"] = json!(r.crops.iter().map(|c| &c.profile).collect::<BTreeSet<_>>());
        row["psm"] = json!(r.engine.psm);
        row["dpi"] = json!(r.engine.dpi);
        row["complete_loss_crops"] = json!(r
            .crops
            .iter()
            .filter(|c| c.result.counts.char_edits >= c.result.counts.ref_chars)
            .count());
        row["summary"] = serde_json::to_value(&r.summary)?;
        reports.push_str(&serde_json::to_string(&row)?);
        reports.push('\n');
        for c in &r.crops {
            let mut row = key.clone();
            row["crop"] = serde_json::to_value(c)?;
            crops.push_str(&serde_json::to_string(&row)?);
            crops.push('\n');
        }
        summary.collected += 1;
    }
    std::fs::create_dir_all(&opts.out)?;
    atomic(&opts.out.join("reports.jsonl"), reports.as_bytes())?;
    atomic(&opts.out.join("crops.jsonl"), crops.as_bytes())?;
    let native_path = opts.out.join("native-outcomes.jsonl");
    if native.is_some() || !native_path.exists() {
        atomic(&native_path, outcomes.as_bytes())?;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gold::{submit, validate::tests};

    #[test]
    fn parses_authoritative_result_metadata_for_an_untargeted_gold_unit() {
        let prepared = submit::tests::prepared();
        let job = submit::job(&prepared, &submit::tests::engine(), 0).unwrap();
        assert!(job.assignment.is_none());
        let jobs = BTreeMap::from([(job.workunit(), job.clone())]);
        let config = NativeConfig {
            mysql_defaults_file: PathBuf::new(),
            mysql_ca_file: PathBuf::new(),
            database: "synthetic".into(),
            language: "eng".into(),
            profile: "eng".into(),
        };
        let line = format!(
            "usnm_gold\t{}\t101\t7\t5\t3\t1\t100\t200\t11\t0\t2\t1",
            job.workunit()
        );
        let parsed = parse_native_rows(&line, &jobs, &config).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].result_id, 101);
        assert_eq!(parsed[0].host_id, Some(7));
        assert_eq!(parsed[0].app, "usnm_gold");
        assert_eq!(parsed[0].engine_image, job.engine.image);
        assert_eq!(parsed[0].received_at, Some(200));
        assert_eq!(parsed[0].outcome, BOINC_OUTCOME_CLIENT_ERROR);
        assert_eq!(
            parsed[0].workunit_assimilate_state,
            BOINC_ASSIMILATE_STATE_DONE
        );
        assert_eq!(parsed[0].workunit_result_count, 1);
        assert!(!parsed[0].canonical);
        assert!(parse_native_rows(&line.replace("\t2\t1", "\t2\t2"), &jobs, &config).is_err());
        assert!(parse_native_rows(&line.replace("\t2\t1", "\t1\t1"), &jobs, &config).is_err());
        assert!(parse_native_rows(&line.replace("usnm_gold", "usnm_ocr"), &jobs, &config).is_err());
        assert!(
            parse_native_rows(&line.replace("\t200\t", "\t0\t"), &jobs, &config).unwrap()[0]
                .received_at
                .is_none()
        );
    }

    #[test]
    fn collects_valid_results_and_counts_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let root = tmp.path().join("root");
        let spool = tmp.path().join("spool");
        std::fs::create_dir_all(&spool).unwrap();
        let (e, _) = tests::setup(&validate::expected_dir(&project));
        let p = submit::tests::prepared();
        let jobs: Vec<_> = (0..3)
            .map(|n| submit::job(&p, &submit::tests::engine(), n).unwrap())
            .collect();
        validate::add_jobs(&validate::expected_dir(&project), &p, &jobs).unwrap();
        let e =
            validate::load_expected(&validate::expected_dir(&project), &e.manifest_sha256).unwrap();
        for j in &jobs {
            let d = submit::units_dir(&root).join(format!("{:04}", j.unit));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(JOB_FILE), serde_json::to_vec(j).unwrap()).unwrap();
            std::fs::write(d.join("submitted"), j.workunit()).unwrap();
        }
        let mut damaged = tests::report(&e, &jobs[0].id);
        damaged.crops[0].result.counts.char_edits = damaged.crops[0].result.counts.ref_chars;
        damaged.crops[0].result.counts.word_edits = damaged.crops[0].result.counts.ref_words;
        damaged.summary = usnm_ocr_survey::bundle::summarize(
            &damaged
                .crops
                .iter()
                .map(|c| c.result.clone())
                .collect::<Vec<_>>(),
        );
        std::fs::write(
            spool.join(format!("{}__file_{RESULT_FILE}", jobs[0].workunit())),
            serde_json::to_vec(&damaged).unwrap(),
        )
        .unwrap();
        std::fs::write(spool.join(format!("{}_error", jobs[1].workunit())), "").unwrap();
        let out = tmp.path().join("out");
        let options = Options {
            root,
            project,
            spool,
            out: out.clone(),
        };
        let s = run(&options).unwrap();
        assert_eq!((s.collected, s.failed, s.pending), (1, 1, 1));
        let rows = std::fs::read_to_string(out.join("reports.jsonl")).unwrap();
        assert_eq!(rows.lines().count(), 1);
        assert!(rows.contains("\"host_id\":7"));
        let row: serde_json::Value = serde_json::from_str(rows.trim()).unwrap();
        assert_eq!(row["complete_loss_crops"], 1);
        assert_eq!(row["profiles"], serde_json::json!(["eng"]));
        assert_eq!(row["psm"], 3);
        assert_eq!(row["dpi"], 300);
        let scope = crate::trust::Scope {
            language: "eng".into(),
            profile: "eng".into(),
            psm: 3,
            dpi: 300,
        };
        let trust = crate::trust::Trust::build_without_native(
            &[out.join("reports.jsonl")],
            1.0,
            &e.manifest_sha256,
            scope.clone(),
        )
        .unwrap();
        assert!(
            trust.hosts.is_empty(),
            "aggregate acceptance cannot hide a lost crop"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("crops.jsonl"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        std::fs::write(
            options
                .spool
                .join(format!("{}__file_{RESULT_FILE}", jobs[0].workunit())),
            serde_json::to_vec(&tests::report(&e, &jobs[0].id)).unwrap(),
        )
        .unwrap();
        run(&options).unwrap();
        let trust = crate::trust::Trust::build_without_native(
            &[out.join("reports.jsonl")],
            1.0,
            &e.manifest_sha256,
            scope.clone(),
        )
        .unwrap();
        assert!(
            !trust.hosts.is_empty(),
            "complete positive remains eligible"
        );
        for wrong_profile in [false, true] {
            let mut newer = tests::report(&e, &jobs[2].id);
            newer.completed_at = "2026-10-14T00:00:00Z".into();
            if wrong_profile {
                newer.crops[0].profile = "fra".into();
            } else {
                let counts = &mut newer.crops[0].result.counts;
                counts.char_edits = counts.ref_chars;
                counts.word_edits = counts.ref_words;
            }
            newer.summary = usnm_ocr_survey::bundle::summarize(
                &newer
                    .crops
                    .iter()
                    .map(|c| c.result.clone())
                    .collect::<Vec<_>>(),
            );
            std::fs::write(
                options
                    .spool
                    .join(format!("{}__file_{RESULT_FILE}", jobs[2].workunit())),
                serde_json::to_vec(&newer).unwrap(),
            )
            .unwrap();
            let collected = run(&options).unwrap();
            assert_eq!(
                (collected.collected, collected.failed, collected.pending),
                (2, 1, 0)
            );
            let trust = crate::trust::Trust::build_without_native(
                &[out.join("reports.jsonl")],
                1.0,
                &e.manifest_sha256,
                scope.clone(),
            )
            .unwrap();
            assert!(
                trust.hosts.is_empty(),
                "newer validated canonical quality failure revokes the prior positive"
            );
        }
    }

    fn native(
        job: &Job,
        result_id: u64,
        host: u64,
        outcome: i64,
        received_at: i64,
        canonical: bool,
        scope: (&str, &str),
    ) -> NativeResult {
        NativeResult {
            schema_version: 3,
            app: submit::APP.into(),
            app_version_id: 100,
            bundle_version: job.bundle_version,
            canonical,
            dpi: job.engine.dpi,
            engine_image: job.engine.image.clone(),
            host_id: Some(host),
            language: scope.0.into(),
            manifest_sha256: job.manifest_sha256.clone(),
            outcome,
            exit_status: if outcome == BOINC_OUTCOME_CLIENT_ERROR {
                1
            } else {
                0
            },
            profile: scope.1.into(),
            psm: job.engine.psm,
            received_at: Some(received_at),
            result_id,
            sent_at: Some(received_at - 60),
            server_state: 5,
            workunit: job.workunit(),
            workunit_assimilate_state: BOINC_ASSIMILATE_STATE_DONE,
            workunit_result_count: 1,
        }
    }

    fn save_job(root: &std::path::Path, job: &Job) {
        let dir = submit::units_dir(root).join(format!("{:04}", job.unit));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(JOB_FILE), serde_json::to_vec(job).unwrap()).unwrap();
        std::fs::write(dir.join("submitted"), job.workunit()).unwrap();
    }

    fn save_report(
        spool: &std::path::Path,
        job: &Job,
        e: &validate::Expectations,
        host: u64,
        profile: &str,
    ) {
        let mut report = tests::report(e, &job.id);
        let boinc = report.boinc.as_mut().unwrap();
        boinc.host_id = Some(host);
        for crop in &mut report.crops {
            crop.profile = profile.to_owned();
        }
        std::fs::write(
            spool.join(format!("{}__file_{RESULT_FILE}", job.workunit())),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn terminal_client_error_revokes_only_the_matching_host_image_and_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let (initial, _) = tests::setup(&validate::expected_dir(&project));
        let prepared = submit::tests::prepared();
        let image_a = submit::tests::engine();
        let mut image_b = image_a.clone();
        image_b.image = format!("sha256:{}", "f".repeat(64));
        let jobs = vec![
            submit::job(&prepared, &image_a, 0).unwrap(),
            submit::job(&prepared, &image_a, 1).unwrap(),
            submit::job(&prepared, &image_a, 2).unwrap(),
            submit::job(&prepared, &image_b, 3).unwrap(),
            submit::job(&prepared, &image_a, 4).unwrap(),
        ];
        validate::add_jobs(&validate::expected_dir(&project), &prepared, &jobs).unwrap();
        let expected =
            validate::load_expected(&validate::expected_dir(&project), &initial.manifest_sha256)
                .unwrap();
        let eng_root = tmp.path().join("eng-root");
        let fra_root = tmp.path().join("fra-root");
        for job in &jobs[..4] {
            save_job(&eng_root, job);
        }
        save_job(&fra_root, &jobs[4]);
        let eng_spool = tmp.path().join("eng-spool");
        let fra_spool = tmp.path().join("fra-spool");
        std::fs::create_dir_all(&eng_spool).unwrap();
        std::fs::create_dir_all(&fra_spool).unwrap();
        save_report(&eng_spool, &jobs[0], &expected, 7, "eng");
        save_report(&eng_spool, &jobs[2], &expected, 8, "eng");
        save_report(&eng_spool, &jobs[3], &expected, 7, "eng");
        // A worker that scores no crops returns an error; the stock assimilator records this
        // terminal marker instead of a fabricated zero-crop canonical report.
        std::fs::write(eng_spool.join(format!("{}_error", jobs[1].workunit())), "").unwrap();
        save_report(&fra_spool, &jobs[4], &expected, 7, "fra");
        let eng_config = NativeConfig {
            mysql_defaults_file: PathBuf::new(),
            mysql_ca_file: PathBuf::new(),
            database: "synthetic".into(),
            language: "eng".into(),
            profile: "eng".into(),
        };
        let fra_config = NativeConfig {
            language: "fra".into(),
            profile: "fra".into(),
            ..eng_config.clone()
        };
        let eng_native = vec![
            native(&jobs[0], 101, 7, 1, 100, true, ("eng", "eng")),
            native(&jobs[1], 102, 7, 3, 200, false, ("eng", "eng")),
            native(&jobs[2], 103, 8, 1, 110, true, ("eng", "eng")),
            native(&jobs[3], 104, 7, 1, 120, true, ("eng", "eng")),
        ];
        let fra_native = vec![native(&jobs[4], 105, 7, 1, 150, true, ("fra", "fra"))];
        let options = |root: PathBuf, spool: PathBuf, out: PathBuf| Options {
            root,
            project: project.clone(),
            spool,
            out,
        };
        let eng_out = tmp.path().join("eng-out");
        let fra_out = tmp.path().join("fra-out");
        let eng_summary = run_with_native(
            &options(eng_root.clone(), eng_spool.clone(), eng_out.clone()),
            &eng_native,
            &eng_config,
        )
        .unwrap();
        assert_eq!((eng_summary.collected, eng_summary.failed), (3, 1));
        let collected_reports = std::fs::read_to_string(eng_out.join("reports.jsonl")).unwrap();
        assert_eq!(collected_reports.lines().count(), 3);
        assert!(!collected_reports.contains(&jobs[1].workunit()));
        assert_eq!(
            std::fs::read_to_string(eng_out.join("native-outcomes.jsonl"))
                .unwrap()
                .lines()
                .count(),
            4
        );
        let mut missing_old_row = eng_native.clone();
        missing_old_row.retain(|row| row.result_id != 102);
        let lost_out = tmp.path().join("lost-terminal-out");
        assert!(run_with_native(
            &options(eng_root.clone(), eng_spool.clone(), lost_out.clone()),
            &missing_old_row,
            &eng_config,
        )
        .is_err());
        assert!(
            !lost_out.exists(),
            "incomplete evidence must not be written"
        );
        let retry_out = tmp.path().join("purged-retry-out");
        std::fs::create_dir_all(&retry_out).unwrap();
        let mut pre_retry_snapshot = native(&jobs[1], 102, 7, 2, 190, false, ("eng", "eng"));
        pre_retry_snapshot.workunit_assimilate_state = 1;
        let pre_retry_path = retry_out.join("native-outcomes.jsonl");
        std::fs::write(
            &pre_retry_path,
            format!("{}\n", serde_json::to_string(&pre_retry_snapshot).unwrap()),
        )
        .unwrap();
        assert!(
            run_with_native(
                &options(eng_root.clone(), eng_spool.clone(), retry_out.clone()),
                &missing_old_row,
                &eng_config,
            )
            .is_err(),
            "an earlier non-final result snapshot cannot be upgraded after a retry is purged"
        );
        assert_eq!(
            std::fs::read_to_string(&pre_retry_path).unwrap(),
            format!("{}\n", serde_json::to_string(&pre_retry_snapshot).unwrap()),
            "uncertified prior evidence must not be replaced"
        );
        let stale_out = tmp.path().join("stale-terminal-out");
        std::fs::create_dir_all(&stale_out).unwrap();
        let mut stale_snapshot = eng_native.clone();
        stale_snapshot[1].server_state = 4;
        stale_snapshot[1].outcome = 0;
        stale_snapshot[1].received_at = None;
        stale_snapshot[1].workunit_result_count = 2;
        let mut sibling = native(&jobs[1], 106, 8, 2, 210, false, ("eng", "eng"));
        sibling.workunit_result_count = 2;
        stale_snapshot.push(sibling);
        let stale_contents = stale_snapshot
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let stale_path = stale_out.join("native-outcomes.jsonl");
        std::fs::write(&stale_path, format!("{stale_contents}\n")).unwrap();
        assert!(
            run_with_native(
                &options(eng_root.clone(), eng_spool.clone(), stale_out.clone()),
                &missing_old_row,
                &eng_config,
            )
            .is_err(),
            "a stale in-progress snapshot cannot satisfy a terminal marker"
        );
        assert_eq!(
            std::fs::read_to_string(stale_path).unwrap(),
            format!("{stale_contents}\n"),
            "unresolved terminal evidence must not replace the prior snapshot"
        );
        run_with_native(
            &options(eng_root.clone(), eng_spool.clone(), eng_out.clone()),
            &missing_old_row,
            &eng_config,
        )
        .unwrap();
        let retained_outcomes =
            std::fs::read_to_string(eng_out.join("native-outcomes.jsonl")).unwrap();
        assert_eq!(retained_outcomes.lines().count(), 4);
        assert!(retained_outcomes.contains("\"result_id\":102"));
        let file_only_out = tmp.path().join("file-only-out");
        std::fs::create_dir_all(&file_only_out).unwrap();
        std::fs::copy(
            eng_out.join("native-outcomes.jsonl"),
            file_only_out.join("native-outcomes.jsonl"),
        )
        .unwrap();
        run(&options(
            eng_root.clone(),
            eng_spool.clone(),
            file_only_out.clone(),
        ))
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(file_only_out.join("native-outcomes.jsonl"))
                .unwrap()
                .lines()
                .count(),
            4,
            "file-only recollection must not erase authoritative outcome history"
        );
        run_with_native(
            &options(fra_root, fra_spool, fra_out.clone()),
            &fra_native,
            &fra_config,
        )
        .unwrap();
        let reports = [eng_out.join("reports.jsonl"), fra_out.join("reports.jsonl")];
        let outcomes = [
            eng_out.join("native-outcomes.jsonl"),
            fra_out.join("native-outcomes.jsonl"),
        ];
        let eng_trust = crate::trust::Trust::build_with_outcomes(
            &reports,
            &outcomes,
            1.0,
            &expected.manifest_sha256,
            crate::trust::Scope {
                language: "eng".into(),
                profile: "eng".into(),
                psm: 3,
                dpi: 300,
            },
        )
        .unwrap();
        let incomplete_outcomes = tmp.path().join("incomplete-outcomes.jsonl");
        let malformed = std::fs::read_to_string(eng_out.join("native-outcomes.jsonl"))
            .unwrap()
            .lines()
            .map(|line| {
                let mut row: serde_json::Value = serde_json::from_str(line).unwrap();
                if row["result_id"] == 101 {
                    row["workunit_result_count"] = json!(2);
                }
                row.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&incomplete_outcomes, format!("{malformed}\n")).unwrap();
        assert!(crate::trust::Trust::build_with_outcomes(
            &reports,
            &[incomplete_outcomes],
            1.0,
            &expected.manifest_sha256,
            crate::trust::Scope {
                language: "eng".into(),
                profile: "eng".into(),
                psm: 3,
                dpi: 300,
            },
        )
        .is_err());
        let image_a = &jobs[0].engine.image;
        let image_b = &jobs[3].engine.image;
        assert!(eng_trust.refuse(image_a, Some(7)).is_some());
        assert!(eng_trust.refuse(image_a, Some(8)).is_none());
        assert!(eng_trust.refuse(image_b, Some(7)).is_none());
        let fra_trust = crate::trust::Trust::build_with_outcomes(
            &reports,
            &outcomes,
            1.0,
            &expected.manifest_sha256,
            crate::trust::Scope {
                language: "fra".into(),
                profile: "fra".into(),
                psm: 3,
                dpi: 300,
            },
        )
        .unwrap();
        assert!(fra_trust.refuse(image_a, Some(7)).is_none());

        let mut unbound_report: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&reports[0])
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        unbound_report.as_object_mut().unwrap().remove("language");
        let unbound_path = tmp.path().join("unbound-report.jsonl");
        std::fs::write(&unbound_path, format!("{}\n", unbound_report)).unwrap();
        assert!(
            crate::trust::Trust::build_with_outcomes(
                &[unbound_path],
                &outcomes,
                1.0,
                &expected.manifest_sha256,
                crate::trust::Scope {
                    language: "fra".into(),
                    profile: "fra".into(),
                    psm: 3,
                    dpi: 300,
                },
            )
            .is_err(),
            "missing native scope must fail even when outside the requested scope"
        );

        for (attempt_outcome, host, exit_status) in [
            (2, Some(7), 0),
            (4, Some(7), 0),
            (5, Some(7), 0),
            (3, Some(7), BOINC_EXIT_ABORTED_BY_PROJECT),
            (3, Some(7), BOINC_EXIT_ABORTED_VIA_GUI),
            (3, None, 1),
        ] {
            let mut nonrevoking = eng_native.clone();
            nonrevoking[1].outcome = attempt_outcome;
            nonrevoking[1].host_id = host;
            nonrevoking[1].exit_status = exit_status;
            let alternate = tmp.path().join(format!(
                "outcome-{attempt_outcome}-{exit_status}-{}.jsonl",
                host.unwrap_or(0)
            ));
            let content = nonrevoking
                .iter()
                .map(|event| serde_json::to_string(event).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&alternate, format!("{content}\n")).unwrap();
            let unaffected = crate::trust::Trust::build_with_outcomes(
                &reports,
                &[alternate, outcomes[1].clone()],
                1.0,
                &expected.manifest_sha256,
                crate::trust::Scope {
                    language: "eng".into(),
                    profile: "eng".into(),
                    psm: 3,
                    dpi: 300,
                },
            )
            .unwrap();
            assert!(
                unaffected.refuse(image_a, Some(7)).is_none(),
                "outcome {attempt_outcome} with exit status {exit_status} and host {host:?} must not revoke"
            );
        }

        let mut no_receive_time = eng_native.clone();
        no_receive_time[1].received_at = None;
        let no_receive_path = tmp.path().join("no-receive-time.jsonl");
        std::fs::write(
            &no_receive_path,
            no_receive_time
                .iter()
                .map(|event| serde_json::to_string(event).unwrap())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let no_receive_trust = crate::trust::Trust::build_with_outcomes(
            &reports,
            &[no_receive_path, outcomes[1].clone()],
            1.0,
            &expected.manifest_sha256,
            crate::trust::Scope {
                language: "eng".into(),
                profile: "eng".into(),
                psm: 3,
                dpi: 300,
            },
        )
        .unwrap();
        assert!(no_receive_trust.refuse(image_a, Some(7)).is_none());

        let mut tied = eng_native.clone();
        tied[1].received_at = Some(100);
        tied[1].sent_at = Some(40);
        let tied_path = tmp.path().join("tied-outcomes.jsonl");
        std::fs::write(
            &tied_path,
            tied.iter()
                .map(|event| serde_json::to_string(event).unwrap())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let tied_trust = crate::trust::Trust::build_with_outcomes(
            &reports,
            &[tied_path, outcomes[1].clone()],
            1.0,
            &expected.manifest_sha256,
            crate::trust::Scope {
                language: "eng".into(),
                profile: "eng".into(),
                psm: 3,
                dpi: 300,
            },
        )
        .unwrap();
        assert!(tied_trust.refuse(image_a, Some(7)).is_some());
    }
}
