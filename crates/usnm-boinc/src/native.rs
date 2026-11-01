//! The native `usnm_ocr` and `usnm_gold` apps (#1418): the page contract of the worker image,
//! in a BOINC slot, with no container runtime. docs/boinc-native-ocr.md has the design.
//!
//! A task is three processes, so the client's control channel keeps working while pages are
//! read and a stop reaches everything at once:
//!
//! - [`run_slot`], which the client starts, checks the build and the unit, keeps the channel,
//!   and writes `boinc_finish_called` when the unit is done.
//! - [`run_unit`], in its own process group, is `usnm-boinc work`: the same [`crate::worker`]
//!   code, so a page, a bundle and a gold unit read and write what they do in the container.
//! - [`exec_engine`] is what `tesseract` resolves to in the unit. It sets the limits the
//!   container's `job.toml` gets from its cgroup and execs the bundled engine.
//!
//! A job names its build by `engine.image`, and host trust is keyed to that digest, so this
//! worker refuses any job that doesn't name exactly this build. Here the digest is the sha256
//! of the build manifest, which lists every file of the build.
use crate::{gold, worker::CPU_DEADLINE_ENV};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use usnm_ocr_contract::boinc::{self, bundle, is_sha256, Job};
use usnm_ocr_quality::slot::{
    resolve_slot_file, write_atomic, write_mmap_channel, BoincControl, ChildAction,
    APP_STATUS_OFFSET, FINISH_FILE, FRACTION_FILE, MMAP_FILE,
};
use usnm_ocr_worker::engine;

/// The manifest's open name in the slot.
pub const MANIFEST_FILE: &str = "usnm_native_build.json";
/// The bundled engine's open name in the slot.
pub const TESSERACT_FILE: &str = "usnm_tesseract";
/// The worker's name in the manifest.
pub const WORKER_FILE: &str = "usnm_ocr";
/// A model's open name is this followed by its file name.
pub const MODEL_PREFIX: &str = "tessdata_";
pub const MANIFEST_VERSION: u32 = 1;
const MAX_MANIFEST: usize = 64 << 10;
const MAX_BUILD_FILE: u64 = 256 << 20;

/// What the container's `job.toml` allows (`--memory=1536m --memory-swap=1536m`). Here it bounds
/// address space, which is stricter than the cgroup's resident limit.
pub const MEMORY_BYTES: u64 = 1536 << 20;
/// The container's scratch tmpfs (`--tmpfs=/tmp:rw,size=512m`), as a limit on any one file.
pub const FILE_BYTES: u64 = 512 << 20;
const OPEN_FILES: u64 = 256;
/// Where the slot holds the engine's view of the build.
const ENGINE_DIR: &str = "usnm_engine";
const TMP_DIR: &str = "usnm_tmp";
const REAL_TESSERACT_ENV: &str = "USNM_TESSERACT_REAL";
/// These two carry the output configs the engine image copies from Debian's package.
const CONFIGS: [(&str, &str); 2] = [
    ("alto", "tessedit_create_alto 1\n"),
    (
        "txt",
        "# This config file should be used with other config files which create renderers.\n\
         # usage example: tesseract eurotext.tif eurotext txt hocr pdf\n\
         tessedit_create_txt 1\n",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildFile {
    pub bytes: u64,
    pub sha256: String,
}

/// `usnm_native_build.json`: every file of a build, by open name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub engine_version: String,
    pub files: BTreeMap<String, BuildFile>,
    pub platform: String,
    pub v: u32,
}

/// A build's identity, as a job and the trust list name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// `sha256:<manifest digest>`, the job's `engine.image`.
    pub image: String,
    /// `sha256:<engine digest>`, a gold job's `engine.base`.
    pub base: String,
    pub manifest: Manifest,
}

/// The locked models, by open name.
fn locked_models() -> anyhow::Result<BTreeMap<String, String>> {
    let mut models = BTreeMap::new();
    for line in engine::MODELS_LOCK.lines().filter(|l| !l.starts_with('#')) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if let [name, _, sum] = fields[..] {
            let file = name
                .strip_prefix("tessdata/")
                .context("model outside tessdata")?;
            models.insert(format!("{MODEL_PREFIX}{file}"), sum.to_owned());
        }
    }
    ensure!(!models.is_empty(), "no models in the lock");
    Ok(models)
}

impl Manifest {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.v == MANIFEST_VERSION, "unsupported build manifest");
        ensure!(
            self.platform == "x86_64-pc-linux-gnu",
            "unsupported build platform"
        );
        for (name, file) in &self.files {
            ensure!(
                is_sha256(&file.sha256) && (1..=MAX_BUILD_FILE).contains(&file.bytes),
                "invalid build file {name}"
            );
        }
        for name in [WORKER_FILE, TESSERACT_FILE] {
            ensure!(self.files.contains_key(name), "the build has no {name}");
        }
        let models = locked_models()?;
        ensure!(
            self.files.len() == models.len() + 2,
            "the build's files aren't the locked set"
        );
        for (name, sum) in models {
            ensure!(
                self.files.get(&name).is_some_and(|f| f.sha256 == sum),
                "the build's {name} isn't the locked model"
            );
        }
        Ok(())
    }

    fn build(self, bytes: &[u8]) -> anyhow::Result<Build> {
        self.validate()?;
        Ok(Build {
            image: format!("sha256:{}", boinc::sha256(bytes)),
            base: format!("sha256:{}", self.files[TESSERACT_FILE].sha256),
            manifest: self,
        })
    }
}

/// The size and sha256 of a file of the build.
fn digest(path: &Path) -> anyhow::Result<BuildFile> {
    let (bytes, sha256) = gold::work::sha256_file(path, MAX_BUILD_FILE)?;
    Ok(BuildFile { bytes, sha256 })
}

/// Writes the build's manifest. Refuses a model that isn't the locked one, and an engine that
/// isn't the locked engine version.
pub fn write_manifest(
    worker: &Path,
    tesseract: &Path,
    tessdata: &Path,
    out: &Path,
) -> anyhow::Result<Build> {
    let mut files = BTreeMap::new();
    files.insert(WORKER_FILE.to_owned(), digest(worker)?);
    files.insert(TESSERACT_FILE.to_owned(), digest(tesseract)?);
    for name in locked_models()?.keys() {
        let file = name.strip_prefix(MODEL_PREFIX).context("model name")?;
        files.insert(name.clone(), digest(&tessdata.join(file))?);
    }
    let version = Command::new(tesseract).arg("--version").output()?;
    let version = String::from_utf8_lossy(&version.stdout);
    let engine_version = version
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("tesseract "))
        .context("can't read the engine's version")?
        .trim()
        .to_owned();
    let manifest = Manifest {
        engine_version,
        files,
        platform: "x86_64-pc-linux-gnu".into(),
        v: MANIFEST_VERSION,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    let build = manifest.build(&bytes)?;
    std::fs::write(out, &bytes)?;
    Ok(build)
}

/// The kinds of unit this worker runs. Every other job file is refused, so a unit of another
/// app can't reach the shared dispatch in [`crate::worker::run`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    /// The job's `engine.image`.
    image: String,
    /// The job's `engine.base`, for a gold unit.
    base: Option<String>,
    /// The deadline of one read.
    timeout_secs: u32,
    /// A gold unit reports wall-clock crop times and its validator bounds them, so it starts
    /// again after a suspension rather than stopping mid-crop.
    restart_after_suspend: bool,
}

fn read_unit(work: &Path) -> anyhow::Result<Unit> {
    for other in [
        crate::assess::JOB_FILE,
        crate::surya::JOB_FILE,
        crate::quality::JOB_FILE,
        crate::qwen::JOB_FILE,
        crate::ndlocr::JOB_FILE,
    ] {
        ensure!(
            !work.join(other).exists(),
            "{other} is not a unit of this app"
        );
    }
    if work.join(gold::job::JOB_FILE).exists() {
        let job: gold::job::Job = serde_json::from_slice(&crate::read(
            &work.join(gold::job::JOB_FILE),
            gold::job::MAX_JSON,
        )?)?;
        job.validate()?;
        return Ok(Unit {
            image: job.engine.image,
            base: Some(job.engine.base),
            timeout_secs: job.engine.timeout_secs,
            restart_after_suspend: true,
        });
    }
    if work.join(bundle::JOB_FILE).exists() {
        let unit = crate::bundle::work::load(work)?;
        let engine = &unit.pages.first().context("empty bundle")?.job.engine;
        return Ok(Unit {
            image: engine.image.clone(),
            base: None,
            timeout_secs: engine.timeout_secs,
            restart_after_suspend: false,
        });
    }
    let job: Job =
        serde_json::from_slice(&crate::read(&work.join("request.json"), boinc::MAX_JSON)?)?;
    job.validate()?;
    Ok(Unit {
        image: job.engine.image,
        base: None,
        timeout_secs: job.engine.timeout_secs,
        restart_after_suspend: false,
    })
}

/// A job for another build is refused here, before anything runs: its result would carry trust
/// that belongs to the build it names.
fn check_unit(unit: &Unit, build: &Build) -> anyhow::Result<()> {
    ensure!(
        unit.image == build.image,
        "the unit was made for build {}, this is {}",
        unit.image,
        build.image
    );
    if let Some(base) = &unit.base {
        ensure!(
            *base == build.base,
            "the unit was made for engine {base}, this build has {}",
            build.base
        );
    }
    Ok(())
}

/// Checks every file of the build against the manifest in the slot.
fn load_build(work: &Path) -> anyhow::Result<(Build, BTreeMap<String, PathBuf>)> {
    let bytes = crate::read_following(&resolve_slot_file(work, MANIFEST_FILE), MAX_MANIFEST)?;
    let manifest: Manifest = serde_json::from_slice(&bytes).context(MANIFEST_FILE)?;
    let build = manifest.build(&bytes)?;
    let mut paths = BTreeMap::new();
    for (name, want) in &build.manifest.files {
        let path = if name == WORKER_FILE {
            std::env::current_exe()?
        } else {
            let path = resolve_slot_file(work, name);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()?.join(path)
            }
        };
        let got = digest(&path).with_context(|| format!("the build's {name}"))?;
        ensure!(got == *want, "the build's {name} differs from its manifest");
        paths.insert(name.clone(), path);
    }
    Ok((build, paths))
}

/// A regular file at `to` with the bytes of `from`, without a copy where the slot and the
/// project directory share a filesystem. `crate::read` refuses symlinks, and it reads models.
fn link_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    if std::fs::hard_link(from, to).is_err() {
        std::fs::copy(from, to).with_context(|| format!("copying {}", from.display()))?;
    }
    Ok(())
}

/// Lays out the engine in the slot: the models and configs as `TESSDATA_PREFIX` expects them,
/// the engine, and a `tesseract` link back to this executable.
fn prepare_engine(
    work: &Path,
    paths: &BTreeMap<String, PathBuf>,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = work.join(ENGINE_DIR);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    let tessdata = dir.join("tessdata");
    std::fs::create_dir_all(tessdata.join("configs"))?;
    for (name, path) in paths {
        if let Some(file) = name.strip_prefix(MODEL_PREFIX) {
            link_file(path, &tessdata.join(file))?;
        }
    }
    for (name, text) in CONFIGS {
        std::fs::write(tessdata.join("configs").join(name), text)?;
    }
    let real = dir.join(TESSERACT_FILE);
    link_file(&paths[TESSERACT_FILE], &real)?;
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755))?;
    std::os::unix::fs::symlink(&paths[WORKER_FILE], dir.join("tesseract"))?;
    Ok((dir, real))
}

fn control(work: &Path) -> BoincControl {
    let control = BoincControl::start(work);
    control.set_child_hook(Some(std::sync::Arc::new(|pgid, action| {
        if let Some(pid) = rustix::process::Pid::from_raw(pgid as i32) {
            let signal = match action {
                ChildAction::Suspend => rustix::process::Signal::STOP,
                ChildAction::Resume => rustix::process::Signal::CONT,
                ChildAction::Terminate => rustix::process::Signal::TERM,
            };
            let _ = rustix::process::kill_process_group(pid, signal);
            if action == ChildAction::Terminate {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::CONT);
            }
        }
    })));
    control
}

fn kill_group(child: &mut Child) -> std::io::Result<()> {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    child.wait().map(|_| ())
}

/// Tells the client how far the unit is, from the `fraction_done` the worker writes.
struct Progress {
    channel: Option<PathBuf>,
    fraction: PathBuf,
    sent: String,
    last: Instant,
}

impl Progress {
    fn new(work: &Path) -> Self {
        Self {
            channel: work
                .join("init_data.xml")
                .exists()
                .then(|| work.join(MMAP_FILE)),
            fraction: work.join(FRACTION_FILE),
            sent: String::new(),
            last: Instant::now(),
        }
    }

    fn report(&mut self) {
        let Some(channel) = &self.channel else { return };
        if self.last.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last = Instant::now();
        if let Ok(text) = std::fs::read_to_string(&self.fraction) {
            let text = text.trim();
            if text != self.sent && text.parse::<f64>().is_ok() {
                let xml = format!("<fraction_done>{text}</fraction_done>\n");
                if write_mmap_channel(channel, APP_STATUS_OFFSET, &xml).is_ok() {
                    self.sent = text.to_owned();
                }
            }
        }
    }
}

/// How the unit ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Finished,
    /// The client asked the task to stop: exit without `boinc_finish_called`.
    Stopped(&'static str),
}

fn supervise(
    work: &Path,
    start: &dyn Fn() -> anyhow::Result<Child>,
    control: &BoincControl,
    restart_after_suspend: bool,
) -> anyhow::Result<Ended> {
    let mut progress = Progress::new(work);
    loop {
        if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
            return Ok(Ended::Stopped(reason));
        }
        let mut child = start()?;
        control.set_child_pgid(Some(child.id()));
        loop {
            if let Some(status) = child.try_wait()? {
                control.set_child_pgid(None);
                if let Some(reason) = control.should_exit() {
                    return Ok(Ended::Stopped(reason));
                }
                ensure!(status.success(), "the unit failed: {status}");
                return Ok(Ended::Finished);
            }
            if let Some(reason) = control.should_exit() {
                kill_group(&mut child)?;
                control.set_child_pgid(None);
                return Ok(Ended::Stopped(reason));
            }
            if restart_after_suspend && control.is_suspended() {
                kill_group(&mut child)?;
                control.set_child_pgid(None);
                break;
            }
            progress.report();
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// One task in the slot `work`: the process the BOINC client starts.
pub fn run_slot(work: &Path) -> anyhow::Result<()> {
    let work = work.canonicalize()?;
    let control = control(&work);
    let unit = read_unit(&work)?;
    let (build, paths) = load_build(&work)?;
    check_unit(&unit, &build)?;
    let (engine_dir, real) = prepare_engine(&work, &paths)?;
    let tmp = work.join(TMP_DIR);
    std::fs::create_dir_all(&tmp)?;
    let worker = paths[WORKER_FILE].clone();
    let start = || -> anyhow::Result<Child> {
        Ok(Command::new(&worker)
            .arg("unit")
            .current_dir(&work)
            .env_clear()
            .env("PATH", &engine_dir)
            .env("TESSDATA_PREFIX", engine_dir.join("tessdata"))
            .env("OMP_THREAD_LIMIT", "1")
            .env("TMPDIR", &tmp)
            .env("USNM_TESSERACT_IMAGE", &build.base)
            .env(REAL_TESSERACT_ENV, &real)
            .env(CPU_DEADLINE_ENV, unit.timeout_secs.to_string())
            .process_group(0)
            .stdin(Stdio::null())
            .spawn()?)
    };
    match supervise(&work, &start, &control, unit.restart_after_suspend)? {
        Ended::Stopped(reason) => eprintln!("stopped by the client: {reason}"),
        Ended::Finished => {
            let _ = std::fs::remove_dir_all(work.join(ENGINE_DIR));
            let _ = std::fs::remove_dir_all(&tmp);
            write_atomic(&work.join(FINISH_FILE), b"0\n")?;
        }
    }
    Ok(())
}

/// The unit, in the child [`run_slot`] starts: what `usnm-boinc work` does in the container.
pub async fn run_unit(work: &Path) -> anyhow::Result<()> {
    // The client can kill the parent alone; nothing here may outlive it.
    let parent = rustix::process::getppid();
    std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if rustix::process::getppid() != parent {
                let _ = rustix::process::kill_process_group(
                    rustix::process::getpid(),
                    rustix::process::Signal::KILL,
                );
            }
        })?;
    crate::worker::run(work).await
}

/// Whether this process was started through the `tesseract` link.
pub fn is_engine_launch() -> bool {
    std::env::args_os()
        .next()
        .is_some_and(|arg| Path::new(&arg).file_name() == Some("tesseract".as_ref()))
}

fn limit(resource: rustix::process::Resource, soft: u64, hard: u64) -> anyhow::Result<()> {
    use rustix::process::{getrlimit, setrlimit, Rlimit};
    // A host's hard limit below ours stands: only root could raise it.
    let ceiling = getrlimit(resource).maximum.unwrap_or(u64::MAX);
    setrlimit(
        resource,
        Rlimit {
            current: Some(soft.min(ceiling)),
            maximum: Some(hard.min(ceiling)),
        },
    )
    .context("setting a resource limit")
}

/// Becomes the engine, under the limits of one read. Only returns on failure.
pub fn exec_engine() -> anyhow::Error {
    let setup = || -> anyhow::Result<std::ffi::OsString> {
        use rustix::process::Resource;
        let real = std::env::var_os(REAL_TESSERACT_ENV).context("no engine named")?;
        let cpu: u64 = std::env::var(CPU_DEADLINE_ENV)?.parse()?;
        rustix::thread::set_no_new_privs(true)?;
        // CPU time, not wall time: a suspended task stops its engine, and this stops with it.
        limit(Resource::Cpu, cpu, cpu + 5)?;
        limit(Resource::As, MEMORY_BYTES, MEMORY_BYTES)?;
        limit(Resource::Fsize, FILE_BYTES, FILE_BYTES)?;
        limit(Resource::Nofile, OPEN_FILES, OPEN_FILES)?;
        limit(Resource::Core, 0, 0)?;
        Ok(real)
    };
    match setup() {
        Ok(real) => Command::new(real)
            .args(std::env::args_os().skip(1))
            .exec()
            .into(),
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests;
