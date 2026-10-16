#!/usr/bin/env python3
"""Single-script macOS volunteer onboarding and preflight for USNewsMap BOINC.

Supports native Apple Silicon (`arm64-apple-darwin`: `usnm_quality` CPU +
`usnm_qwen` `apple_gpu_8b` Metal GPU) and Intel Mac (`x86_64-apple-darwin`:
`usnm_quality` CPU) without requiring Docker or Podman.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

DEFAULT_PROJECT_URL = "https://boinc.usnewsmap.com/usnewsmap/"
DEFAULT_DATA_DIR = Path("/Library/Application Support/BOINC Data")
APP_RESOURCES = Path("/Applications/BOINCManager.app/Contents/Resources")

MAC_PREFS = {
    "run_on_batteries": "0",
    "run_if_user_active": "1",
    "run_gpu_if_user_active": "1",
    "idle_time_to_run": "3",
    "leave_apps_in_memory": "1",
    "max_ncpus_pct": "75",
    "cpu_usage_limit": "100",
    "ram_max_used_busy_pct": "75",
    "ram_max_used_idle_pct": "85",
    "work_buf_min_days": "0.1",
    "work_buf_additional_days": "0.25",
}

MIN_RAM_QWEN_BYTES = 6 * (1024 ** 3)
MIN_FREE_DISK_QWEN_BYTES = 12 * (1024 ** 3)
MIN_FREE_DISK_QUALITY_BYTES = 2 * (1024 ** 3)


class MacSetupError(RuntimeError):
    pass


def merge_prefs_xml(path: Path, edits: dict[str, str]) -> str:
    if path.exists():
        root = ET.parse(path).getroot()
        if root.tag != "global_preferences":
            raise MacSetupError(f"Unexpected XML root '{root.tag}' in {path}")
    else:
        root = ET.Element("global_preferences")
    for key, value in edits.items():
        existing = root.findall(key)
        if len(existing) > 1:
            raise MacSetupError(f"Duplicate <{key}> elements in {path}")
        elem = existing[0] if existing else ET.SubElement(root, key)
        elem.text = value
    ET.indent(root, space="  ")
    return ET.tostring(root, encoding="unicode") + "\n"


def detect_host_resources(data_dir: Path) -> dict[str, object]:
    arch = platform.machine()
    ram_bytes = 0
    try:
        out = subprocess.run(
            ["sysctl", "-n", "hw.memsize"],
            capture_output=True,
            text=True,
            check=True,
            timeout=10,
        ).stdout.strip()
        ram_bytes = int(out)
    except Exception:
        ram_bytes = 0

    check_dir = data_dir if data_dir.exists() else Path("/")
    free_disk_bytes = shutil.disk_usage(check_dir).free

    boinc_platform = (
        "arm64-apple-darwin" if arch == "arm64" else "x86_64-apple-darwin"
    )
    eligible_apps = ["usnm_quality (native CPU)"]
    if (
        arch == "arm64"
        and ram_bytes >= MIN_RAM_QWEN_BYTES
        and free_disk_bytes >= MIN_FREE_DISK_QWEN_BYTES
    ):
        eligible_apps.append("usnm_qwen (native apple_gpu_8b Metal GPU)")

    return {
        "arch": arch,
        "boinc_platform": boinc_platform,
        "ram_bytes": ram_bytes,
        "ram_gib": round(ram_bytes / (1024 ** 3), 2),
        "free_disk_bytes": free_disk_bytes,
        "free_disk_gib": round(free_disk_bytes / (1024 ** 3), 2),
        "eligible_apps": eligible_apps,
    }


def find_boinccmd(custom_boinccmd: Path | None = None) -> Path:
    if custom_boinccmd is not None:
        if custom_boinccmd.is_file():
            return custom_boinccmd
        raise MacSetupError(f"Specified --boinccmd not found: {custom_boinccmd}")
    bundled = APP_RESOURCES / "boinccmd"
    if bundled.is_file():
        return bundled
    which = shutil.which("boinccmd")
    if which:
        return Path(which)
    raise MacSetupError(
        "BOINC CLI (boinccmd) not found. Install BOINC Manager from "
        "https://boinc.berkeley.edu/download.php or `brew install --cask boinc`."
    )


def read_gui_rpc_password(data_dir: Path) -> str | None:
    pw_file = data_dir / "gui_rpc_auth.cfg"
    if not pw_file.is_file():
        return None
    return pw_file.read_text(encoding="utf-8", errors="replace").strip() or None


def is_project_attached(data_dir: Path, project_url: str) -> bool:
    state = data_dir / "client_state.xml"
    if not state.is_file():
        return False
    norm = project_url.rstrip("/")
    try:
        root = ET.parse(state).getroot()
        for proj in root.findall("project"):
            master = (proj.findtext("master_url") or "").strip().rstrip("/")
            if master == norm:
                return True
    except Exception:
        return False
    return False


def run_boinccmd(
    boinccmd: Path, data_dir: Path, args: list[str]
) -> subprocess.CompletedProcess[str]:
    cmd = [str(boinccmd)]
    passwd = read_gui_rpc_password(data_dir)
    if passwd:
        cmd.extend(["--passwd", passwd])
    cmd.extend(args)
    res = subprocess.run(
        cmd,
        cwd=str(data_dir) if data_dir.is_dir() else None,
        capture_output=True,
        text=True,
        timeout=30,
    )
    if res.returncode != 0:
        detail = (res.stderr or res.stdout).strip()
        raise MacSetupError(f"boinccmd {' '.join(args[:2])} failed: {detail}")
    return res


def setup_mac(
    *,
    data_dir: Path = DEFAULT_DATA_DIR,
    project_url: str = DEFAULT_PROJECT_URL,
    account_key: str | None = None,
    boinccmd_path: Path | None = None,
    apply: bool = False,
) -> dict[str, object]:
    info = detect_host_resources(data_dir)
    if info["free_disk_bytes"] < MIN_FREE_DISK_QUALITY_BYTES:
        raise MacSetupError(
            f"At least 2 GiB free disk space is required (found {info['free_disk_gib']} GiB)."
        )
    boinccmd = find_boinccmd(boinccmd_path)
    if not data_dir.is_dir():
        raise MacSetupError(
            f"BOINC Data directory not found at '{data_dir}'. Launch BOINC Manager once first."
        )

    prefs_path = data_dir / "global_prefs_override.xml"
    prefs_xml = merge_prefs_xml(prefs_path, MAC_PREFS)
    attached = is_project_attached(data_dir, project_url)

    actions: list[str] = []
    if apply:
        prefs_path.write_text(prefs_xml, encoding="utf-8")
        actions.append(f"wrote {prefs_path}")
        run_boinccmd(boinccmd, data_dir, ["--read_global_prefs_override"])
        actions.append("reloaded global_prefs_override.xml")
        if attached:
            run_boinccmd(boinccmd, data_dir, ["--project", project_url, "update"])
            actions.append(f"updated attached project {project_url}")
        elif account_key:
            run_boinccmd(
                boinccmd,
                data_dir,
                ["--project_attach", project_url, account_key.strip()],
            )
            actions.append(f"attached project {project_url}")
            attached = True
    else:
        actions.append(f"previewed {prefs_path} (pass --apply to write and reload)")

    return {
        "mode": "apply" if apply else "check-only",
        "boinccmd": str(boinccmd),
        "data_dir": str(data_dir),
        "project_url": project_url,
        "project_attached": attached,
        "host": info,
        "actions": actions,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Check or configure a macOS BOINC volunteer host for USNewsMap."
    )
    parser.add_argument(
        "--data-dir",
        type=Path,
        default=DEFAULT_DATA_DIR,
        help="BOINC Data directory (default: '/Library/Application Support/BOINC Data')",
    )
    parser.add_argument(
        "--boinccmd",
        type=Path,
        default=None,
        help="Path to boinccmd binary",
    )
    parser.add_argument(
        "--project-url",
        default=DEFAULT_PROJECT_URL,
        help=f"Project URL (default: {DEFAULT_PROJECT_URL})",
    )
    parser.add_argument(
        "--account-key",
        default=None,
        help="Optional BOINC account key to attach the host automatically when --apply is set",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Write global_prefs_override.xml, reload preferences, and attach/update project",
    )
    args = parser.parse_args(argv)
    try:
        report = setup_mac(
            data_dir=args.data_dir,
            project_url=args.project_url,
            account_key=args.account_key,
            boinccmd_path=args.boinccmd,
            apply=args.apply,
        )
    except MacSetupError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
