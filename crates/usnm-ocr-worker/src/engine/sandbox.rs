//! Shared process/container sandbox, runtime detection and scratch-slot cleanup.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{bail, Context};

use super::tail;

/// Where Tesseract runs: a binary on the host, or a pinned container image
/// run with podman (ADR-0015: EL9 has no Tesseract 5, so the node's comes as
/// an image). The engine contract (ADR-0016, #307) replaces this with a
/// long-lived instance per engine; until then each page is one container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    Host(PathBuf),
    Podman {
        /// The podman binary.
        podman: PathBuf,
        /// The engine runtime's Podman API socket, or none for podman run
        /// by the worker's own user (a developer machine).
        remote: Option<Remote>,
        /// `sha256:<id>` or `<name>@sha256:<digest>`: never a mutable tag.
        image: String,
    },
}

/// The engine runtime of ADR-0016 (13 §13.3): podman runs as the engine
/// account, `usnmeng`, behind a Podman API socket that the worker reaches
/// with `podman --remote`, so the worker itself never needs user
/// namespaces and its unit can set `NoNewPrivileges=yes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// `unix:///run/usnm-ocr/podman.sock`.
    pub url: String,
    /// The size-capped tmpfs that pages' scratch directories go in
    /// (`/run/usnm-ocr/scratch`). The worker's own `/tmp` is private to its
    /// unit, so the engine account's containers couldn't mount it.
    pub scratch: PathBuf,
    pub limits: Limits,
}

/// Limits on each engine container (13 §13.6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Limits {
    /// `--memory` and `--memory-swap`: no swap.
    pub memory_mib: u32,
    pub cpus: u32,
    pub pids: u32,
}

impl Default for Limits {
    /// Tesseract's starting entry in 13 §13.6.2.
    fn default() -> Self {
        Self {
            memory_mib: 1536,
            cpus: 1,
            pids: 64,
        }
    }
}

/// The user engine images run as (`USER 10001` in their Containerfiles).
/// Under the engine account's rootless podman it maps to one of that
/// account's subordinate ids, never to the account itself.
pub const ENGINE_USER: u32 = 10001;

/// The label on every container the worker starts, so stale ones can be
/// found.
const CONTAINER_LABEL: &str = "usnm.engine=tesseract";

/// What the worker learns from the engine runtime at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sandbox {
    pub(crate) podman: PathBuf,
    pub(crate) url: String,
    pub(super) image: String,
    pub(super) scratch: PathBuf,
    pub(crate) info: RuntimeInfo,
}

/// What `podman info` says about the engine runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeInfo {
    /// The host uid that [`ENGINE_USER`] maps to.
    pub(crate) engine_uid: u32,
    /// The host uid the runtime itself runs as (container uid 0: the
    /// engine account), which needs to reach a page's directory to mount
    /// it; none for rootful podman.
    pub(super) runtime_uid: Option<u32>,
    pub(crate) selinux: bool,
}

/// From `podman info --format json`: the host uids [`ENGINE_USER`] and
/// container root map to, and whether SELinux is on. Refuses a runtime that can't apply the
/// memory, CPU and pid limits (cgroup controllers not delegated to the
/// engine account), so limits never silently don't apply.
pub(super) fn parse_info(json: &str) -> anyhow::Result<RuntimeInfo> {
    let v: serde_json::Value = serde_json::from_str(json).context("podman info isn't JSON")?;
    let host = &v["host"];
    let controllers: Vec<&str> = host["cgroupControllers"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str()).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = ["memory", "pids", "cpu"]
        .into_iter()
        .filter(|c| !controllers.contains(c))
        .collect();
    if !missing.is_empty() {
        bail!(
            "the engine runtime's cgroup has no {} controller (it has {controllers:?}); \
             run scripts/ocr-node/node-setup.sh to delegate them",
            missing.join(", ")
        );
    }
    let maps = host["idMappings"]["uidmap"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let field = |m: &serde_json::Value, k: &str| m[k].as_u64().unwrap_or(0);
    let host_of = |user: u64| {
        maps.iter()
            .find(|m| {
                let c = field(m, "container_id");
                c <= user && user < c + field(m, "size")
            })
            .map(|m| field(m, "host_id") + (user - field(m, "container_id")))
            .and_then(|u| u32::try_from(u).ok())
    };
    let (engine_uid, runtime_uid) = if maps.is_empty() {
        // Rootful podman: no mapping.
        (ENGINE_USER, None)
    } else {
        let engine = host_of(u64::from(ENGINE_USER)).with_context(|| {
            format!("the engine account's subordinate ids don't cover uid {ENGINE_USER}")
        })?;
        (engine, host_of(0))
    };
    let selinux = host["security"]["selinuxEnabled"]
        .as_bool()
        .unwrap_or(false);
    Ok(RuntimeInfo {
        engine_uid,
        runtime_uid,
        selinux,
    })
}

/// Scratch slots, and with them MCS category pairs: slot n is
/// `slot-<n>` in the scratch directory, at level `s0:c<2n>,c<2n+1>`.
pub(super) const SLOTS: u32 = 512;

/// A page's slot directory, removed with everything in it when dropped.
#[derive(Debug)]
pub(crate) struct PageDir {
    pub(crate) path: PathBuf,
    pub(super) slot: u32,
}

impl Drop for PageDir {
    /// What [`Sandbox::release`] didn't remove (a page cut short).
    fn drop(&mut self) {
        if !self.path.exists() {
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(target: "usnm_ocr_worker::engine", dir = %self.path.display(), error = %e, "couldn't remove a page's scratch directory");
        }
    }
}

/// The MCS level of slot `slot` (13 §13.3).
fn slot_level(slot: u32) -> String {
    format!("s0:c{},c{}", 2 * slot, 2 * slot + 1)
}

impl PageDir {
    pub(crate) fn level(&self) -> String {
        slot_level(self.slot)
    }
}

/// Claims a free slot in `scratch`, mode 0700. The claim is the directory
/// itself (`mkdir` fails if it exists), so every process on the node that
/// shares the scratch, a worker and an install's `check-engine` alike,
/// gets a different slot, and so a different MCS pair, for each page in
/// flight.
pub(super) fn claim_slot(scratch: &Path) -> anyhow::Result<PageDir> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let start = NEXT
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(std::process::id());
    for i in 0..SLOTS {
        let slot = start.wrapping_add(i) % SLOTS;
        let path = scratch.join(format!("slot-{slot}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Ok(PageDir { path, slot }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("creating {}", path.display()));
            }
        }
    }
    bail!(
        "all {SLOTS} scratch slots in {} are in use",
        scratch.display()
    )
}

/// Removes slots older than `age`: left by a worker that was killed. No
/// page runs that long. Returns the ones it couldn't remove, with their
/// slot numbers.
pub(super) fn remove_stale_slots(scratch: &Path, age: Duration) -> Vec<(PathBuf, u32)> {
    let mut left = Vec::new();
    let Ok(entries) = std::fs::read_dir(scratch) else {
        return left;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().starts_with("slot-") {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d > age);
        if old && std::fs::remove_dir_all(e.path()).is_err() {
            if let Some(slot) = name
                .to_string_lossy()
                .strip_prefix("slot-")
                .and_then(|n| n.parse().ok())
            {
                left.push((e.path(), slot));
            }
        }
    }
    left
}

/// The owner and group of `path`.
pub(super) fn owner(path: &Path) -> anyhow::Result<(u32, u32)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path)?;
        Ok((meta.uid(), meta.gid()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        bail!("the engine runtime needs Unix")
    }
}

/// Runs a short helper (`setfacl`, `chcon`) and fails with its stderr.
pub(crate) async fn checked(cmd: &mut tokio::process::Command) -> anyhow::Result<()> {
    let out = cmd
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("running {:?}", cmd.as_std().get_program()))?;
    if !out.status.success() {
        bail!(
            "{:?} exited with {}: {}",
            cmd.as_std().get_program(),
            out.status,
            tail(&String::from_utf8_lossy(&out.stderr), 400)
        );
    }
    Ok(())
}

impl Sandbox {
    pub(crate) async fn detect(
        podman: &Path,
        remote: &Remote,
        image: &str,
    ) -> anyhow::Result<Self> {
        let meta = std::fs::metadata(&remote.scratch)
            .with_context(|| format!("scratch directory {}", remote.scratch.display()))?;
        if !meta.is_dir() {
            bail!("scratch {} isn't a directory", remote.scratch.display());
        }
        let out = podman_command(podman, Some(&remote.url))
            .args(["info", "--format", "json"])
            .kill_on_drop(true)
            .output()
            .await
            .context("running podman info")?;
        if !out.status.success() {
            bail!(
                "podman info over {} exited with {}: {}",
                remote.url,
                out.status,
                tail(&String::from_utf8_lossy(&out.stderr), 400)
            );
        }
        let info = parse_info(&String::from_utf8_lossy(&out.stdout))?;
        // Containers left by a worker that was killed: the ones that have
        // exited. A running one may belong to another worker process (an
        // install's check-engine), so it's left to finish.
        let pruned = podman_command(podman, Some(&remote.url))
            .args(["container", "prune", "--force", "--filter"])
            .arg(format!("label={CONTAINER_LABEL}"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        if !matches!(pruned, Ok(s) if s.success()) {
            tracing::warn!(target: "usnm_ocr_worker::engine", "couldn't remove stale engine containers");
        }
        let sandbox = Self {
            podman: podman.to_owned(),
            url: remote.url.clone(),
            image: image.to_owned(),
            scratch: remote.scratch.clone(),
            info,
        };
        for (path, slot) in remove_stale_slots(&remote.scratch, Duration::from_secs(6 * 3600)) {
            sandbox.purge(&path, slot).await;
        }
        Ok(sandbox)
    }

    /// A page's scratch directory, which the engine container can use:
    /// a slot owned by the worker with mode 0700, with ACL entries for the
    /// container user (and defaults, so the files in it are shared both
    /// ways) and search-only for the engine account, whose podman mounts
    /// it, and, under SELinux, labelled `container_file_t` at the slot's
    /// level. The container runs at that level, so no volume is mounted
    /// with `:Z`, which the engine account couldn't apply to a directory it
    /// doesn't own (13 §13.3).
    /// Removes a page's slot. The container user owns what it made there
    /// and can make it unreadable to the worker (a directory with mode
    /// 000), so when the worker can't remove it, a container as that user
    /// at the slot's level empties it first.
    pub(crate) async fn release(&self, dir: PageDir) {
        if std::fs::remove_dir_all(&dir.path).is_ok() {
            return;
        }
        self.purge(&dir.path, dir.slot).await;
    }

    async fn purge(&self, path: &Path, slot: u32) {
        let mut cmd = podman_command(&self.podman, Some(&self.url));
        cmd.args([
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--read-only",
        ])
        .args(["--cap-drop=all", "--security-opt=no-new-privileges"])
        .args(["--label", CONTAINER_LABEL, "--log-driver=none"])
        .args(["--user", &format!("{ENGINE_USER}:{ENGINE_USER}")])
        .args([
            "--memory",
            "128m",
            "--memory-swap",
            "128m",
            "--pids-limit",
            "16",
        ]);
        if self.info.selinux {
            cmd.args([
                "--security-opt",
                &format!("label=level:{}", slot_level(slot)),
            ]);
        }
        cmd.args(["--volume", &format!("{}:/work:rw", path.display())])
            .args(["--entrypoint", "sh", &self.image, "-c"])
            .arg("chmod -R u+rwX /work; find /work -mindepth 1 -delete")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let ran = tokio::time::timeout(Duration::from_secs(120), cmd.status()).await;
        if let Err(e) = std::fs::remove_dir_all(path) {
            tracing::warn!(target: "usnm_ocr_worker::engine", dir = %path.display(), error = %e, ran = ?ran.ok(), "couldn't empty a page's scratch slot");
        }
    }

    pub(crate) async fn page_dir(&self) -> anyhow::Result<(PageDir, Option<String>)> {
        let dir = claim_slot(&self.scratch)?;
        let (me, _) = owner(&dir.path)?;
        let e = self.info.engine_uid;
        let mut acl = format!("u:{e}:rwx,d:u:{e}:rwx,d:u:{me}:rwx");
        if let Some(r) = self.info.runtime_uid {
            acl.push_str(&format!(",u:{r}:x"));
        }
        checked(
            tokio::process::Command::new("setfacl")
                .args(["-m", &acl])
                .arg(&dir.path),
        )
        .await?;
        let level = if self.info.selinux {
            let level = dir.level();
            checked(
                tokio::process::Command::new("chcon")
                    .args(["-t", "container_file_t", "-l", &level])
                    .arg(&dir.path),
            )
            .await?;
            Some(level)
        } else {
            None
        };
        Ok((dir, level))
    }
}

/// A podman command, through the Podman API socket at `url` when given.
/// For the socket, the client gets a cleared environment with only what it
/// needs, so the worker's settings never reach it (13 §13.6.1).
pub(crate) fn podman_command(podman: &Path, url: Option<&str>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(podman);
    if let Some(url) = url {
        cmd.env_clear();
        for k in [
            "PATH",
            "LANG",
            "HOME",
            "XDG_RUNTIME_DIR",
            "XDG_CONFIG_HOME",
            "CONTAINERS_CONF",
        ] {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        cmd.args(["--remote", "--url", url]);
    }
    cmd
}

/// A container image reference that names its content: an image id
/// (`sha256:` and 64 hex digits) or a name pinned by digest
/// (`<name>@sha256:<digest>`). Tags can move, so they are refused.
pub fn check_image_ref(image: &str) -> anyhow::Result<()> {
    let hex64 =
        |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let ok = match image.split_once("@sha256:") {
        Some((name, digest)) => {
            hex64(digest)
                && !name.is_empty()
                && name.bytes().all(|b| {
                    b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || matches!(b, b'.' | b'/' | b'-' | b'_' | b':')
                })
        }
        None => image.strip_prefix("sha256:").is_some_and(hex64),
    };
    if !ok {
        bail!("Tesseract image `{image}` must be `sha256:<id>` or `<name>@sha256:<digest>`, not a tag");
    }
    Ok(())
}

/// Removes a page's container. Containers run without `--rm`, so the worker
/// can read `OOMKilled` after one exits ([`ContainerGuard::finish`]); if the
/// page is cancelled or times out first, dropping the guard removes the
/// container: killing the podman client leaves it running.
pub(crate) struct ContainerGuard {
    pub(crate) podman: PathBuf,
    pub(crate) url: Option<String>,
    pub(crate) name: String,
    pub(crate) armed: bool,
}

impl ContainerGuard {
    fn rm(&self) -> tokio::process::Command {
        let mut cmd = podman_command(&self.podman, self.url.as_deref());
        cmd.args(["rm", "--force", "--ignore", "--time", "0", &self.name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd
    }

    /// After the container exits: whether the kernel killed it for memory,
    /// then remove it. A failed removal is left to the drop.
    pub(crate) async fn finish(&mut self) -> bool {
        let oom = podman_command(&self.podman, self.url.as_deref())
            .args([
                "container",
                "inspect",
                "--format",
                "{{.State.OOMKilled}}",
                &self.name,
            ])
            .kill_on_drop(true)
            .output()
            .await
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true"
            });
        if matches!(self.rm().status().await, Ok(s) if s.success()) {
            self.armed = false;
        }
        oom
    }
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut cmd = self.rm();
        let name = self.name.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                // `podman rm` can fail while the container is still
                // stopping; try a few times, and say so if it never goes.
                for attempt in 1..=3u32 {
                    match cmd.status().await {
                        Ok(s) if s.success() => return,
                        Ok(s) => tracing::warn!(target: "usnm_ocr_worker::engine", container = %name, attempt, status = %s, "podman rm failed"),
                        Err(e) => tracing::warn!(target: "usnm_ocr_worker::engine", container = %name, attempt, error = %e, "podman rm didn't run"),
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5 * u64::from(attempt))).await;
                }
                tracing::error!(target: "usnm_ocr_worker::engine", container = %name, "couldn't remove a stopped page's container; remove it by hand");
            });
        }
    }
}

impl Runner {
    /// The command for `tesseract <args>`, with `work` (when given) shared
    /// as `/work`, and the paths in `args` already relative to it. `level`
    /// is the MCS level `work` is labelled with, under the engine runtime.
    /// The guard removes the container.
    pub(super) fn command(
        &self,
        threads: u32,
        work: Option<&Path>,
        level: Option<&str>,
    ) -> (tokio::process::Command, Option<ContainerGuard>) {
        match self {
            Runner::Host(bin) => {
                let mut cmd = tokio::process::Command::new(bin);
                cmd.env("OMP_THREAD_LIMIT", threads.to_string());
                if let Some(w) = work {
                    cmd.current_dir(w);
                }
                (cmd, None)
            }
            Runner::Podman {
                podman,
                remote,
                image,
            } => {
                let name = format!("usnm-tesseract-{}", crate::queue::random_token());
                let url = remote.as_ref().map(|r| r.url.clone());
                let mut cmd = podman_command(podman, url.as_deref());
                // No network, a read-only root, no capabilities and no
                // privilege gain: the engine only reads the scan in /work
                // and writes its output there.
                cmd.args(["run", "--pull=never", "--network=none", "--read-only"])
                    .args(["--cap-drop=all", "--security-opt=no-new-privileges"])
                    .args(["--name", &name, "--label", CONTAINER_LABEL])
                    .args(["--env", &format!("OMP_THREAD_LIMIT={threads}")])
                    .args(["--entrypoint", "tesseract"]);
                match remote {
                    Some(r) => {
                        // 13 §13.6.1: the image's user, which maps to a
                        // subordinate id of the engine account; memory
                        // equal to memory plus swap, so no swap; CPU, pid,
                        // open-file and core limits; nothing from the host.
                        let mem = format!("{}m", r.limits.memory_mib);
                        cmd.args(["--http-proxy=false", "--no-hosts"])
                            .args(["--tmpfs", "/tmp:rw,nosuid,nodev,size=64m,mode=1777"])
                            .args(["--user", &format!("{ENGINE_USER}:{ENGINE_USER}")])
                            .args(["--memory", &mem, "--memory-swap", &mem])
                            .args(["--cpus", &r.limits.cpus.to_string()])
                            .args(["--pids-limit", &r.limits.pids.to_string()])
                            .args(["--ulimit", "nofile=1024:1024", "--ulimit", "core=0"])
                            .args(["--log-driver=none", "--env", "HOME=/tmp"]);
                        if let Some(level) = level {
                            cmd.args(["--security-opt", &format!("label=level:{level}")]);
                        }
                        if let Some(w) = work {
                            cmd.args(["--volume", &format!("{}:/work:rw", w.display())])
                                .args(["--workdir", "/work"]);
                        }
                    }
                    None => {
                        cmd.arg("--userns=keep-id");
                        if let Some(w) = work {
                            // :Z gives the page's private scratch directory
                            // the container's SELinux label. The engine runs
                            // as the directory's owner (the worker's user,
                            // mapped to itself by keep-id), not the image's
                            // user, so it can write its output there.
                            cmd.args(["--volume", &format!("{}:/work:Z", w.display())])
                                .args(["--workdir", "/work"]);
                            if let Ok((uid, gid)) = owner(w) {
                                cmd.args(["--user", &format!("{uid}:{gid}")]);
                            }
                        }
                    }
                }
                cmd.arg(image);
                let guard = ContainerGuard {
                    podman: podman.clone(),
                    url,
                    name,
                    armed: true,
                };
                (cmd, Some(guard))
            }
        }
    }

    pub(super) fn describe(&self) -> String {
        match self {
            Runner::Host(bin) => bin.display().to_string(),
            Runner::Podman {
                image,
                remote: Some(r),
                ..
            } => format!("podman image {image} through {}", r.url),
            Runner::Podman { image, .. } => format!("podman image {image}"),
        }
    }
}
