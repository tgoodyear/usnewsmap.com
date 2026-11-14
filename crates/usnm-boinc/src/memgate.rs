//! Memory check before an OCR job starts its engine (#1934).
//!
//! BOINC starts one task per usable CPU, and the container runtime's VM (a Podman machine on a
//! Mac) can hold fewer tasks than that. The container's own `--memory` limit doesn't help: every
//! task is inside its limit while the VM's kernel kills one for memory, and Tesseract dies with
//! SIGKILL. The stock `docker_wrapper` has no pre-start hook, so the worker looks itself: if the
//! headroom is short it asks the client to retry the task later and exits without starting.
//!
//! The ask is BOINC's temporary exit: a `boinc_temporary_exit` file in the slot (the first line
//! the delay in seconds, the second the reason). The client reads it from the slot directory
//! whenever the task's process exits, whatever the exit code, and on Unix before it looks at the
//! exit status, so `docker_wrapper` passes it through without knowing about it: the slot is
//! mounted at `/work` in the container, and the wrapper ends with the container's exit code. The
//! client deletes the file when it restarts the task. (The client's Windows path looks at the
//! finish file first, which `docker_wrapper` always writes; container tasks don't run there.)
use anyhow::Context;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// The file the client reads for a temporary exit (`api/boinc_api.cpp`, `client/app_control.cpp`).
pub const TEMPORARY_EXIT_FILE: &str = "boinc_temporary_exit";
/// How many times this task has deferred; kept in the slot across restarts.
pub const DEFER_COUNT_FILE: &str = "usnm_memory_defers";
/// After this many deferrals the task starts anyway, as it did before the check: a VM that never
/// frees memory shouldn't keep a task from ever running. The client aborts a task at 100
/// temporary exits, so this stays well under that.
pub const MAX_DEFERS: u32 = 20;
/// The retry delay: this, plus up to [`BACKOFF_JITTER_SECS`] so tasks that deferred together
/// don't all start together.
pub const BACKOFF_SECS: u32 = 180;
pub const BACKOFF_JITTER_SECS: u32 = 120;

const MIB: u64 = 1 << 20;
/// What one Tesseract page read needs: the decoded scan, the engine's working set and the worker
/// itself. An estimate under the 1.5 GiB container limit, which a page may approach but which
/// the docs size VMs by.
pub const TESSERACT_NEED_MIB: u64 = 1024;
/// Kept free beyond the need, for the VM's other tasks and its own overhead.
pub const MARGIN_MIB: u64 = 256;

/// What a job needs free to start, in bytes: the engine's estimate plus the margin.
pub fn required_bytes(kind: usnm_ocr_worker::engine::EngineKind) -> u64 {
    let need = match kind {
        usnm_ocr_worker::engine::EngineKind::Tesseract => TESSERACT_NEED_MIB,
        // Only Tesseract jobs reach this check; anything else is as heavy as the limit allows.
        _ => 1536,
    };
    (need + MARGIN_MIB) * MIB
}

/// Where the memory figures are read from; a test points these at files it wrote.
#[derive(Debug, Clone)]
pub struct Sources {
    pub cgroup_v2: PathBuf,
    pub cgroup_v1: PathBuf,
    pub meminfo: PathBuf,
}

impl Sources {
    pub fn system() -> Self {
        Self {
            cgroup_v2: "/sys/fs/cgroup".into(),
            cgroup_v1: "/sys/fs/cgroup/memory".into(),
            meminfo: "/proc/meminfo".into(),
        }
    }
}

/// The smaller of the headroom figures that could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headroom {
    pub bytes: u64,
    /// Which figure was the smaller: `cgroup`, or `meminfo` (the VM as a whole).
    pub limited_by: &'static str,
    pub cgroup: Option<u64>,
    pub meminfo: Option<u64>,
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// One `key value` line of a cgroup `memory.stat`.
fn stat_field(path: &Path, key: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        (k == key).then(|| v.trim().parse().ok())?
    })
}

/// The container's cgroup headroom: limit minus use, where the use leaves out inactive file
/// cache the kernel gives back before it kills anything. `None` with no limit or no cgroup.
fn cgroup_headroom(s: &Sources) -> Option<u64> {
    let v2 = (|| {
        // `max` isn't a number: no limit at this level.
        let limit = read_u64(&s.cgroup_v2.join("memory.max"))?;
        let used = read_u64(&s.cgroup_v2.join("memory.current"))?;
        let cache = stat_field(&s.cgroup_v2.join("memory.stat"), "inactive_file").unwrap_or(0);
        Some(limit.saturating_sub(used.saturating_sub(cache)))
    })();
    if v2.is_some() {
        return v2;
    }
    let limit = read_u64(&s.cgroup_v1.join("memory.limit_in_bytes"))?;
    // v1 spells "no limit" as a number near 2^63.
    if limit >= 1 << 60 {
        return None;
    }
    let used = read_u64(&s.cgroup_v1.join("memory.usage_in_bytes"))?;
    let cache = stat_field(&s.cgroup_v1.join("memory.stat"), "total_inactive_file").unwrap_or(0);
    Some(limit.saturating_sub(used.saturating_sub(cache)))
}

/// `MemAvailable` of `/proc/meminfo`, in bytes. In a container this is the VM's, not the limit's.
fn meminfo_available(s: &Sources) -> Option<u64> {
    fs::read_to_string(&s.meminfo).ok()?.lines().find_map(|l| {
        let rest = l.strip_prefix("MemAvailable:")?;
        let kb: u64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
        kb.checked_mul(1024)
    })
}

/// The headroom now, or `None` if neither source could be read.
pub fn headroom(s: &Sources) -> Option<Headroom> {
    let cgroup = cgroup_headroom(s);
    let meminfo = meminfo_available(s);
    let (bytes, limited_by) = match (cgroup, meminfo) {
        (Some(c), Some(m)) if c <= m => (c, "cgroup"),
        (Some(_), Some(m)) => (m, "meminfo"),
        (Some(c), None) => (c, "cgroup"),
        (None, Some(m)) => (m, "meminfo"),
        (None, None) => return None,
    };
    Some(Headroom {
        bytes,
        limited_by,
        cgroup,
        meminfo,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Start. The line is what to log.
    Start(String),
    /// Ask the client to retry in this many seconds.
    Defer { secs: u32, line: String },
}

fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

fn fmt_opt(v: Option<u64>) -> String {
    v.map_or("n/a".into(), |b| format!("{}MiB", mib(b)))
}

/// Decides from `headroom`, the job's `required` bytes and how many times the task has deferred.
pub fn decide(
    headroom: Option<&Headroom>,
    required: u64,
    deferred: u32,
    jitter_secs: u32,
) -> Decision {
    let Some(h) = headroom else {
        return Decision::Start(
            "usnm memory check: no cgroup or meminfo figures readable, starting".into(),
        );
    };
    let figures = format!(
        "headroom {}MiB (cgroup {}, vm {}), needs {}MiB",
        mib(h.bytes),
        fmt_opt(h.cgroup),
        fmt_opt(h.meminfo),
        mib(required)
    );
    if h.bytes >= required {
        return Decision::Start(format!("usnm memory check: {figures}, starting"));
    }
    if deferred >= MAX_DEFERS {
        return Decision::Start(format!(
            "usnm memory check: {figures}, short but already deferred {deferred} times, starting"
        ));
    }
    let secs = BACKOFF_SECS + jitter_secs % (BACKOFF_JITTER_SECS + 1);
    Decision::Defer {
        secs,
        line: format!(
            "usnm memory check: {figures}, short (limited by {}); deferring {secs}s via boinc_temporary_exit ({}/{MAX_DEFERS})",
            h.limited_by,
            deferred + 1
        ),
    }
}

fn deferred_count(work: &Path) -> u32 {
    fs::read_to_string(work.join(DEFER_COUNT_FILE))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Whether the job may start. On a deferral this has written the temporary-exit file, and the
/// caller must return without starting anything.
pub fn check(
    work: &Path,
    sources: &Sources,
    required: u64,
    jitter_secs: u32,
) -> anyhow::Result<bool> {
    let h = headroom(sources);
    let deferred = deferred_count(work);
    match decide(h.as_ref(), required, deferred, jitter_secs) {
        Decision::Start(line) => {
            eprintln!("{line}");
            Ok(true)
        }
        Decision::Defer { secs, line } => {
            eprintln!("{line}");
            crate::atomic(
                &work.join(DEFER_COUNT_FILE),
                format!("{}\n", deferred + 1).as_bytes(),
            )
            .context("recording the deferral")?;
            crate::atomic(
                &work.join(TEMPORARY_EXIT_FILE),
                format!("{secs}\nnot enough free memory to start Tesseract\n").as_bytes(),
            )
            .context("writing the temporary exit")?;
            Ok(false)
        }
    }
}

/// Spreads the retries of tasks that deferred together.
pub fn jitter_from_clock() -> u32 {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    n ^ std::process::id()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn sources(root: &Path) -> Sources {
        Sources {
            cgroup_v2: root.join("v2"),
            cgroup_v1: root.join("v1"),
            meminfo: root.join("meminfo"),
        }
    }

    fn meminfo(root: &Path, kb: u64) {
        write(
            root,
            "meminfo",
            &format!("MemTotal:       4000000 kB\nMemFree: 1 kB\nMemAvailable:   {kb} kB\n"),
        );
    }

    #[test]
    fn cgroup_v2_headroom_is_limit_minus_use_less_inactive_cache() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "v2/memory.max", "1610612736\n");
        write(t.path(), "v2/memory.current", "524288000\n");
        write(
            t.path(),
            "v2/memory.stat",
            "anon 1\ninactive_file 104857600\n",
        );
        let h = headroom(&sources(t.path())).unwrap();
        assert_eq!(h.bytes, 1_610_612_736 - (524_288_000 - 104_857_600));
        assert_eq!(h.limited_by, "cgroup");
        assert_eq!(h.meminfo, None);
    }

    #[test]
    fn an_unlimited_v2_cgroup_leaves_meminfo() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "v2/memory.max", "max\n");
        write(t.path(), "v2/memory.current", "1\n");
        meminfo(t.path(), 2048);
        let h = headroom(&sources(t.path())).unwrap();
        assert_eq!((h.bytes, h.limited_by), (2048 * 1024, "meminfo"));
    }

    #[test]
    fn cgroup_v1_is_the_fallback() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "v1/memory.limit_in_bytes", "1610612736\n");
        write(t.path(), "v1/memory.usage_in_bytes", "1073741824\n");
        write(t.path(), "v1/memory.stat", "total_inactive_file 0\n");
        let h = headroom(&sources(t.path())).unwrap();
        assert_eq!(h.bytes, 536_870_912);
    }

    #[test]
    fn a_v1_cgroup_without_a_limit_is_ignored() {
        let t = tempfile::tempdir().unwrap();
        write(
            t.path(),
            "v1/memory.limit_in_bytes",
            "9223372036854771712\n",
        );
        write(t.path(), "v1/memory.usage_in_bytes", "1\n");
        assert_eq!(headroom(&sources(t.path())), None);
    }

    #[test]
    fn the_smaller_of_cgroup_and_vm_wins() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "v2/memory.max", "1610612736\n");
        write(t.path(), "v2/memory.current", "0\n");
        meminfo(t.path(), 300 * 1024);
        let h = headroom(&sources(t.path())).unwrap();
        assert_eq!((h.bytes, h.limited_by), (300 * MIB, "meminfo"));
        meminfo(t.path(), 4000 * 1024);
        let h = headroom(&sources(t.path())).unwrap();
        assert_eq!((h.bytes, h.limited_by), (1_610_612_736, "cgroup"));
    }

    #[test]
    fn enough_memory_starts_without_touching_the_slot() {
        let t = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(t.path(), "v2/memory.max", "1610612736\n");
        write(t.path(), "v2/memory.current", "0\n");
        meminfo(t.path(), 4000 * 1024);
        let need = required_bytes(usnm_ocr_worker::engine::EngineKind::Tesseract);
        assert!(check(work.path(), &sources(t.path()), need, 7).unwrap());
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 0);
    }

    #[test]
    fn nothing_readable_starts() {
        let t = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        assert!(check(work.path(), &sources(t.path()), 1 << 30, 0).unwrap());
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_short_vm_defers_with_the_temporary_exit_file_and_counts_it() {
        let t = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(t.path(), "v2/memory.max", "1610612736\n");
        write(t.path(), "v2/memory.current", "0\n");
        meminfo(t.path(), 200 * 1024);
        let need = required_bytes(usnm_ocr_worker::engine::EngineKind::Tesseract);
        assert!(!check(work.path(), &sources(t.path()), need, 50).unwrap());
        let file = fs::read_to_string(work.path().join(TEMPORARY_EXIT_FILE)).unwrap();
        let mut lines = file.lines();
        assert_eq!(lines.next(), Some("230"));
        assert!(lines.next().is_some_and(|r| !r.is_empty()));
        assert_eq!(deferred_count(work.path()), 1);
        // Again: the count rises.
        assert!(!check(work.path(), &sources(t.path()), need, 0).unwrap());
        assert_eq!(deferred_count(work.path()), 2);
    }

    #[test]
    fn after_the_deferral_cap_the_task_starts_anyway() {
        let h = Headroom {
            bytes: MIB,
            limited_by: "meminfo",
            cgroup: None,
            meminfo: Some(MIB),
        };
        assert!(matches!(
            decide(Some(&h), 100 * MIB, MAX_DEFERS - 1, 0),
            Decision::Defer { .. }
        ));
        assert!(matches!(
            decide(Some(&h), 100 * MIB, MAX_DEFERS, 0),
            Decision::Start(_)
        ));
    }

    #[test]
    fn the_backoff_stays_within_its_window_and_the_log_is_one_line() {
        let h = Headroom {
            bytes: MIB,
            limited_by: "cgroup",
            cgroup: Some(MIB),
            meminfo: None,
        };
        for jitter in [0, 1, 119, 120, 121, u32::MAX] {
            let Decision::Defer { secs, line } = decide(Some(&h), 100 * MIB, 0, jitter) else {
                panic!("expected a deferral");
            };
            assert!((BACKOFF_SECS..=BACKOFF_SECS + BACKOFF_JITTER_SECS).contains(&secs));
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn the_need_is_the_engine_estimate_plus_the_margin() {
        assert_eq!(
            required_bytes(usnm_ocr_worker::engine::EngineKind::Tesseract),
            (TESSERACT_NEED_MIB + MARGIN_MIB) * MIB
        );
    }
}
