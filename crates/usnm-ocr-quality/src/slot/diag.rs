//! Bounded diagnostics and selected host/runtime metadata.

use super::resolve_slot_file;
use std::{fmt::Write as _, path::Path};
use usnm_ocr_contract::quality::{Host, MAX_DIAG};

/// The diagnostics text, kept under [`MAX_DIAG`]: each section is cut to its
/// own cap and the whole to the limit, with a note where it was cut.
pub struct Diag(String);
impl Default for Diag {
    fn default() -> Self {
        Self::new()
    }
}
impl Diag {
    pub fn new() -> Self {
        Self(String::new())
    }
    pub fn section(&mut self, title: &str, body: &str, cap: usize) {
        let _ = writeln!(self.0, "== {title}");
        let body = body.trim_end();
        if body.len() > cap {
            let mut end = cap;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            let _ = writeln!(
                self.0,
                "{}\n[cut at {cap} of {} bytes]",
                &body[..end],
                body.len()
            );
        } else if !body.is_empty() {
            let _ = writeln!(self.0, "{body}");
        }
    }
    pub fn finish(mut self) -> Vec<u8> {
        let limit = MAX_DIAG - 64;
        if self.0.len() > limit {
            let mut end = limit;
            while !self.0.is_char_boundary(end) {
                end -= 1;
            }
            self.0.truncate(end);
            self.0.push_str("\n[diagnostics cut at the size limit]\n");
        }
        self.0.into_bytes()
    }
}

/// The last `n` bytes of `s`, from a character boundary.
pub fn tail(s: &str, n: usize) -> &str {
    let mut i = s.len().saturating_sub(n);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
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

pub fn boinc_info(work: &Path, host: &mut Host, diag: &mut Diag) {
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
    host.ncpus = tag(&xml, "ncpus").and_then(|s| s.parse().ok());
    diag.section("boinc (init_data.xml, selected fields)", &body, 2048);
}

pub fn runtime_info(host: &mut Host, diag: &mut Diag) {
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
    if Path::new("/sys/fs/cgroup").exists() {
        for (name, path) in [
            ("cgroup memory.max", "/sys/fs/cgroup/memory.max"),
            ("cgroup memory.swap.max", "/sys/fs/cgroup/memory.swap.max"),
            ("cgroup cpu.max", "/sys/fs/cgroup/cpu.max"),
            ("cgroup pids.max", "/sys/fs/cgroup/pids.max"),
        ] {
            let v = std::fs::read_to_string(path).unwrap_or_else(|e| format!("({e})"));
            let _ = writeln!(body, "{name}: {}", v.trim());
        }
    }
    if let Ok(m) = std::fs::read_to_string("/proc/meminfo") {
        for l in m
            .lines()
            .filter(|l| l.starts_with("MemTotal") || l.starts_with("MemAvailable"))
        {
            let _ = writeln!(body, "{l}");
        }
    }
    diag.section("runtime", &body, 2048);
}
