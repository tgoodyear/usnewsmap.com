//! The BOINC client's side of the slot: the control channel, `init_data.xml`, and the
//! runtime the client runs the app under.
use std::{fmt::Write as _, path::Path};
use usnm_ocr_contract::surya::Host;
use usnm_ocr_quality::slot::{resolve_slot_file, write_atomic, BoincControl, Diag};

/// The file `boinc_temporary_exit()` writes: the client restarts the task after the delay
/// instead of reporting it (BOINC `api/boinc_api.cpp`, `client/app_control.cpp`).
pub(crate) const TEMPORARY_EXIT_FILE: &str = "boinc_temporary_exit";
/// Device-probe timeouts this task has already postponed for; kept in the slot across restarts.
pub(crate) const PROBE_TIMEOUTS_FILE: &str = "usnm_gpu_probe_timeouts";
pub(crate) const MAX_PROBE_POSTPONES: u32 = 3;
pub(crate) const PROBE_POSTPONE_SECS: u32 = 120;

/// After a GPU device probe timed out, asks the client to restart the task later, as
/// `boinc_temporary_exit()` does. The timed-out probe has read the driver libraries into the page
/// cache, so the restart usually lists the device at once (#1252). Returns the attempt number, or
/// `None` once [`MAX_PROBE_POSTPONES`] have been used and the timeout must stand as `no_gpu`.
pub(crate) fn postpone_probe_timeout(work: &Path) -> anyhow::Result<Option<u32>> {
    let counter = work.join(PROBE_TIMEOUTS_FILE);
    let used = std::fs::read_to_string(&counter)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if used >= MAX_PROBE_POSTPONES {
        return Ok(None);
    }
    let attempt = used + 1;
    write_atomic(&counter, format!("{attempt}\n").as_bytes())?;
    write_atomic(
        &work.join(TEMPORARY_EXIT_FILE),
        format!(
            "{PROBE_POSTPONE_SECS}\nGPU driver still loading (device probe timed out {attempt}/{MAX_PROBE_POSTPONES})\n"
        )
        .as_bytes(),
    )?;
    Ok(Some(attempt))
}

pub(crate) fn start_boinc_control(work: &Path) -> BoincControl {
    let control = BoincControl::start(work);
    #[cfg(unix)]
    {
        use usnm_ocr_quality::slot::ChildAction;
        control.set_child_hook(Some(std::sync::Arc::new(|pgid, action| {
            if let Some(pid) = rustix::process::Pid::from_raw(pgid as i32) {
                let sig = match action {
                    ChildAction::Suspend => rustix::process::Signal::STOP,
                    ChildAction::Resume => rustix::process::Signal::CONT,
                    ChildAction::Terminate => rustix::process::Signal::TERM,
                };
                let _ = rustix::process::kill_process_group(pid, sig);
            }
        })));
    }
    control
}

pub(crate) fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(xml[start..end].trim())
}

const INIT_FIELDS: [&str; 10] = [
    "major_version",
    "minor_version",
    "release",
    "app_version",
    "plan_class",
    "gpu_type",
    "gpu_device_num",
    "gpu_opencl_dev_index",
    "ncpus",
    "rsc_memory_bound",
];

pub(crate) fn boinc_info(work: &Path, host: &mut Host, diag: &mut Diag) {
    let init_path = resolve_slot_file(work, "init_data.xml");
    let Ok(xml) = std::fs::read_to_string(init_path) else {
        diag.section("boinc", "no init_data.xml in the slot", 256);
        return;
    };
    let mut body = String::new();
    for f in INIT_FIELDS {
        if let Some(v) = tag(&xml, f) {
            if v.len() <= 32
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                let _ = writeln!(body, "{f}: {v}");
            }
        }
    }
    if let (Some(a), Some(b), Some(c)) = (
        tag(&xml, "major_version"),
        tag(&xml, "minor_version"),
        tag(&xml, "release"),
    ) {
        host.boinc_client = Some(format!("{a}.{b}.{c}")).filter(|s| s.len() <= 32);
    }
    host.plan_class = tag(&xml, "plan_class")
        .filter(|s| s.len() <= 32)
        .map(str::to_owned);
    host.gpu_type = tag(&xml, "gpu_type")
        .filter(|s| s.len() <= 32)
        .map(str::to_owned);
    host.gpu_device_num = tag(&xml, "gpu_device_num").and_then(|s| s.parse().ok());
    host.ncpus = tag(&xml, "ncpus").and_then(|s| s.parse().ok());
    diag.section("boinc (init_data.xml, selected fields)", &body, 2048);
}

pub(crate) fn runtime_info(host: &mut Host, diag: &mut Diag) {
    let mut body = String::new();
    if let Ok(env) = std::fs::read_to_string("/run/.containerenv") {
        for l in env.lines() {
            if let Some(v) = l.strip_prefix("engine=") {
                let v = v.trim_matches('"');
                host.runtime = Some(v.chars().take(32).collect());
                let _ = writeln!(body, "engine: {v}");
            }
            if let Some(v) = l.strip_prefix("rootless=") {
                host.rootless = Some(v.trim() == "1");
                let _ = writeln!(body, "rootless: {v}");
            }
        }
    } else if Path::new("/.dockerenv").exists() {
        host.runtime = Some("docker".into());
        body.push_str("engine: docker (/.dockerenv)\n");
    } else {
        let rt = format!("native-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        host.runtime = Some(rt.chars().take(32).collect());
        let _ = writeln!(body, "engine: {rt}");
    }
    host.cpu_threads = std::thread::available_parallelism()
        .ok()
        .and_then(|n| u32::try_from(n.get()).ok());
    let _ = writeln!(body, "available parallelism: {:?}", host.cpu_threads);
    diag.section("runtime", &body, 2048);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_timeouts_postpone_in_the_boinc_format_a_bounded_number_of_times() {
        let tmp = tempfile::tempdir().unwrap();
        for attempt in 1..=MAX_PROBE_POSTPONES {
            assert_eq!(postpone_probe_timeout(tmp.path()).unwrap(), Some(attempt));
            let file = std::fs::read_to_string(tmp.path().join(TEMPORARY_EXIT_FILE)).unwrap();
            let mut lines = file.lines();
            assert_eq!(lines.next(), Some("120"));
            assert_eq!(
                lines.next().map(str::to_owned),
                Some(format!(
                    "GPU driver still loading (device probe timed out {attempt}/3)"
                ))
            );
            assert_eq!(lines.next(), None, "not a user notice");
            std::fs::remove_file(tmp.path().join(TEMPORARY_EXIT_FILE)).unwrap();
        }
        assert_eq!(postpone_probe_timeout(tmp.path()).unwrap(), None);
        assert!(!tmp.path().join(TEMPORARY_EXIT_FILE).exists());
        std::fs::write(tmp.path().join(PROBE_TIMEOUTS_FILE), "99\n").unwrap();
        assert_eq!(postpone_probe_timeout(tmp.path()).unwrap(), None);
    }
}
