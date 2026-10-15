//! Hand staged units to stock `create_work`, two results per unit, with the
//! assessment's write-ahead markers (`submitting`, then `submitted`).
use super::{units, APP, JOB_FILE, MODEL_DIR, PAGE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use usnm_ocr_contract::{
    quality::MAX_SCAN,
    qwen::{Job, MAX_JOB_JSON},
};

const EMBEDDED_WEIGHTS_LOCK: &str = include_str!("../../../../ocr/boinc/qwen/weights.lock");

/// Execution target and input template routing for `usnm_qwen` workunits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Target {
    /// Docker/Podman container worker (`templates/usnm_qwen_in`, models baked into image).
    #[default]
    #[value(name = "container")]
    Container,
    /// Native 8B GPU worker (`templates/usnm_qwen_8b_in`, stages sticky 8B GGUF + mmproj).
    #[value(name = "native-8b", alias = "native8b")]
    Native8b,
    /// Native 2B GPU/CPU worker (`templates/usnm_qwen_2b_in`, stages sticky 2B GGUF + mmproj).
    #[value(name = "native-2b", alias = "native2b")]
    Native2b,
}

impl Target {
    pub fn wu_template(self) -> &'static str {
        match self {
            Self::Container => "templates/usnm_qwen_in",
            Self::Native8b => "templates/usnm_qwen_8b_in",
            Self::Native2b => "templates/usnm_qwen_2b_in",
        }
    }

    /// BOINC `workunit.batch` identifier passed via `create_work --batch` so
    /// `sched/plan_class_spec.cpp` (`wu_is_infeasible_for_plan_class`) binds each
    /// Qwen target exclusively to compatible app-version plan classes:
    /// - `1` (`Container`): `docker*` (`docker_mt`, `docker_nvidia_cuda`)
    /// - `2` (`Native2b`): native 2B/CPU plan classes (`mt`, `nvidia_cuda_2b`)
    /// - `8` (`Native8b`): native 8B GPU plan classes (`apple_gpu_8b`, `nvidia_cuda_8b`)
    pub fn batch(self) -> i32 {
        match self {
            Self::Container => 1,
            Self::Native2b => 2,
            Self::Native8b => 8,
        }
    }

    pub fn batch_str(self) -> &'static str {
        match self {
            Self::Container => "1",
            Self::Native2b => "2",
            Self::Native8b => "8",
        }
    }

    /// Returns whether `plan_class` is eligible to execute workunits created for `self`.
    pub fn is_plan_class_compatible(self, plan_class: &str) -> bool {
        match self {
            Self::Container => plan_class == "docker" || plan_class.starts_with("docker_"),
            Self::Native8b => !plan_class.starts_with("docker") && plan_class.ends_with("_8b"),
            Self::Native2b => {
                !plan_class.starts_with("docker")
                    && (plan_class == "mt" || plan_class.ends_with("_2b"))
            }
        }
    }

    pub fn model_files(self) -> &'static [&'static str] {
        match self {
            Self::Container => &[],
            Self::Native8b => &[
                "Qwen3VL-8B-Instruct-Q4_K_M.gguf",
                "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf",
            ],
            Self::Native2b => &[
                "Qwen3-VL-2B-Instruct-Q8_0.gguf",
                "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub project: PathBuf,
    pub max_units: usize,
    pub target: Target,
    pub model_dir: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub submitted: usize,
    pub uncertain: usize,
}

/// Twelve crops through an 8B model at Q4: a few hundred prefill tokens
/// and a few hundred output tokens each, about 1e13 operations. UNVERIFIED
/// until the probe measures it; each crop has the unit's deadline anyway.
pub const FPOPS_EST: &str = "1e13";
pub const FPOPS_BOUND: &str = "1e16";
/// 6 GiB: the 8B model at Q4 needs about 7 GB of GPU memory and 4 GiB of
/// RAM; the 2B model at Q8 about 3 GiB of RAM (`ocr/boinc/qwen/`).
pub const MEMORY_BOUND: &str = "6442450944";

pub fn create_work_args(job: &Job) -> Vec<String> {
    create_work_args_for_target(job, Target::Container)
}

pub fn create_work_args_for_target(job: &Job, target: Target) -> Vec<String> {
    create_work_args_with_lock(job, target, EMBEDDED_WEIGHTS_LOCK)
        .expect("embedded weights.lock is valid")
}

pub fn create_work_args_with_lock(
    job: &Job,
    target: Target,
    weights_lock_text: &str,
) -> anyhow::Result<Vec<String>> {
    let mut args: Vec<String> = [
        "--appname",
        APP,
        "--wu_name",
        &job.workunit(),
        "--wu_template",
        target.wu_template(),
        "--result_template",
        "templates/usnm_qwen_out",
        "--batch",
        target.batch_str(),
        "--min_quorum",
        "2",
        "--target_nresults",
        "2",
        "--max_error_results",
        "3",
        "--max_total_results",
        "4",
        "--max_success_results",
        "3",
        "--rsc_fpops_est",
        FPOPS_EST,
        "--rsc_fpops_bound",
        FPOPS_BOUND,
        "--rsc_memory_bound",
        MEMORY_BOUND,
        "--rsc_disk_bound",
        "2147483648",
        "--delay_bound",
        "259200",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(workunit_physical_names_with_lock(
        job,
        target,
        weights_lock_text,
    )?);
    Ok(args)
}

/// Physical names in the download tree for a per-unit staging step, in input
/// template order for container workunits, with the unit file each comes from.
pub fn physical_names(job: &Job) -> Vec<(String, &'static str)> {
    vec![
        (format!("{}_page_jpg", job.id), PAGE_FILE),
        (format!("{}_qwen_json", job.id), JOB_FILE),
    ]
}

/// Digest-qualified physical name for a shared sticky model file in the BOINC
/// download tree so different `weights.lock` revisions coexist without colliding
/// on server URLs or client caches, while keeping the template's `<open_name>`
/// logical basename unchanged.
pub fn model_physical_name(logical: &str, sha256: &str) -> String {
    format!("usnm_qwen_{sha256}_{logical}")
}

/// All physical file names passed to `create_work` in input template order
/// (`<id>_page_jpg`, `<id>_qwen_json`, and optional digest-qualified shared
/// sticky `.gguf` and `.mmproj`).
pub fn workunit_physical_names(job: &Job, target: Target) -> Vec<String> {
    workunit_physical_names_with_lock(job, target, EMBEDDED_WEIGHTS_LOCK)
        .expect("embedded weights.lock is valid")
}

pub fn workunit_physical_names_with_lock(
    job: &Job,
    target: Target,
    weights_lock_text: &str,
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::with_capacity(2 + target.model_files().len());
    out.push(format!("{}_page_jpg", job.id));
    out.push(format!("{}_qwen_json", job.id));
    if !target.model_files().is_empty() {
        let digests = parse_weights_lock(weights_lock_text)?;
        for &model_file in target.model_files() {
            let sha = digests
                .get(model_file)
                .with_context(|| format!("missing {model_file} in weights.lock"))?;
            out.push(model_physical_name(model_file, sha));
        }
    }
    Ok(out)
}

pub fn parse_weights_lock(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        ensure!(parts.len() >= 3, "invalid weights.lock line: {trimmed}");
        map.insert(parts[0].to_string(), parts[2].to_ascii_lowercase());
    }
    ensure!(!map.is_empty(), "weights.lock contained no model digests");
    Ok(map)
}

fn sha256_file_streaming(path: &Path) -> anyhow::Result<String> {
    let mut reader = std::fs::File::open(path)
        .with_context(|| format!("opening file for sha256 {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("reading file for sha256 {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Streams `src` through a SHA-256 hasher into a temporary file in `dir_hier_path`'s
/// directory, verifies the digest against `expected_sha256`, and atomically publishes
/// the file with `0o664` permissions without loading the multi-GiB GGUF into RAM.
/// If `dst` already exists in the download hierarchy with the expected digest,
/// reuses it in place without copying or rewriting.
async fn stage_shared_model_streaming(
    project: &Path,
    physical: &str,
    src: &Path,
    expected_sha256: &str,
) -> anyhow::Result<()> {
    let output = Command::new(project.join("bin/dir_hier_path"))
        .arg(physical)
        .current_dir(project)
        .output()
        .await?;
    ensure!(output.status.success(), "dir_hier_path failed");
    let rel_or_abs = String::from_utf8(output.stdout)?;
    let rel_or_abs = Path::new(rel_or_abs.trim());
    ensure!(!rel_or_abs.as_os_str().is_empty(), "empty staging path");
    let dst = if rel_or_abs.is_absolute() {
        rel_or_abs.to_owned()
    } else {
        project.join(rel_or_abs)
    };
    if dst.is_file() && sha256_file_streaming(&dst).is_ok_and(|d| d == expected_sha256) {
        return Ok(());
    }
    let parent = dst.parent().context("download path has no parent")?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let download_root = project.join("download");
        let mut dir = parent;
        while dir.starts_with(&download_root) {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o2775))?;
            if dir == download_root {
                break;
            }
            let Some(next) = dir.parent() else { break };
            dir = next;
        }
    }

    let mut reader = std::fs::File::open(src)
        .with_context(|| format!("opening model file {}", src.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temp file in {}", parent.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("reading model file {}", src.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        temp.write_all(&buf[..n])
            .with_context(|| format!("writing staged model {}", dst.display()))?;
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    ensure!(
        actual == expected_sha256,
        "model file {} sha256 mismatch against weights.lock: expected {}, got {}",
        src.display(),
        expected_sha256,
        actual
    );
    temp.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o664))?;
    }
    temp.persist(&dst)
        .map_err(|e| e.error)
        .with_context(|| format!("publishing {}", dst.display()))?;
    Ok(())
}

async fn stage_shared_models_with_lock(
    project: &Path,
    model_dir: Option<&Path>,
    target: Target,
    weights_lock_text: &str,
) -> anyhow::Result<()> {
    if target == Target::Container {
        return Ok(());
    }
    let dir = model_dir.unwrap_or_else(|| Path::new(MODEL_DIR));
    let expected_digests = parse_weights_lock(weights_lock_text)?;
    for &file in target.model_files() {
        let expected = expected_digests
            .get(file)
            .with_context(|| format!("missing {file} in weights.lock"))?;
        let physical = model_physical_name(file, expected);
        let path = dir.join(file);
        stage_shared_model_streaming(project, &physical, &path, expected).await?;
    }
    Ok(())
}

pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    run_with_lock(opts, EMBEDDED_WEIGHTS_LOCK).await
}

pub async fn run_with_lock(opts: &Options, weights_lock_text: &str) -> anyhow::Result<Summary> {
    let project = opts.project.canonicalize()?;
    let dir = units(&opts.root);
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {dir:?}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join(JOB_FILE).exists())
        .map(|e| e.path())
        .collect();
    names.sort();
    let mut summary = Summary::default();
    let mut models_staged = false;
    for unit in names {
        if summary.submitted >= opts.max_units {
            break;
        }
        if unit.join("submitted").exists() {
            continue;
        }
        if unit.join("submitting").exists() {
            summary.uncertain += 1;
            continue;
        }
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)?;
        job.validate()?;
        let page = read(&unit.join(PAGE_FILE), MAX_SCAN)?;
        ensure!(
            job.input.matches(&page),
            "staged page changed for {}",
            job.doc_id
        );
        if !models_staged {
            stage_shared_models_with_lock(
                &project,
                opts.model_dir.as_deref(),
                opts.target,
                weights_lock_text,
            )
            .await?;
            models_staged = true;
        }
        atomic(&unit.join("submitting"), job.workunit().as_bytes())?;
        for (physical, file) in physical_names(&job) {
            let limit = if file == JOB_FILE {
                MAX_JOB_JSON
            } else {
                MAX_SCAN
            };
            crate::assess::submit::stage_file(&project, &physical, &read(&unit.join(file), limit)?)
                .await?;
        }
        let status = Command::new(project.join("bin/create_work"))
            .current_dir(&project)
            .args(create_work_args_with_lock(
                &job,
                opts.target,
                weights_lock_text,
            )?)
            .status()
            .await?;
        ensure!(
            status.success(),
            "create_work failed for {}; {} is uncertain until checked",
            job.workunit(),
            unit.display()
        );
        std::fs::rename(unit.join("submitting"), unit.join("submitted"))?;
        summary.submitted += 1;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_ocr_contract::boinc::sha256;

    #[test]
    fn two_inputs_in_template_order() {
        let tmp = tempfile::tempdir().unwrap();
        let unit = tmp.path().join("q");
        crate::quality::work::tests::unit(&unit);
        let j = super::super::stage::local_unit(
            &unit.join("page.jpg"),
            &unit.join("loc.xml"),
            &tmp.path().join("u"),
            super::super::stage::tests::engine(),
            1,
            4,
            600,
            4,
        )
        .unwrap();
        let names = physical_names(&j);
        assert_eq!(names[0].1, PAGE_FILE);
        assert_eq!(names[1].1, JOB_FILE);
        let args = create_work_args(&j);
        assert!(args.contains(&"templates/usnm_qwen_in".to_string()));
        assert!(args.windows(2).any(|w| w == ["--batch", "1"]));
        assert!(args.ends_with(&[names[0].0.clone(), names[1].0.clone()]));
    }

    #[test]
    fn target_plan_class_compatibility_rejects_incompatible_assignments() {
        assert!(Target::Container.is_plan_class_compatible("docker_mt"));
        assert!(Target::Container.is_plan_class_compatible("docker_nvidia_cuda"));
        assert!(!Target::Container.is_plan_class_compatible("mt"));
        assert!(!Target::Container.is_plan_class_compatible("apple_gpu_8b"));
        assert!(!Target::Container.is_plan_class_compatible("nvidia_cuda_2b"));

        assert!(Target::Native8b.is_plan_class_compatible("apple_gpu_8b"));
        assert!(Target::Native8b.is_plan_class_compatible("nvidia_cuda_8b"));
        assert!(!Target::Native8b.is_plan_class_compatible("mt"));
        assert!(!Target::Native8b.is_plan_class_compatible("nvidia_cuda_2b"));
        assert!(!Target::Native8b.is_plan_class_compatible("docker_nvidia_cuda"));

        assert!(Target::Native2b.is_plan_class_compatible("mt"));
        assert!(Target::Native2b.is_plan_class_compatible("nvidia_cuda_2b"));
        assert!(!Target::Native2b.is_plan_class_compatible("apple_gpu_8b"));
        assert!(!Target::Native2b.is_plan_class_compatible("nvidia_cuda_8b"));
        assert!(!Target::Native2b.is_plan_class_compatible("docker_mt"));
    }

    #[test]
    fn native_targets_select_model_templates_and_four_physical_names() {
        let tmp = tempfile::tempdir().unwrap();
        let unit = tmp.path().join("q");
        crate::quality::work::tests::unit(&unit);
        let j = super::super::stage::local_unit(
            &unit.join("page.jpg"),
            &unit.join("loc.xml"),
            &tmp.path().join("u"),
            super::super::stage::tests::engine(),
            1,
            4,
            600,
            4,
        )
        .unwrap();

        let digests = parse_weights_lock(EMBEDDED_WEIGHTS_LOCK).unwrap();
        let args_8b = create_work_args_for_target(&j, Target::Native8b);
        assert!(args_8b.contains(&"templates/usnm_qwen_8b_in".to_string()));
        assert!(args_8b.windows(2).any(|w| w == ["--batch", "8"]));
        assert!(args_8b.ends_with(&[
            format!("{}_page_jpg", j.id),
            format!("{}_qwen_json", j.id),
            model_physical_name(
                "Qwen3VL-8B-Instruct-Q4_K_M.gguf",
                digests.get("Qwen3VL-8B-Instruct-Q4_K_M.gguf").unwrap(),
            ),
            model_physical_name(
                "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf",
                digests.get("mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf").unwrap(),
            ),
        ]));

        let args_2b = create_work_args_for_target(&j, Target::Native2b);
        assert!(args_2b.contains(&"templates/usnm_qwen_2b_in".to_string()));
        assert!(args_2b.windows(2).any(|w| w == ["--batch", "2"]));
        assert!(args_2b.ends_with(&[
            format!("{}_page_jpg", j.id),
            format!("{}_qwen_json", j.id),
            model_physical_name(
                "Qwen3-VL-2B-Instruct-Q8_0.gguf",
                digests.get("Qwen3-VL-2B-Instruct-Q8_0.gguf").unwrap(),
            ),
            model_physical_name(
                "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
                digests
                    .get("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf")
                    .unwrap(),
            ),
        ]));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn stage_shared_models_stages_verified_gguf_and_rejects_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let bin_dir = project.join("bin");
        let download_dir = project.join("download");
        let model_dir = tmp.path().join("models");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&download_dir).unwrap();
        std::fs::create_dir_all(&model_dir).unwrap();

        let dir_hier = bin_dir.join("dir_hier_path");
        std::fs::write(&dir_hier, "#!/bin/sh\necho \"download/$1\"\n").unwrap();
        std::fs::set_permissions(&dir_hier, std::fs::Permissions::from_mode(0o755)).unwrap();

        let gguf_8b = b"synthetic-8b-gguf";
        let mmproj_8b = b"synthetic-8b-mmproj";
        let gguf_2b = b"synthetic-2b-gguf";
        let mmproj_2b = b"synthetic-2b-mmproj";
        std::fs::write(model_dir.join("Qwen3VL-8B-Instruct-Q4_K_M.gguf"), gguf_8b).unwrap();
        std::fs::write(
            model_dir.join("mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf"),
            mmproj_8b,
        )
        .unwrap();
        std::fs::write(model_dir.join("Qwen3-VL-2B-Instruct-Q8_0.gguf"), gguf_2b).unwrap();
        std::fs::write(
            model_dir.join("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf"),
            mmproj_2b,
        )
        .unwrap();

        // Embedded weights.lock rejects synthetic test bytes whose SHA-256 differs from production.
        let err = stage_shared_models_with_lock(
            &project,
            Some(&model_dir),
            Target::Native8b,
            EMBEDDED_WEIGHTS_LOCK,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("sha256 mismatch"));

        // Matching lock stages both 8B and 2B sticky files into download/ under digest-qualified names.
        let sha_8b = sha256(gguf_8b);
        let sha_8b_mm = sha256(mmproj_8b);
        let sha_2b = sha256(gguf_2b);
        let sha_2b_mm = sha256(mmproj_2b);
        let lock = format!(
            "Qwen3VL-8B-Instruct-Q4_K_M.gguf https://example.invalid/8b.gguf {sha_8b}\n\
             mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf https://example.invalid/8b.mmproj {sha_8b_mm}\n\
             Qwen3-VL-2B-Instruct-Q8_0.gguf https://example.invalid/2b.gguf {sha_2b}\n\
             mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf https://example.invalid/2b.mmproj {sha_2b_mm}\n",
        );
        stage_shared_models_with_lock(&project, Some(&model_dir), Target::Native8b, &lock)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(download_dir.join(model_physical_name(
                "Qwen3VL-8B-Instruct-Q4_K_M.gguf",
                &sha_8b
            )))
            .unwrap(),
            gguf_8b
        );
        assert_eq!(
            std::fs::read(download_dir.join(model_physical_name(
                "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf",
                &sha_8b_mm
            )))
            .unwrap(),
            mmproj_8b
        );

        stage_shared_models_with_lock(&project, Some(&model_dir), Target::Native2b, &lock)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(download_dir.join(model_physical_name(
                "Qwen3-VL-2B-Instruct-Q8_0.gguf",
                &sha_2b
            )))
            .unwrap(),
            gguf_2b
        );
        assert_eq!(
            std::fs::read(download_dir.join(model_physical_name(
                "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
                &sha_2b_mm
            )))
            .unwrap(),
            mmproj_2b
        );

        // Existing verified downloads are reused in place without reading/copying from model_dir.
        std::fs::remove_dir_all(&model_dir).unwrap();
        stage_shared_models_with_lock(&project, Some(&model_dir), Target::Native2b, &lock)
            .await
            .unwrap();

        // When no work is pending (empty units directory or max_units == 0), run_with_lock
        // does not stage models at all.
        let campaign_root = tmp.path().join("campaign");
        std::fs::create_dir_all(units(&campaign_root)).unwrap();
        let empty_summary = run_with_lock(
            &Options {
                root: campaign_root,
                project: project.clone(),
                max_units: 10,
                target: Target::Native8b,
                model_dir: Some(tmp.path().join("nonexistent-models")),
            },
            &lock,
        )
        .await
        .unwrap();
        assert_eq!(empty_summary.submitted, 0);
    }
}
