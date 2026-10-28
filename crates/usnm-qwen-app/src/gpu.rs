//! GPU detection: which device BOINC assigned this task, and how much memory it has free.
use crate::{
    boinc::tag,
    report::redacted_tail,
    server::{find_llama_server, library_env},
};
use std::{
    fmt::Write as _,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use usnm_ocr_contract::{qwen::Job, surya::Host};
use usnm_ocr_quality::slot::{resolve_slot_file, Diag};

fn quick(program: &str, args: &[&str], deadline: Duration) -> Result<(bool, String), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program}: no answer in {}s", deadline.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push_str("\n[stderr]\n");
        text.push_str(&err);
    }
    Ok((out.status.success(), text))
}

/// One device from `llama-server --list-devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VulkanDevice {
    index: u32,
    name: String,
    total_mib: u64,
    free_mib: u64,
}

/// Software rasterizers Mesa and others expose as Vulkan devices; never a GPU class's device.
const SOFTWARE_VULKAN: [&str; 3] = ["llvmpipe", "lavapipe", "swiftshader"];

/// Parses `  Vulkan0: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 7125 MiB free)` lines
/// and returns the device BOINC assigned. BOINC numbers a vendor's GPUs among themselves
/// (`gpu_device_num` for `gpu_type intel` counts Intel GPUs only), so `wanted` is an ordinal
/// among the listed hardware devices of `vendor`, not a `VulkanN` index. There is no fallback to
/// another device: one BOINC did not assign may be running another task.
fn vulkan_device(list: &str, vendor: &str, wanted: u32) -> Option<VulkanDevice> {
    let devices: Vec<VulkanDevice> = list
        .lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Vulkan")?;
            let (index, rest) = rest.split_once(": ")?;
            let index: u32 = index.parse().ok()?;
            let rest = rest.strip_suffix(" MiB free)")?;
            let open = rest.rfind(" (")?;
            let (name, mem) = (&rest[..open], &rest[open + 2..]);
            let (total, free) = mem.split_once(" MiB, ")?;
            Some(VulkanDevice {
                index,
                name: name.trim().to_string(),
                total_mib: total.trim().parse().ok()?,
                free_mib: free.trim().parse().ok()?,
            })
        })
        .filter(|d| {
            let n = d.name.to_ascii_lowercase();
            let matches_vendor = if vendor == "amd" {
                n.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| matches!(word, "amd" | "ati" | "radeon"))
                    || n.contains("advanced micro devices")
            } else {
                n.contains(vendor)
            };
            !SOFTWARE_VULKAN.iter().any(|s| n.contains(s)) && matches_vendor
        })
        .collect();
    devices.into_iter().nth(usize::try_from(wanted).ok()?)
}

/// Bounds GPU enumeration. Cold Vulkan driver loading on a busy volunteer host can take more
/// than a minute (65 s measured for a correct NVIDIA listing, #1252), so this matches the
/// model-load deadline instead of misreporting a slow but healthy GPU as `no_gpu`.
pub(crate) const DEVICE_PROBE_DEADLINE: Duration = crate::server::LOAD_DEADLINE;

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_PROBE_DEADLINE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

const PROBE_TIMEOUT: &str = "GPU device probe: no answer in";

/// Whether a probe error is the bounded deadline (a slow, cold driver load) rather than an answer.
pub(crate) fn probe_timed_out(error: &str) -> bool {
    error.starts_with(PROBE_TIMEOUT)
}

/// `llama-server --list-devices`, bounded by [`DEVICE_PROBE_DEADLINE`].
fn list_devices(server_bin: &Path) -> Result<String, String> {
    let mut cmd = Command::new(server_bin);
    cmd.arg("--list-devices");
    let out = device_probe(&mut cmd, server_bin)?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(text)
}

fn device_probe(cmd: &mut Command, server_bin: &Path) -> Result<std::process::Output, String> {
    #[cfg(test)]
    let deadline = TEST_PROBE_DEADLINE
        .with(|d| d.get())
        .unwrap_or(DEVICE_PROBE_DEADLINE);
    #[cfg(not(test))]
    let deadline = DEVICE_PROBE_DEADLINE;
    device_probe_within(cmd, server_bin, deadline)
}

fn device_probe_within(
    cmd: &mut Command,
    server_bin: &Path,
    deadline: Duration,
) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((var, value)) = library_env(server_bin) {
        cmd.env(var, value);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", server_bin.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{PROBE_TIMEOUT} {deadline:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "GPU device probe failed: {}",
            redacted_tail(&String::from_utf8_lossy(&out.stderr), 1024)
        ));
    }
    Ok(out)
}

fn assigned_amd_device(work: &Path, host: &Host, list: &str) -> Result<VulkanDevice, String> {
    let init = std::fs::read_to_string(work.join("init_data.xml")).map_err(|e| e.to_string())?;
    let opencl_index = tag(&init, "gpu_opencl_dev_index")
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("missing assigned OpenCL index")?;
    if host.gpu_device_num.is_none_or(|n| n < 0)
        || !host
            .gpu_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("ati") || t.eq_ignore_ascii_case("amd"))
    {
        return Err("missing AMD resource assignment".into());
    }
    let server = find_llama_server(work).map_err(|e| e.to_string())?;
    let dir = server.parent().ok_or("missing Vulkan bundle directory")?;
    let helper = resolve_slot_file(
        work,
        if cfg!(windows) {
            "usnm-amd-device-map.exe"
        } else {
            "usnm-amd-device-map"
        },
    );
    let out = device_probe(
        Command::new(helper).arg(dir).arg(opencl_index.to_string()),
        &server,
    )?;
    let text = std::str::from_utf8(&out.stdout)
        .map_err(|e| e.to_string())?
        .trim();
    let index = text
        .strip_prefix("Vulkan")
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("invalid PCI device mapping")?;
    let matches: Vec<_> = (0..256)
        .map_while(|n| vulkan_device(list, "amd", n))
        .filter(|d| d.index == index)
        .collect();
    if matches.len() != 1 {
        return Err("PCI-verified AMD device missing or duplicated in Vulkan listing".into());
    }
    Ok(matches[0].clone())
}

/// Why a Vulkan unit can't run on this host, if it can't: no hardware device, or less free
/// device memory than the job's largest model needs (#786).
pub(crate) fn vulkan_refusal(gpu: &GpuState, job: &Job) -> Option<String> {
    gpu.vulkan_device
        .is_none()
        .then(|| {
            "Vulkan plan class assigned, but llama-server lists no hardware Vulkan device"
                .to_string()
        })
        .or_else(|| {
            let need = job
                .engine
                .models
                .iter()
                .map(|m| u64::from(m.min_gpu_mb))
                .max()?;
            let free = gpu.free_mb?;
            (free < need)
                .then(|| format!("Vulkan device has {free} MiB free; the model needs {need} MiB"))
        })
}

#[derive(Debug, Clone)]
pub(crate) struct GpuState {
    pub(crate) cuda_device: Option<u32>,
    /// The `VulkanN` ordinal of the assigned device, as `llama-server --list-devices` names it and
    /// `--device` takes it (#786).
    pub(crate) vulkan_device: Option<u32>,
    pub(crate) free_mb: Option<u64>,
    pub(crate) used_before_mb: Option<u64>,
    pub(crate) use_gpu: bool,
    pub(crate) device_label: String,
    /// The device probe hit its deadline without answering, so the GPU is unknown, not absent.
    pub(crate) probe_timed_out: bool,
}

pub(crate) fn detect_gpu(work: &Path, host: &mut Host, diag: &mut Diag) -> GpuState {
    let mut body = String::new();
    let plan = host.plan_class.as_deref().unwrap_or("");
    let gpu_type = host.gpu_type.as_deref().unwrap_or("");

    if plan == "mt" || plan == "docker_mt" || plan == "cpu" {
        let _ = writeln!(body, "CPU-only plan class ({plan})");
        diag.section("gpu", &body, 1024);
        return GpuState {
            cuda_device: None,
            vulkan_device: None,
            free_mb: None,
            used_before_mb: None,
            use_gpu: false,
            device_label: "cpu".into(),
            probe_timed_out: false,
        };
    }

    if plan.contains("vulkan") {
        // No CPU fallback inside a GPU class: without a hardware Vulkan device the GPU is
        // `None` and the unit ends `no_gpu` (#786).
        let devices = find_llama_server(work)
            .map_err(|e| format!("{e:#}"))
            .and_then(|bin| list_devices(&bin));
        let wanted = u32::try_from(host.gpu_device_num.unwrap_or(0)).ok();
        let mut probe_timeout = false;
        let (vulkan_device, free_mb) = match &devices {
            Ok(text) => {
                let _ = writeln!(body, "llama-server --list-devices:\n{}", text.trim());
                let vendor = if plan.contains("intel") {
                    "intel"
                } else if plan.contains("nvidia") {
                    "nvidia"
                } else if plan == "amd_vulkan_8b" {
                    "amd"
                } else {
                    "unsupported-vulkan-vendor"
                };
                let selected = if plan == "amd_vulkan_8b" {
                    match assigned_amd_device(work, host, text) {
                        Ok(d) => Some(d),
                        Err(error) => {
                            probe_timeout = probe_timed_out(&error);
                            let _ = writeln!(body, "AMD PCI identity mapping refused: {error}");
                            None
                        }
                    }
                } else {
                    wanted.and_then(|ordinal| vulkan_device(text, vendor, ordinal))
                };
                match selected {
                    Some(d) => {
                        host.gpu = Some(d.name.chars().take(64).collect());
                        host.gpu_mem_total_mib = Some(d.total_mib);
                        (Some(d.index), Some(d.free_mib))
                    }
                    None => {
                        let _ = writeln!(body, "no hardware Vulkan device");
                        (None, None)
                    }
                }
            }
            Err(e) => {
                probe_timeout = probe_timed_out(e);
                let _ = writeln!(body, "listing Vulkan devices failed: {e}");
                (None, None)
            }
        };
        let _ = writeln!(body, "Vulkan via plan_class, device {vulkan_device:?}");
        diag.section("gpu", &body, 2048);
        return GpuState {
            cuda_device: None,
            vulkan_device,
            free_mb,
            used_before_mb: None,
            use_gpu: vulkan_device.is_some(),
            device_label: "vulkan".into(),
            probe_timed_out: probe_timeout,
        };
    }

    // Check NVIDIA via nvidia-smi.
    if let Ok((true, out)) = quick(
        "nvidia-smi",
        &[
            "--query-gpu=index,name,memory.total,memory.free,memory.used,driver_version",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_secs(10),
    ) {
        let _ = writeln!(body, "nvidia-smi:\n{}", out.trim());
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        if !lines.is_empty() {
            let wanted = u32::try_from(host.gpu_device_num.unwrap_or(0)).unwrap_or(0);
            let chosen = lines
                .iter()
                .find_map(|l| {
                    let i: u32 = l.split(',').next()?.trim().parse().ok()?;
                    (i == wanted).then_some((i, *l))
                })
                .or_else(|| {
                    let i: u32 = lines[0].split(',').next()?.trim().parse().ok()?;
                    Some((i, lines[0]))
                });
            if let Some((idx, line)) = chosen {
                let parts: Vec<&str> = line.split(',').map(str::trim).collect();
                host.gpu = parts.get(1).map(|s| s.chars().take(64).collect());
                host.gpu_mem_total_mib = parts.get(2).and_then(|s| s.parse().ok());
                host.gpu_driver = parts.get(5).map(|s| s.chars().take(32).collect());
                let free_mb = parts.get(3).and_then(|s| s.parse().ok());
                let used_before_mb = parts.get(4).and_then(|s| s.parse().ok());
                diag.section("gpu", &body, 2048);
                return GpuState {
                    cuda_device: Some(idx),
                    vulkan_device: None,
                    free_mb,
                    used_before_mb,
                    use_gpu: true,
                    device_label: "cuda".into(),
                    probe_timed_out: false,
                };
            }
        }
    }

    if plan.contains("apple_gpu")
        || plan.contains("metal")
        || gpu_type.eq_ignore_ascii_case("apple")
        || gpu_type.eq_ignore_ascii_case("apple_gpu")
    {
        let _ = writeln!(body, "Apple Metal active via plan_class/gpu_type");
        diag.section("gpu", &body, 1024);
        return GpuState {
            cuda_device: None,
            vulkan_device: None,
            free_mb: None,
            used_before_mb: None,
            use_gpu: true,
            device_label: "metal".into(),
            probe_timed_out: false,
        };
    }

    let _ = writeln!(body, "running on CPU");
    diag.section("gpu", &body, 1024);
    GpuState {
        cuda_device: None,
        vulkan_device: None,
        free_mb: None,
        used_before_mb: None,
        use_gpu: false,
        device_label: "cpu".into(),
        probe_timed_out: false,
    }
}

pub(crate) fn gpu_used_mib(dev: u32) -> Option<u64> {
    let (ok, out) = quick(
        "nvidia-smi",
        &[
            &format!("--id={dev}"),
            "--query-gpu=memory.used",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_secs(5),
    )
    .ok()?;
    if !ok {
        return None;
    }
    out.lines().next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boinc::boinc_info;
    #[cfg(unix)]
    use crate::test_support::write_qwen_unit;

    #[test]
    fn apple_gpu_plan_class_and_gpu_type_enable_metal() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>apple_gpu_8b</plan_class><gpu_type>apple_gpu</gpu_type></app_init_data>",
        )
        .unwrap();
        let mut host = Host::default();
        let mut diag = Diag::new();
        boinc_info(&slot, &mut host, &mut diag);
        let gpu = detect_gpu(&slot, &mut host, &mut diag);
        assert!(gpu.use_gpu);
        assert_eq!(gpu.device_label, "metal");
    }

    #[test]
    fn vulkan_device_list_skips_software_devices() {
        let list = "load_backend: loaded Vulkan backend\nAvailable devices:\n  Vulkan0: llvmpipe (LLVM 19.1.7, 256 bits) (15834 MiB, 15000 MiB free)\n  Vulkan1: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 7125 MiB free)\n";
        let d = vulkan_device(list, "intel", 0).unwrap();
        assert_eq!(d.index, 1);
        assert_eq!(d.name, "Intel(R) Iris(R) Xe Graphics (TGL GT2)");
        assert_eq!((d.total_mib, d.free_mib), (7917, 7125));
        let soft = "  Vulkan0: llvmpipe (LLVM 19.1.7, 256 bits) (1 MiB, 1 MiB free)";
        assert!(vulkan_device(soft, "", 0).is_none());
        let two = "  Vulkan0: Intel(R) Arc(TM) A770 Graphics (16384 MiB, 16000 MiB free)\n  Vulkan1: Intel(R) UHD Graphics 770 (7000 MiB, 6500 MiB free)";
        assert_eq!(vulkan_device(two, "intel", 1).unwrap().index, 1);
        // No fallback to an unassigned device.
        assert!(vulkan_device(two, "intel", 5).is_none());
        // BOINC's Intel device 0 is the first Intel device, not Vulkan0.
        let mixed = "  Vulkan0: NVIDIA GeForce RTX 3060 (12288 MiB, 11000 MiB free)\n  Vulkan1: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 7125 MiB free)";
        assert_eq!(vulkan_device(mixed, "intel", 0).unwrap().index, 1);
        assert!(vulkan_device(mixed, "intel", 1).is_none());
        assert_eq!(vulkan_device(mixed, "nvidia", 0).unwrap().index, 0);
        assert!(vulkan_device(mixed, "nvidia", 1).is_none());
        assert!(vulkan_device("no devices", "intel", 0).is_none());
    }

    #[cfg(unix)]
    fn fake_llama_server(slot: &Path, listing: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let bin = slot.join("llama-server");
        std::fs::write(&bin, format!("#!/bin/sh\nprintf '%s\\n' '{listing}'\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn device_probe_waits_for_slow_enumeration_but_kills_a_hung_one() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("llama-server");
        std::fs::write(&bin, "#!/bin/sh\nsleep \"$1\"\necho Vulkan0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = device_probe_within(Command::new(&bin).arg("1"), &bin, Duration::from_secs(10))
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Vulkan0");
        let started = Instant::now();
        let err = device_probe_within(
            Command::new(&bin).arg("30"),
            &bin,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(err, "GPU device probe: no answer in 300ms");
        assert!(probe_timed_out(&err));
        assert!(!probe_timed_out("GPU device probe failed: no device"));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(DEVICE_PROBE_DEADLINE > Duration::from_secs(65));
        assert!(DEVICE_PROBE_DEADLINE <= crate::server::LOAD_DEADLINE);
    }

    #[cfg(unix)]
    #[test]
    fn amd_vulkan_assignment_and_memory_are_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let mut job = write_qwen_unit(&slot);
        job.engine.models[0].min_gpu_mb = 7000;
        let listing = "  Vulkan0: NVIDIA GeForce RTX 3060 (12288 MiB, 11000 MiB free)
  Vulkan1: AMD llvmpipe (LLVM 19) (16384 MiB, 16000 MiB free)
  Vulkan2: AMD Radeon RX 9070 XT (16384 MiB, 15000 MiB free)
  Vulkan3: Intel Arc A770 (16384 MiB, 16000 MiB free)
  Vulkan4: Advanced Micro Devices, Inc. [RADV NAVI48] (16384 MiB, 6000 MiB free)";
        fake_llama_server(&slot, listing);
        use std::os::unix::fs::PermissionsExt as _;
        let helper = slot.join("usnm-amd-device-map");
        std::fs::write(
            &helper,
            "#!/bin/sh\ncase \"$2\" in 0) echo Vulkan2;; 1) echo Vulkan4;; *) exit 1;; esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        for (physical, opencl, expected) in [
            (12, 0, Some(2)),
            (0, 1, Some(4)),
            (0, 2, None),
            (-1, 0, None),
        ] {
            std::fs::write(
                slot.join("init_data.xml"),
                format!("<app_init_data><plan_class>amd_vulkan_8b</plan_class><gpu_type>ATI</gpu_type><gpu_device_num>{physical}</gpu_device_num><gpu_opencl_dev_index>{opencl}</gpu_opencl_dev_index></app_init_data>"),
            )
            .unwrap();
            let mut host = Host::default();
            let mut diag = Diag::new();
            boinc_info(&slot, &mut host, &mut diag);
            let gpu = detect_gpu(&slot, &mut host, &mut diag);
            assert_eq!(gpu.vulkan_device, expected);
            assert_eq!(gpu.cuda_device, None);
            assert_eq!(gpu.use_gpu, expected.is_some());
            assert_eq!(vulkan_refusal(&gpu, &job).is_none(), expected == Some(2));
            if expected == Some(4) {
                assert!(vulkan_refusal(&gpu, &job)
                    .unwrap()
                    .contains("6000 MiB free"));
            }
        }
        // Only the second AMD adapter remains visible. A PCI mapping of the assigned
        // first adapter must not be reinterpreted as an ordinal into this shorter list.
        fake_llama_server(
            &slot,
            "  Vulkan4: AMD Radeon RX 9070 XT (16384 MiB, 15000 MiB free)",
        );
        std::fs::write(slot.join("init_data.xml"), "<app_init_data><plan_class>amd_vulkan_8b</plan_class><gpu_type>ATI</gpu_type><gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index></app_init_data>").unwrap();
        let mut host = Host::default();
        let mut diag = Diag::new();
        boinc_info(&slot, &mut host, &mut diag);
        assert!(!detect_gpu(&slot, &mut host, &mut diag).use_gpu);
        fake_llama_server(&slot, listing);
        for response in ["Vulkan1", "Vulkan0", "Vulkan3", "Vulkan999", "ambiguous"] {
            std::fs::write(&helper, format!("#!/bin/sh\necho {response}\n")).unwrap();
            assert!(!detect_gpu(&slot, &mut host, &mut diag).use_gpu);
        }
        std::fs::remove_file(&helper).unwrap();
        assert!(!detect_gpu(&slot, &mut host, &mut diag).use_gpu);
        std::fs::write(&helper, "#!/bin/sh\necho Vulkan2\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        for name in [
            "AMD Radeon RX 9070 XT",
            "Radeon RX 9070 XT (RADV NAVI48)",
            "Advanced Micro Devices, Inc.",
            "ATI Radeon",
        ] {
            let line = format!("  Vulkan7: {name} (16384 MiB, 15000 MiB free)");
            assert_eq!(vulkan_device(&line, "amd", 0).unwrap().index, 7);
            assert!(vulkan_device(&line, "amd", 1).is_none());
        }
        for name in [
            "AMD lavapipe",
            "Radeon SwiftShader",
            "Intel Arc",
            "NVIDIA RTX",
        ] {
            let line = format!("  Vulkan0: {name} (16384 MiB, 15000 MiB free)");
            assert!(vulkan_device(&line, "amd", 0).is_none());
        }
        for assignment in [
            "<gpu_type>NVIDIA</gpu_type><gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_type>ATI</gpu_type><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_type>ATI</gpu_type><gpu_device_num>invalid</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_type>ATI</gpu_type><gpu_device_num>0</gpu_device_num>",
            "<gpu_type>ATI</gpu_type><gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>-1</gpu_opencl_dev_index>",
        ] {
            let init = format!(
                "<app_init_data><plan_class>amd_vulkan_8b</plan_class>{assignment}</app_init_data>"
            );
            std::fs::write(slot.join("init_data.xml"), init).unwrap();
            let mut host = Host::default();
            let mut diag = Diag::new();
            boinc_info(&slot, &mut host, &mut diag);
            let gpu = detect_gpu(&slot, &mut host, &mut diag);
            assert!(!gpu.use_gpu);
            assert!(vulkan_refusal(&gpu, &job).is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn vulkan_plan_class_uses_the_listed_device_and_refuses_too_little_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_qwen_unit(&slot);
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>intel_vulkan_8b</plan_class><gpu_type>intel_gpu</gpu_type></app_init_data>",
        )
        .unwrap();
        fake_llama_server(
            &slot,
            "  Vulkan0: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 6000 MiB free)",
        );
        let mut host = Host::default();
        let mut diag = Diag::new();
        boinc_info(&slot, &mut host, &mut diag);
        let gpu = detect_gpu(&slot, &mut host, &mut diag);
        assert!(gpu.use_gpu);
        assert_eq!(gpu.device_label, "vulkan");
        assert_eq!((gpu.vulkan_device, gpu.free_mb), (Some(0), Some(6000)));
        assert_eq!(host.gpu_mem_total_mib, Some(7917));
        let mut big = job.clone();
        big.engine.models[0].min_gpu_mb = 7000;
        assert!(vulkan_refusal(&gpu, &big)
            .unwrap()
            .contains("6000 MiB free"));
        big.engine.models[0].min_gpu_mb = 5000;
        assert!(vulkan_refusal(&gpu, &big).is_none());

        // Only a software device: no GPU, and the unit is refused rather than run on the CPU.
        fake_llama_server(
            &slot,
            "  Vulkan0: llvmpipe (LLVM 19.1.7, 256 bits) (15834 MiB, 15000 MiB free)",
        );
        let gpu = detect_gpu(&slot, &mut host, &mut diag);
        assert!(!gpu.use_gpu);
        assert!(vulkan_refusal(&gpu, &job)
            .unwrap()
            .contains("no hardware Vulkan device"));
    }
}
