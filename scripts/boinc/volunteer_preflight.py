#!/usr/bin/env python3
"""Fail closed before BOINC starts when its rootless Podman cannot run a bounded task."""
from __future__ import annotations

import argparse
import ast
import grp
from typing import NamedTuple
import json
import os
import pwd
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid
try:
    import qwen_disk
except ModuleNotFoundError:
    from scripts.boinc import qwen_disk

try:
    import tomllib  # type: ignore[import-not-found]
except ModuleNotFoundError:  # Python 3.9 / 3.10 fallback
    tomllib = None  # type: ignore[assignment]

MEMORY = 1536 * 1024 * 1024
PIDS = 64
CPU = "100000 100000"
WORKDIR = "/work"
DEFAULT_CREATE_ARGS = (
    "--network=none",
    "--read-only",
    "--cap-drop=ALL",
    "--cap-add=DAC_OVERRIDE",
    "--security-opt=no-new-privileges",
    "--cpus=1",
    "--memory=1536m",
    "--memory-swap=1536m",
    "--pids-limit=64",
    "--tmpfs=/tmp:rw,size=512m",
)
IMAGE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:/-]*@sha256:[0-9a-f]{64}\Z")
SIZE_RE = re.compile(r"([1-9][0-9]*)\s*([kmgKMG](?:i?[bB])?|[bB])?\Z")
PROBE = r"""set -eu
test "$(cat /work/input)" = usnm-preflight
printf 'slot readable and writable\n' > /work/output
chmod 0644 /work/output
test -f /sys/fs/cgroup/cgroup.controllers
printf 'cpu=%s\n' "$(cat /sys/fs/cgroup/cpu.max)"
printf 'memory=%s\n' "$(cat /sys/fs/cgroup/memory.max)"
printf 'swap=%s\n' "$(cat /sys/fs/cgroup/memory.swap.max)"
printf 'pids=%s\n' "$(cat /sys/fs/cgroup/pids.max)"
if touch /usnm-preflight-write 2>/dev/null; then exit 21; fi
test "$(wc -l < /proc/net/route)" -eq 1
printf 'ready=yes\n'
remaining=120
while [ "$remaining" -gt 0 ]; do sleep 1; remaining=$((remaining - 1)); done
"""


class PreflightError(RuntimeError):
    pass


class JobLimits(NamedTuple):
    workdir: str = WORKDIR
    memory: int = MEMORY
    memory_swap: int = MEMORY
    pids: int = PIDS
    cpus: int | float = 1
    use_gpu: bool = False
    create_args: tuple[str, ...] = DEFAULT_CREATE_ARGS
    disk_cold_bytes: int = 2 * 1024 ** 3
    disk_warm_bytes: int = 2 * 1024 ** 3


DEFAULT_LIMITS = JobLimits()


def _strip_toml_comment(line: str) -> str:
    in_single = False
    in_double = False
    escaped = False
    for index, ch in enumerate(line):
        if escaped:
            escaped = False
            continue
        if ch == "\\" and in_double:
            escaped = True
            continue
        if ch == "'" and not in_double:
            in_single = not in_single
        elif ch == '"' and not in_single:
            in_double = not in_double
        elif ch == "#" and not in_single and not in_double:
            return line[:index]
    if in_single or in_double or escaped:
        raise ValueError(f"Unterminated string literal in TOML line: {line!r}")
    return line


def _parse_toml_fallback(raw: str) -> dict[str, object]:
    """Minimal top-level TOML parser for Python 3.9/3.10 where tomllib is absent."""
    result: dict[str, object] = {}
    lines = [_strip_toml_comment(line).strip() for line in raw.splitlines()]
    index = 0
    while index < len(lines):
        stmt = lines[index]
        index += 1
        if not stmt:
            continue
        if stmt.startswith("["):
            raise ValueError(f"Unsupported TOML section header: {stmt!r}")
        if "=" not in stmt:
            raise ValueError(f"Malformed TOML assignment: {stmt!r}")
        key, raw_val = stmt.split("=", 1)
        key = key.strip()
        raw_val = raw_val.strip()
        if not re.fullmatch(r"[A-Za-z0-9_-]+", key):
            raise ValueError(f"Invalid TOML key: {key!r}")
        if key in result:
            raise ValueError(f"Duplicate TOML key: {key!r}")
        if raw_val.startswith("["):
            parts = [raw_val]
            while not parts[-1].endswith("]"):
                if index >= len(lines):
                    raise ValueError(f"Unterminated TOML array for {key!r}")
                next_line = lines[index]
                index += 1
                if next_line:
                    parts.append(next_line)
            joined = " ".join(parts)
            try:
                parsed = ast.literal_eval(joined)
            except (SyntaxError, ValueError) as error:
                raise ValueError(f"Malformed TOML array for {key!r}: {error}") from error
            if not isinstance(parsed, list) or not all(isinstance(item, str) for item in parsed):
                raise ValueError(f"TOML array for {key!r} must contain only strings")
            result[key] = parsed
        elif raw_val == "true":
            result[key] = True
        elif raw_val == "false":
            result[key] = False
        elif re.fullmatch(r"-?[0-9]+", raw_val):
            result[key] = int(raw_val)
        elif re.fullmatch(r"-?[0-9]+\.[0-9]+", raw_val):
            result[key] = float(raw_val)
        elif (raw_val.startswith('"') and raw_val.endswith('"')) or (
            raw_val.startswith("'") and raw_val.endswith("'")
        ):
            try:
                parsed_str = ast.literal_eval(raw_val)
            except (SyntaxError, ValueError) as error:
                raise ValueError(f"Malformed TOML string for {key!r}: {error}") from error
            if not isinstance(parsed_str, str):
                raise ValueError(f"Expected string value for {key!r}")
            result[key] = parsed_str
        else:
            raise ValueError(f"Unsupported TOML value for {key!r}: {raw_val!r}")
    return result


def _load_toml(raw: str) -> dict[str, object]:
    if tomllib is not None:
        return tomllib.loads(raw)
    return _parse_toml_fallback(raw)


def parse_bytes(raw: str) -> int:
    match = SIZE_RE.fullmatch(raw.strip())
    if not match:
        raise PreflightError(f"Invalid memory limit {raw!r} in job.toml")
    amount = int(match.group(1))
    unit = (match.group(2) or "b")[0].lower()
    factors = {"b": 1, "k": 1024, "m": 1024 ** 2, "g": 1024 ** 3}
    return amount * factors[unit]


def limits_from_job_toml(path: Path) -> JobLimits:
    try:
        text = Path(path).read_text()
        raw_toml = _load_toml(text)
    except (OSError, ValueError) as error:
        raise PreflightError(f"Cannot parse job.toml {path}: {error}") from error
    workdir = raw_toml.get("workdir", WORKDIR)
    if not isinstance(workdir, str) or not workdir.startswith("/") or workdir == "/":
        raise PreflightError(f"Invalid workdir {workdir!r} in {path}")
    use_gpu = raw_toml.get("use_gpu", False)
    if not isinstance(use_gpu, bool):
        raise PreflightError(f"Invalid use_gpu setting in {path}")
    raw_args = raw_toml.get("create_args", "")
    if isinstance(raw_args, str):
        if not raw_args.strip():
            raise PreflightError(f"Missing create_args in {path}")
        tokens = tuple(shlex.split(raw_args))
    elif isinstance(raw_args, list) and raw_args and all(isinstance(item, str) and item.strip() for item in raw_args):
        tokens = tuple(raw_args)
    else:
        raise PreflightError(f"Missing create_args in {path}")
    memory = None
    memory_swap = None
    pids = None
    cpus: int | float = 1
    has_network_none = False
    has_cap_drop_all = False
    has_no_new_privs = False
    has_read_only = False
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token == "--read-only":
            has_read_only = True
            index += 1
            continue
        if "=" in token and token.startswith("--"):
            flag, value = token.split("=", 1)
            step = 1
        elif token in ("--cpus", "--memory", "--memory-swap", "--pids-limit", "--gpus", "--device", "--cap-drop", "--security-opt", "--network"):
            if index + 1 >= len(tokens):
                raise PreflightError(f"Option {token} in {path} is missing a value")
            flag, value = token, tokens[index + 1]
            step = 2
        else:
            index += 1
            continue
        # Ensure each required option has a non-empty value when passed as --opt=val
        if flag in ("--memory", "--memory-swap", "--pids-limit", "--cpus", "--gpus", "--device", "--cap-drop", "--security-opt", "--network") and not value:
            raise PreflightError(f"Option {flag} in {path} is missing a value")
        if flag == "--memory":
            memory = parse_bytes(value)
        elif flag == "--memory-swap":
            memory_swap = parse_bytes(value)
        elif flag == "--pids-limit":
            try:
                pids = int(value)
            except ValueError as error:
                raise PreflightError(f"Invalid --pids-limit {value!r} in {path}") from error
            if pids <= 0:
                raise PreflightError(f"--pids-limit must be positive in {path}")
        elif flag == "--cpus":
            try:
                parsed_cpus = float(value)
            except ValueError as error:
                raise PreflightError(f"Invalid --cpus {value!r} in {path}") from error
            if parsed_cpus <= 0:
                raise PreflightError(f"--cpus must be positive in {path}")
            cpus = int(parsed_cpus) if parsed_cpus.is_integer() else parsed_cpus
        elif flag == "--gpus":
            if not value:
                raise PreflightError(f"Invalid --gpus value in {path}")
            use_gpu = True
        elif flag == "--device" and value.startswith("nvidia.com/gpu"):
            use_gpu = True
        elif flag == "--network" and value == "none":
            has_network_none = True
        elif flag == "--cap-drop" and value == "ALL":
            has_cap_drop_all = True
        elif flag == "--security-opt" and value == "no-new-privileges":
            has_no_new_privs = True
        index += step
    if not all([has_network_none, has_read_only, has_cap_drop_all, has_no_new_privs]):
        raise PreflightError(
            f"{path} must include --network=none, --read-only, --cap-drop=ALL, and --security-opt=no-new-privileges"
        )
    if memory is None or memory_swap is None or pids is None:
        raise PreflightError(f"{path} must specify --memory, --memory-swap, and --pids-limit")
    if memory_swap != memory:
        raise PreflightError(f"{path} must set --memory-swap equal to --memory so no extra swap is allowed")
    disk = {}
    for key in ("cold", "warm"):
        markers = re.findall(rf"^# usnm_disk_{key}_bytes=(.*)$", text, re.MULTILINE)
        if markers:
            if len(markers) != 1 or not re.fullmatch(r"[1-9][0-9]*", markers[0]):
                raise PreflightError(f"Invalid {key} disk policy in {path}")
            disk[f"disk_{key}_bytes"] = int(markers[0])
    if disk and (len(disk) != 2 or disk["disk_cold_bytes"] < disk["disk_warm_bytes"]):
        raise PreflightError(f"Incomplete/inconsistent disk policy in {path}")
    return JobLimits(
        workdir=workdir,
        memory=memory,
        memory_swap=memory_swap,
        pids=pids,
        cpus=cpus,
        use_gpu=use_gpu,
        create_args=tokens,
        **disk,
    )


def probe_script(workdir: str = WORKDIR) -> str:
    if workdir == WORKDIR:
        return PROBE
    return PROBE.replace("/work", workdir)


def run(args, timeout=30):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise PreflightError(f"Cannot run {args[0]}: {error}") from error
    if result.returncode:
        detail = (result.stderr or result.stdout).strip()[-3000:]
        raise PreflightError(f"{args[0]} {args[1]} failed: {detail}")
    return result.stdout


def check_limits(values, limits: JobLimits = DEFAULT_LIMITS):
    """Inspect kernel readings, not OCI configuration or accepted CLI arguments."""
    try:
        quota, period = values["cpu"].split()
        expected_cpus = limits.cpus if limits.cpus is not None else 1
        bounded_cpu = quota != "max" and 0 < int(quota) <= int(round(float(expected_cpus) * int(period)))
        bounded_memory = int(values["memory"]) == limits.memory
        no_extra_swap = int(values["swap"]) == limits.memory_swap - limits.memory
        bounded_pids = int(values["pids"]) == limits.pids
    except (KeyError, ValueError):
        raise PreflightError("Missing/unbounded cgroup v2 limits. Configure the BOINC user systemd session and writable cgroups.")
    if not all([bounded_cpu, bounded_memory, no_extra_swap, bounded_pids]):
        raise PreflightError(f"Kernel limits differ from the task policy: {values}. Fix cgroup delegation; do not accept work.")


def resolve_job_limits(data_dir: Path, job_toml=None, job_tomls=None) -> list[JobLimits]:
    explicit = []
    for source in (job_toml, job_tomls):
        if source is None:
            continue
        if isinstance(source, (str, Path, JobLimits)):
            explicit.append(source)
        else:
            explicit.extend(source)
    if explicit:
        return [item if isinstance(item, JobLimits) else limits_from_job_toml(Path(item)) for item in explicit]
    projects_dir = data_dir / "projects"
    if projects_dir.is_dir():
        discovered = sorted(projects_dir.rglob("job*.toml"))
        if discovered:
            return [limits_from_job_toml(path) for path in discovered]
    return [DEFAULT_LIMITS]


def build_create_args(limits: JobLimits) -> list[str]:
    args = list(limits.create_args)
    if not any(token == "--cpus" or token.startswith("--cpus=") for token in args):
        args.append(f"--cpus={limits.cpus}")
    if limits.use_gpu and not any(
        token in ("--gpus", "--device")
        or token.startswith("--gpus=")
        or token.startswith("--device=nvidia.com/gpu")
        for token in args
    ):
        args.extend(["--gpus", "all"])
    return args


def check_info(info):
    host = info.get("host", {})
    if not host.get("security", {}).get("rootless", False):
        raise PreflightError("Podman must run rootless as the BOINC service user, not root.")
    if host.get("cgroupVersion") != "v2" or host.get("cgroupManager") != "systemd":
        raise PreflightError("Rootless Podman needs cgroup v2 and its systemd cgroup manager. Use the volunteer setup helper.")


def state(name, runner):
    return json.loads(runner(["podman", "inspect", name]))[0]["State"]


def wait_state(name, status, runner):
    for _ in range(20):
        current = state(name, runner)
        if current.get("Status") == status:
            return current
        time.sleep(0.1)
    raise PreflightError(f"Podman did not enter {status}; pause/resume is unusable.")


def filesystem_id(path):
    return path.stat().st_dev


def check_disk(paths, required, disk_usage=None, co_located=False):
    """No cache discounts: image presence alone does not prove complete shared layers."""
    disk_usage = disk_usage or shutil.disk_usage
    checked = {}
    device = None
    for path in paths:
        path = Path(path)
        while not path.exists() and path != path.parent:
            path = path.parent
        if co_located:
            current = filesystem_id(path)
            if device is not None and current != device:
                raise PreflightError(
                    "Qwen image store and extraction scratch must share BOINC's filesystem "
                    "so the scheduler's reported free disk remains meaningful before each dispatch."
                )
            device = current
        free = disk_usage(path).free
        if free < required:
            raise PreflightError(
                f"Insufficient free disk at {path}: {free} bytes available, "
                f"{required} cold-cache bytes required before accepting work "
                "(includes extraction overlap, slot workspace and reserve)."
            )
        checked[str(path)] = free
    return checked


def preflight(data_dir, image, runner=None, service=None, job_toml=None, job_tomls=None):
    runner = runner or run
    if not IMAGE.fullmatch(image):
        raise PreflightError("--image must name a registry image pinned by its full sha256 digest.")
    if os.geteuid() == 0:
        raise PreflightError("Run the preflight as the BOINC service user inside its systemd service, not with sudo/root.")
    if service is not None:
        uid = os.geteuid()
        expected = {
            "HOME": str(data_dir),
            "XDG_RUNTIME_DIR": f"/run/usnm-boinc-{uid}-{service.removesuffix('.service')}",
            "DBUS_SESSION_BUS_ADDRESS": f"unix:path=/run/user/{uid}/bus",
            "CONTAINERS_CONF": f"/etc/usnm-boinc-volunteer/{service}/containers.conf",
            "CONTAINERS_STORAGE_CONF": f"/etc/usnm-boinc-volunteer/{service}/storage.conf",
        }
        if any(os.environ.get(key) != value for key, value in expected.items()):
            raise PreflightError("Managed Podman service environment was overridden or unset; resolve conflicting systemd drop-ins or EnvironmentFile settings.")
    limits_list = resolve_job_limits(data_dir, job_toml=job_toml, job_tomls=job_tomls)
    if service is not None:
        (data_dir / "usnm-podman-tmp").mkdir(mode=0o700, exist_ok=True)
    info = json.loads(runner(["podman", "info", "--format=json"]))
    check_info(info)
    slots = data_dir.resolve(strict=True) / "slots"
    if not slots.is_dir() or not os.access(slots, os.W_OK):
        raise PreflightError("The BOINC slots directory must exist and be writable by the service user.")
    required = max(limits.disk_cold_bytes for limits in limits_list)
    if image.startswith(qwen_disk.REPOSITORY + "@"):
        try:
            metadata = qwen_disk.image_metadata(image)
        except Exception as error:
            raise PreflightError(f"Cannot verify anonymous image disk metadata: {error}") from error
        if metadata is not None:
            required = max(required, metadata["container_cold_bytes"])
    store = info.get("store", {})
    # Runtime graph storage and copy/extraction scratch may be on different disks
    # from BOINC. Require the full envelope on each (intentionally conservative).
    graph = store.get("graphRoot", data_dir / "usnm-podman-storage")
    scratch = store.get("imageCopyTmpDir", os.environ.get("TMPDIR", "/var/tmp"))
    if required > 2 * 1024 ** 3 and not all(
        isinstance(store.get(key), str) and Path(store[key]).is_absolute()
        for key in ("graphRoot", "imageCopyTmpDir")
    ):
        raise PreflightError("Podman did not report absolute graphRoot and imageCopyTmpDir; cannot admit Qwen safely.")
    disk_free = check_disk([slots, graph, scratch], required, co_located=required > 2 * 1024 ** 3)
    # Work under the real slots parent; no :z/:Z shortcut that hides a broken host setup.
    # The official client also runs an unqualified hello-world during runtime detection.
    hello_name = "usnm-preflight-hello-" + uuid.uuid4().hex
    try:
        runner(["podman", "run", "--rm", "--name", hello_name, "docker.io/library/hello-world"], timeout=180)
    finally:
        cleanup_error = None
        try:
            runner(["podman", "rm", "--force", "--ignore", hello_name])
        except PreflightError as error:
            cleanup_error = error
        if cleanup_error is not None:
            raise PreflightError(f"Preflight cleanup failed; BOINC remains stopped: {cleanup_error}")
    try:
        runner(["podman", "image", "inspect", image])
    except PreflightError:
        runner(["podman", "pull", image], timeout=1800)
    last_values = {}
    for limits in limits_list:
        name = "usnm-preflight-" + uuid.uuid4().hex
        scratch = Path(tempfile.mkdtemp(prefix=".usnm-preflight-", dir=slots))
        try:
            scratch.chmod(0o771)
            input_file = scratch / "input"
            input_file.write_text("usnm-preflight\n")
            input_file.chmod(0o600)
            create_cmd = [
                "podman", "create", "--name", name, "--pull=never",
                *build_create_args(limits),
                "--log-driver=k8s-file", "--volume", f"{scratch}:{limits.workdir}",
                "--entrypoint=/bin/sh", image, "-c", probe_script(limits.workdir),
            ]
            try:
                runner(create_cmd)
                runner(["podman", "start", name])
            except PreflightError as error:
                if limits.use_gpu:
                    raise PreflightError(
                        "GPU pass-through probe (--gpus all / --device nvidia.com/gpu=all) failed; "
                        f"configure NVIDIA CDI for rootless Podman (nvidia-ctk cdi generate): {error}"
                    ) from error
                raise
            values = {}
            for _ in range(100):
                logs = runner(["podman", "logs", name])
                values = dict(line.split("=", 1) for line in logs.splitlines() if "=" in line)
                if values.get("ready") == "yes":
                    break
                if state(name, runner).get("Status") == "exited":
                    raise PreflightError(f"Slot/isolation probe failed: {logs[-2000:]}. On SELinux hosts, label only the BOINC slots as container_file_t.")
                time.sleep(0.1)
            else:
                raise PreflightError("Slot probe timed out. Inspect Podman/SELinux diagnostics before accepting work.")
            check_limits(values, limits)
            output_file = scratch / "output"
            if output_file.read_text() != "slot readable and writable\n" or output_file.stat().st_mode & 0o777 != 0o644:
                raise PreflightError("Container output is not readable by the BOINC service user with mode 0644.")
            runner(["podman", "pause", name])
            wait_state(name, "paused", runner)
            runner(["podman", "unpause", name])
            wait_state(name, "running", runner)
            last_values = values
        finally:
            # Never target a volunteer task or prune the user's runtime/storage.
            cleanup_error = None
            try:
                runner(["podman", "rm", "--force", "--ignore", name])
            except PreflightError as error:
                cleanup_error = error
            if cleanup_error is None:
                shutil.rmtree(scratch)
            else:
                raise PreflightError(f"Preflight cleanup failed; BOINC remains stopped: {cleanup_error}")
    return {"rootless": True, "slot_io": True, "pause_resume": True, "kernel_limits": last_values,
            "disk_cold_required_bytes": required, "disk_free_bytes": disk_free}


def check_ocr_groups(uid, service, runner=None, actual=False):
    runner = runner or run
    account = pwd.getpwuid(uid)
    if account.pw_name != "boinc" or uid == 0:
        raise PreflightError("OCR-node mode requires the dedicated boinc account.")
    groups = set(os.getgrouplist(account.pw_name, account.pw_gid))
    if actual:
        groups.update(os.getgroups())
        groups.add(os.getegid())
    for unit in [service, f"user@{uid}.service"]:
        if not unit:
            raise PreflightError("OCR-node group checks require the client service name.")
        for prop in ["Group", "SupplementaryGroups"]:
            for name in runner(["systemctl", "show", unit, f"--property={prop}", "--value"]).split():
                groups.add(int(name) if name.isdecimal() else grp.getgrnam(name).gr_gid)
    for name in ["root", "docker", "sudo", "wheel", "himds", "usnm", "usnmeng", "usnmocr"]:
        try:
            forbidden = grp.getgrnam(name).gr_gid
        except KeyError:
            continue
        if forbidden in groups:
            raise PreflightError(f"The dedicated BOINC client and user session must not belong to {name}.")


def check_ocr_node_slice(uid, cgroup_root=Path("/sys/fs/cgroup"), membership=None):
    if uid <= 0 or os.geteuid() != uid:
        raise PreflightError("OCR-node preflight must run as the dedicated BOINC UID.")
    membership = Path("/proc/self/cgroup").read_text() if membership is None else membership
    relative = f"user.slice/user-{uid}.slice"
    line = next((line.removeprefix("0::") for line in membership.splitlines() if line.startswith("0::")), "")
    if not line.startswith(f"/{relative}/"):
        raise PreflightError("BOINC is outside its capped dedicated user slice.")
    parent = cgroup_root / relative
    quota, period = (parent / "cpu.max").read_text().split()
    memory = (parent / "memory.max").read_text().strip()
    swap = (parent / "memory.swap.max").read_text().strip()
    tasks = (parent / "pids.max").read_text().strip()
    if (quota == "max" or int(period) <= 0 or int(quota) <= 0 or int(quota) * 4 > int(period)
            or memory == "max" or not 0 < int(memory) <= 2 * 1024**3
            or swap != "0" or tasks == "max" or not 0 < int(tasks) <= 256):
        raise PreflightError("OCR-node aggregate CPU/memory/swap/task limits are missing or weakened.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--service")
    parser.add_argument("--job-toml", type=Path, action="append", default=None)
    parser.add_argument("--ocr-node-uid", type=int)
    args = parser.parse_args()
    def interrupted(signum, frame):
        raise PreflightError("Startup preflight interrupted; no work may be accepted.")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        if args.ocr_node_uid is not None:
            check_ocr_node_slice(args.ocr_node_uid)
            check_ocr_groups(args.ocr_node_uid, args.service, actual=True)
        result = preflight(args.data_dir, args.image, service=args.service, job_tomls=args.job_toml)
    except (PreflightError, OSError, KeyError, ValueError, json.JSONDecodeError) as error:
        print(f"BOINC host preflight FAILED: {error}", file=sys.stderr)
        return 1
    print("BOINC host preflight passed: " + json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
