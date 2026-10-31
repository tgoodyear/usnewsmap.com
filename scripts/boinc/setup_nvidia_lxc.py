#!/usr/bin/env python3
"""Repair native NVIDIA/Vulkan userspace inside an existing Debian/Ubuntu LXC.

Run without --apply to inspect the APT plan. Never installs a kernel driver,
Docker, or Podman, changes GPU device permissions, or modifies BOINC preferences.
The Proxmox host must already pass its NVIDIA devices through to the container.
"""
from __future__ import annotations

import argparse
import grp
import os
from pathlib import Path
import platform
import pwd
import re
import shlex
import shutil
import stat
import subprocess
import tempfile


PIN = Path("/etc/apt/preferences.d/usnm-nvidia-lxc")
PIN_HEADER = "# Managed by setup_nvidia_lxc.py; rerun after updating the host driver.\n"
COMMON = ["libvulkan1", "vulkan-tools", "libegl1", "libgl1", "libgomp1"]
CUDA_DRIVER_PROBE = """
import ctypes
cuda = ctypes.CDLL("libcuda.so.1")
cuda.cuInit.argtypes = [ctypes.c_uint]
cuda.cuInit.restype = ctypes.c_int
status = cuda.cuInit(0)
if status:
    raise SystemExit(f"CUDA driver cuInit failed: {status}")
count = ctypes.c_int()
cuda.cuDeviceGetCount.argtypes = [ctypes.POINTER(ctypes.c_int)]
cuda.cuDeviceGetCount.restype = ctypes.c_int
status = cuda.cuDeviceGetCount(ctypes.byref(count))
if status or count.value < 1:
    raise SystemExit(f"CUDA driver device enumeration failed: status={status}, count={count.value}")
print(f"CUDA driver API initialized: {count.value} device(s); no CUDA toolkit required.")
"""


def run(command, timeout=120):
    result = subprocess.run(command, text=True, capture_output=True, timeout=timeout,
                            env=os.environ | {"LC_ALL": "C", "DEBIAN_FRONTEND": "noninteractive",
                                              "NEEDRESTART_MODE": "l"})
    if result.returncode:
        raise RuntimeError(f"{shlex.join(command)} failed ({result.returncode}):\n"
                           f"{result.stdout}{result.stderr}")
    return result.stdout


def distribution():
    for name in ("/etc/os-release", "/usr/lib/os-release"):
        try:
            lines = Path(name).read_text().splitlines()
        except FileNotFoundError:
            continue
        values = [shlex.split(line[3:], comments=True) for line in lines if line.startswith("ID=")]
        if len(values) != 1 or len(values[0]) != 1:
            raise ValueError(f"Expected one distribution ID in {name}")
        return values[0][0]
    raise ValueError("Cannot identify the distribution: no os-release file")


def host_driver(text):
    match = re.search(r"^NVRM version:.*?\b(\d{3,4}\.\d+(?:\.\d+)?)\b", text, re.MULTILINE)
    if not match:
        raise ValueError("Cannot identify the loaded host driver in /proc/driver/nvidia/version. "
                         "Ask the Proxmox operator to expose the host NVIDIA driver and devices.")
    return match[1]


def upstream(version):
    return version.split(":", 1)[-1].split("-", 1)[0]


def package_families(system, driver):
    branch = driver.split(".", 1)[0]
    if system == "ubuntu":
        # NVIDIA's 590+ repository consolidated nvidia-smi into libnvidia-compute.
        return [
            ["libnvidia-compute", "libnvidia-gl"],
            [f"libnvidia-compute-{branch}", f"libnvidia-gl-{branch}", f"nvidia-utils-{branch}"],
            [f"libnvidia-compute-{branch}-server", f"libnvidia-gl-{branch}-server",
             f"nvidia-utils-{branch}-server"],
        ]
    return [["libcuda1", "libnvidia-ml1", "nvidia-smi", "nvidia-vulkan-icd",
             "nvidia-egl-icd", "libglx-nvidia0", "libegl-nvidia0"]]


def select_packages(families, driver, available):
    versions = {}
    for line in available.splitlines():
        fields = [field.strip() for field in line.split("|")]
        if len(fields) >= 3 and upstream(fields[1]) == driver:
            versions.setdefault(fields[0].split(":", 1)[0], fields[1])
    for family in families:
        if all(name in versions for name in family):
            return [f"{name}={versions[name]}" for name in family]
    raise ValueError(f"No complete userspace package set matching host driver {driver} is available. "
                     "Refresh APT indexes and use official NVIDIA packages for this container's "
                     "distribution, or have the host operator align its driver with an available "
                     "version. Refusing a different patch version or an unverified .run installer.")


def check_transaction(text, driver):
    for line in text.splitlines():
        if line.startswith("Remv "):
            raise ValueError(f"Refusing package removal: {line}")
        if not line.startswith(("Inst ", "Conf ")):
            continue
        name = line.split()[1].split(":", 1)[0]
        if (re.search(r"(?:^|-)dkms(?:-|$)", name) or name.startswith((
                "linux-image", "linux-headers", "linux-modules", "linux-generic",
                "nvidia-dkms", "nvidia-kernel", "nvidia-driver", "nvidia-headless",
                "nvidia-open", "cuda-drivers"))
                or (name.startswith("nvidia-")
                    and re.search(r"-(?:kernel|driver|headless|open)(?:-|$)", name))):
            raise ValueError(f"Refusing host/kernel driver package inside LXC: {name}")
        match = re.search(r"\((\S+)", line)
        if not match:
            raise ValueError(f"Cannot inspect APT installation/configuration entry: {line}")
        version = upstream(match[1])
        # Reject transitional packages pulling another driver, but not independently
        # versioned EGL/GBM/Wayland adapter libraries (1.x).
        if ("nvidia" in name or name == "libcuda1") and re.match(r"\d{3,4}\.", version):
            if version != driver:
                raise ValueError(f"APT would mix host driver {driver} with {name} {version}")


def write_pin(packages):
    text = PIN_HEADER + "\n".join(
        f"\nPackage: {name}\nPin: version {version}\nPin-Priority: 1001\n"
        for name, version in (package.split("=", 1) for package in packages))
    if PIN.is_symlink() or (PIN.exists() and not PIN.read_text().startswith(PIN_HEADER)):
        raise ValueError(f"Refusing to replace an unmanaged pin: {PIN}")
    with tempfile.NamedTemporaryFile(mode="w", dir=PIN.parent, prefix=".usnm-nvidia-", delete=False) as f:
        temporary = Path(f.name)
        try:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
            temporary.chmod(0o644)
            os.replace(temporary, PIN)
        finally:
            temporary.unlink(missing_ok=True)


def verify_gpu(user, driver):
    account = pwd.getpwnam(user)
    prefix = ["runuser", "--user", user, "--"]
    detected = run(prefix + ["nvidia-smi", "--query-gpu=name,driver_version",
                             "--format=csv,noheader"], timeout=60)
    rows = [line.rsplit(",", 1) for line in detected.splitlines() if line.strip()]
    if not rows or any(len(row) != 2 or row[1].strip() != driver for row in rows):
        raise ValueError(f"NVIDIA driver verification failed as {user}:\n{detected}")
    cuda = run(prefix + ["python3", "-c", CUDA_DRIVER_PROBE], timeout=60)
    with tempfile.TemporaryDirectory(prefix="usnm-nvidia-vulkan-") as directory:
        os.chown(directory, account.pw_uid, account.pw_gid)
        summary = run(prefix + ["env", f"XDG_RUNTIME_DIR={directory}", "vulkaninfo", "--summary"],
                      timeout=300)
    devices = re.split(r"\nGPU\d+:\s*\n", summary)[1:]
    if not any(re.search(r"vendorID\s*=\s*0x10de\b", device, re.IGNORECASE)
               and re.search(r"deviceType\s*=\s*PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\b", device)
               for device in devices):
        raise ValueError(f"No NVIDIA hardware Vulkan device is visible to {user}:\n{summary}")
    print(f"NVIDIA, CUDA driver API and hardware Vulkan verified as {user}:\n{detected}{cuda}{summary}")


def repair(args):
    if platform.system() != "Linux":
        raise ValueError("Run this script inside the Linux LXC, not on your workstation.")
    if run(["systemd-detect-virt", "--container"]).strip() != "lxc":
        raise ValueError("Run inside the LXC, not on the Proxmox host.")
    system = distribution()
    if system not in ("debian", "ubuntu"):
        raise ValueError("Only Debian/Ubuntu LXC userspace is supported.")
    if run(["dpkg", "--print-architecture"]).strip() != "amd64":
        raise ValueError("The native NVIDIA Vulkan app currently supports Linux amd64 only.")
    if (args.apply or args.check_only) and os.geteuid() != 0:
        raise ValueError("--apply and --check-only require root inside the LXC.")
    if pwd.getpwnam(args.boinc_user).pw_uid == 0:
        raise ValueError("Use the non-root account that runs BOINC, not root.")
    driver = host_driver(Path("/proc/driver/nvidia/version").read_text())
    devices = [Path("/dev/nvidiactl"), Path("/dev/nvidia-uvm")]
    adapters = list(Path("/dev").glob("nvidia[0-9]*"))
    if not adapters or any(not path.exists() or not stat.S_ISCHR(path.stat().st_mode)
                           for path in devices + adapters):
        raise ValueError("Missing NVIDIA character devices. The Proxmox operator must pass through "
                         "/dev/nvidiactl, /dev/nvidia-uvm and the assigned /dev/nvidiaN. "
                         "This script will not create devices or weaken their permissions.")
    if args.check_only:
        verify_gpu(args.boinc_user, driver)
        return
    if shutil.which("nvidia-uninstall"):
        raise ValueError("An NVIDIA .run installation appears present (nvidia-uninstall found). "
                         "Refusing to mix runfile and APT drivers. Use --check-only to verify "
                         "the existing stack; have the operator choose one installation method.")
    if PIN.is_symlink() or (PIN.exists() and not PIN.read_text().startswith(PIN_HEADER)):
        raise ValueError(f"Refusing to replace an unmanaged pin: {PIN}")
    if args.apply:
        run(["apt-get", "update"], timeout=600)
    families = package_families(system, driver)
    available = run(["apt-cache", "madison", *dict.fromkeys(name for family in families for name in family)])
    packages = select_packages(families, driver, available)
    install = ["apt-get", "--no-install-recommends", "--no-remove", "--allow-downgrades",
               "install", *packages, *COMMON]
    plan = run([*install[:1], "--simulate", *install[1:]])
    check_transaction(plan, driver)
    print(f"Loaded host driver: {driver}\nUserspace only: {shlex.join(packages + COMMON)}\n{plan}")
    if not args.apply:
        print("No changes made. Use --apply as root inside the LXC to install, pin and verify.")
        return
    run([*install[:1], "--yes", *install[1:]], timeout=1200)
    write_pin(packages)
    groups = [group.gr_name for group in grp.getgrall() if group.gr_name in ("video", "render")]
    if groups:
        run(["usermod", "--append", "--groups", ",".join(groups), args.boinc_user])
    run(["ldconfig"])
    verify_gpu(args.boinc_user, driver)
    print(f"Driver libraries pinned in {PIN}; rerun after a host driver update. "
          "No BOINC version, preferences or running tasks were changed.\n"
          "When current work has checkpointed, restart boinc-client to load the repaired libraries "
          "and supplementary groups. This verifies userspace, not GPU-job admission or OCR results.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--apply", action="store_true", help="install the inspected userspace-only package plan")
    mode.add_argument("--check-only", action="store_true",
                      help="verify existing NVML/CUDA-driver/Vulkan access as BOINC; do not touch APT")
    parser.add_argument("--boinc-user", default="boinc", help="existing account running the BOINC client")
    args = parser.parse_args()
    try:
        repair(args)
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
