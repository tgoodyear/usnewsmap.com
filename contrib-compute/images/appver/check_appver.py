#!/usr/bin/env python3
"""Put the signatures beside a staged app version and check it, in the appver image build
(Containerfile). Any failure fails the build, so no publish image exists for a bad version.

  check_appver.py --version-dir DIR --sigs DIR --public-key FILE [--crypt-prog PATH]

Refuses:
- a signature in --sigs for no staged file (an orphan .sig), or any other file there;
- a staged file (other than version.xml) without a signature;
- a signature that BOINC's crypt_prog -verify rejects against the public key;
- a version.xml that names a physical_name that isn't staged;
- a version directory that holds anything but plain files.
"""
from __future__ import annotations

import argparse
from pathlib import Path
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
APPS = ("usnm_ocr", "usnm_assess", "usnm_gold", "usnm_surya", "usnm_quality", "usnm_qwen")
PLATFORMS = (
    "x86_64-pc-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "arm64-apple-darwin",
    "x86_64-apple-darwin",
    "windows_x86_64",
)


class Refused(Exception):
    pass


def _infer_app_and_platform(
    version_dir: Path,
    spec: ET.Element,
    app: str | None = None,
    platform: str | None = None,
) -> tuple[str | None, str | None]:
    resolved_app = app
    resolved_platform = platform
    parts = version_dir.parts
    if len(parts) >= 4 and parts[-4] == "apps":
        if resolved_app is None:
            resolved_app = parts[-3]
        if resolved_platform is None:
            resolved_platform = version_dir.name.split("__", 1)[0]
    if resolved_app is None or resolved_platform is None:
        mains = [f for f in spec.findall("file") if f.find("main_program") is not None]
        if mains:
            main_phys = mains[0].findtext("physical_name") or ""
            for candidate_app in APPS:
                if candidate_app in main_phys and resolved_app is None:
                    resolved_app = candidate_app
                    break
            for candidate_plat in PLATFORMS:
                if candidate_plat in main_phys and resolved_platform is None:
                    resolved_platform = candidate_plat
                    break
    return resolved_app, resolved_platform


def check(
    version_dir: Path,
    sigs: Path,
    public_key: Path,
    crypt_prog: str,
    kind: str | None = None,
    app: str | None = None,
    platform: str | None = None,
) -> list[str]:
    """Copies each NAME.sig from sigs into version_dir and checks the result. Returns the signed files."""
    if kind is not None and kind not in ("container", "native"):
        raise Refused(f"unsupported appver kind: {kind}")
    if not public_key.is_file() or public_key.stat().st_size == 0:
        raise Refused(f"no code-signing public key at {public_key}")
    for p in version_dir.iterdir():
        if not p.is_file() or p.is_symlink() or not NAME.fullmatch(p.name):
            raise Refused(f"{p.name!r} in the version directory is not a plain file")
        if p.name.endswith(".sig"):
            raise Refused(f"{p.name} was staged, not signed: signatures come only from --sigs")
    files = sorted(p.name for p in version_dir.iterdir() if p.name != "version.xml")
    if "version.xml" not in {p.name for p in version_dir.iterdir()}:
        raise Refused("no version.xml")
    if not files:
        raise Refused("no files besides version.xml")
    given = sorted(p.name for p in sigs.iterdir())
    bad = [n for n in given if not n.endswith(".sig") or not NAME.fullmatch(n)]
    if bad:
        raise Refused("not a signature: " + ", ".join(bad))
    signed = {n[: -len(".sig")] for n in given}
    orphans = sorted(signed - set(files))
    if orphans:
        raise Refused("a .sig with no staged file: " + ", ".join(o + ".sig" for o in orphans))
    unsigned = [n for n in files if n not in signed]
    if unsigned:
        raise Refused("no .sig for " + ", ".join(unsigned))
    try:
        spec = ET.parse(version_dir / "version.xml").getroot()
    except ET.ParseError as e:
        raise Refused(f"version.xml is not XML: {e}") from e
    referenced = [e.text or "" for e in spec.iter("physical_name")]
    missing = [name for name in referenced if name not in files]
    if missing:
        raise Refused("version.xml names files that are not staged: " + ", ".join(missing))
    unlisted = [name for name in files if name not in set(referenced)]
    if unlisted:
        raise Refused("staged files not in version.xml: " + ", ".join(unlisted))
    resolved_app, resolved_platform = _infer_app_and_platform(version_dir, spec, app=app, platform=platform)
    if kind == "container" and spec.find("is_wrapper") is None:
        raise Refused("container version.xml is missing <is_wrapper/>")
    if kind == "container" or spec.find("is_wrapper") is not None:
        mains = [f for f in spec.findall("file") if f.find("main_program") is not None]
        if len(mains) != 1:
            raise Refused("container version.xml must have one <main_program/>")
        main_phys = mains[0].findtext("physical_name") or ""
        if resolved_platform == "windows_x86_64" and not main_phys.endswith(".exe"):
            raise Refused("windows_x86_64 container wrapper physical_name must end with .exe")
    if kind == "native":
        if spec.find("is_wrapper") is not None:
            raise Refused("native version.xml must not contain <is_wrapper/>")
        api_version = (spec.findtext("api_version") or "").strip()
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", api_version) or int(api_version.split(".", 1)[0]) < 6:
            raise Refused("native version.xml must specify <api_version> >= 6.0.0 for boinc_mmap_file IPC")
        mains = [f for f in spec.findall("file") if f.find("main_program") is not None]
        if len(mains) != 1 or mains[0].find("copy_file") is None:
            raise Refused("native version.xml must have one <main_program/> with <copy_file/>")
        for f in spec.findall("file"):
            if f.find("main_program") is None:
                if f.find("copy_file") is None or not (f.findtext("logical_name") or "").strip():
                    raise Refused("native extra files in version.xml must have <logical_name> and <copy_file/>")
        if resolved_app == "usnm_qwen":
            present_logicals = {
                (f.findtext("logical_name") or "").strip()
                for f in spec.findall("file")
                if f.find("main_program") is None
            }
            required_server = "llama-server.exe" if resolved_platform == "windows_x86_64" else "llama-server"
            required_extras = (required_server, "llama.lock", "weights.lock")
            missing_extras = [name for name in required_extras if name not in present_logicals]
            if missing_extras:
                raise Refused(
                    "native usnm_qwen version.xml is missing required bundle files: "
                    + ", ".join(missing_extras)
                )
    for n in files:
        shutil.copyfile(sigs / (n + ".sig"), version_dir / (n + ".sig"))
        r = subprocess.run([crypt_prog, "-verify", str(version_dir / n), str(version_dir / (n + ".sig")),
                            str(public_key)], capture_output=True, text=True)
        if r.returncode != 0:
            raise Refused(f"{n}.sig does not verify against the public key: {(r.stdout + r.stderr).strip()}")
    return files


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--kind", default=None, choices=("container", "native"))
    ap.add_argument("--app", default=None, choices=APPS)
    ap.add_argument("--platform", default=None, choices=PLATFORMS)
    ap.add_argument("--version-dir", required=True, type=Path)
    ap.add_argument("--sigs", required=True, type=Path)
    ap.add_argument("--public-key", required=True, type=Path)
    ap.add_argument("--crypt-prog", default="/usr/local/boinc/lib/crypt_prog")
    a = ap.parse_args(argv)
    try:
        files = check(
            a.version_dir,
            a.sigs,
            a.public_key,
            a.crypt_prog,
            kind=a.kind,
            app=a.app,
            platform=a.platform,
        )
    except Refused as e:
        print(f"check_appver: refused {a.version_dir}: {e}", file=sys.stderr)
        return 1
    print(f"check_appver: {len(files)} files signed and verified: {', '.join(files)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
