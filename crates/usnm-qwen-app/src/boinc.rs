//! The BOINC client's side of the slot: the control channel, `init_data.xml`, and the
//! runtime the client runs the app under.
use std::{fmt::Write as _, path::Path};
use usnm_ocr_contract::surya::Host;
use usnm_ocr_quality::slot::{resolve_slot_file, BoincControl, Diag};

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
