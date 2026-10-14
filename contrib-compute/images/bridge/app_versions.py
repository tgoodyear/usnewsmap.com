#!/usr/bin/env python3
"""Install a signed app version uploaded by an operator (docs/boinc.md, "Install a signed app version").

The bridge job's install-versions step runs this, then bin/update_versions. It downloads
<app>/<version>/<platform>/ from the upload account's container with the job's managed
identity into a scratch directory and checks it:

- the names are plain file names (no directories), and version.xml is there;
- every other file has a <name>.sig beside it, and every .sig has its file;
- every .sig verifies against the project's code-signing public key (keys/code_sign_public),
  with BOINC's crypt_prog -verify, the check the client makes;
- every physical_name that version.xml names is there.

Anything else fails the step and nothing is placed. A good version goes to
<project>/apps/<app>/<version>/<platform>/ (written beside it, then renamed), with the app's
templates from the image into <project>/templates/. A version already placed with the same
bytes is left as it is; one with other bytes is refused, as scripts/boinc/install.py refuses.
"""
import argparse
import filecmp
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

# As scripts/boinc/install.py's TEMPLATES (contrib-compute/images/bridge/test_app_versions.py
# checks they agree). usnm_gold's input template is written by `usnm-boinc gold submit`.
TEMPLATES = {
    "usnm_ocr": ["usnm_ocr_in", "usnm_ocr_out"],
    "usnm_assess": ["usnm_assess_in", "usnm_assess_crash_in", "usnm_assess_out"],
    "usnm_gold": ["usnm_gold_out"],
}
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
STORAGE_API = "2023-11-03"


class Refused(Exception):
    pass


def check(stage: Path, public_key: Path, crypt_prog: str) -> list[str]:
    """The checks above, on a downloaded version. Returns the names of the signed files."""
    if not public_key.is_file() or public_key.stat().st_size == 0:
        raise Refused(f"no code-signing public key at {public_key}: nothing can be verified")
    names = sorted(p.name for p in stage.iterdir())
    for p in stage.iterdir():
        if not p.is_file() or p.is_symlink() or not NAME.fullmatch(p.name):
            raise Refused(f"{p.name!r} is not a plain file name")
    if not names:
        raise Refused("nothing was uploaded")
    if "version.xml" not in names:
        raise Refused("no version.xml")
    files = [n for n in names if n != "version.xml" and not n.endswith(".sig")]
    sigs = {n[: -len(".sig")] for n in names if n.endswith(".sig")}
    if not files:
        raise Refused("no files besides version.xml")
    unsigned = [n for n in files if n not in sigs]
    if unsigned:
        raise Refused("no .sig for " + ", ".join(unsigned))
    orphans = sorted(sigs - set(files))
    if orphans:
        raise Refused("a .sig with no file: " + ", ".join(o + ".sig" for o in orphans))
    if "version.xml" in sigs:
        raise Refused("version.xml is not signed: update_versions does not read it as a file")
    for n in files:
        r = subprocess.run([crypt_prog, "-verify", str(stage / n), str(stage / (n + ".sig")), str(public_key)],
                           capture_output=True, text=True)
        if r.returncode != 0:
            raise Refused(f"{n}.sig does not verify against {public_key}: {(r.stdout + r.stderr).strip()}")
    try:
        spec = ET.parse(stage / "version.xml").getroot()
    except ET.ParseError as e:
        raise Refused(f"version.xml is not XML: {e}") from e
    named = [e.text or "" for e in spec.iter("physical_name")]
    missing = [n for n in named if n not in files]
    if missing:
        raise Refused("version.xml names files that were not uploaded: " + ", ".join(missing))
    return files


def place(stage: Path, target: Path) -> bool:
    """Puts the checked version at target. False when the same version is already there."""
    if target.exists():
        theirs = sorted(p.name for p in target.iterdir())
        ours = sorted(p.name for p in stage.iterdir())
        if theirs == ours and all(filecmp.cmp(stage / n, target / n, shallow=False) for n in ours):
            return False
        raise Refused(f"{target} exists with other files; a changed version needs a new version number")
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = Path(tempfile.mkdtemp(prefix=f".{target.name}.", dir=target.parent))
    try:
        for p in stage.iterdir():
            shutil.copyfile(p, tmp / p.name)
            # The wrapper is the main program; update_versions records the mode it finds.
            (tmp / p.name).chmod(0o755 if p.name.startswith("docker_wrapper") else 0o644)
        tmp.chmod(0o755)
        tmp.rename(target)
    except BaseException:
        shutil.rmtree(tmp, ignore_errors=True)
        raise
    return True


def copy_templates(app: str, templates: Path, project: Path) -> None:
    (project / "templates").mkdir(exist_ok=True)
    for name in TEMPLATES[app]:
        shutil.copyfile(templates / name, project / "templates" / name)


def token() -> str:
    endpoint, header = os.environ.get("IDENTITY_ENDPOINT"), os.environ.get("IDENTITY_HEADER")
    if not endpoint or not header:
        raise Refused("no managed identity endpoint in the environment")
    q = urllib.parse.urlencode({"api-version": "2019-08-01", "resource": "https://storage.azure.com/",
                                "client_id": os.environ["AZURE_CLIENT_ID"]})
    req = urllib.request.Request(f"{endpoint}?{q}", headers={"X-IDENTITY-HEADER": header})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)["access_token"]


def download(account: str, container: str, prefix: str, dest: Path) -> None:
    """Every blob under prefix into dest, by its name after the prefix."""
    auth = {"Authorization": "Bearer " + token(), "x-ms-version": STORAGE_API}
    base = f"https://{account}.blob.core.windows.net/{container}"
    marker = ""
    while True:
        q = {"restype": "container", "comp": "list", "prefix": prefix}
        if marker:
            q["marker"] = marker
        with urllib.request.urlopen(urllib.request.Request(base + "?" + urllib.parse.urlencode(q), headers=auth),
                                    timeout=60) as r:
            root = ET.fromstring(r.read())
        for b in root.iter("Blob"):
            name = b.findtext("Name") or ""
            rest = name[len(prefix):]
            if "/" in rest or not NAME.fullmatch(rest):
                raise Refused(f"{name!r} is not a plain file name under {prefix}")
            out = dest / rest
            url = base + "/" + urllib.parse.quote(name)
            with urllib.request.urlopen(urllib.request.Request(url, headers=auth), timeout=300) as r, \
                    open(out, "wb") as f:
                shutil.copyfileobj(r, f)
        marker = root.findtext("NextMarker") or ""
        if not marker:
            return


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--account", required=True, help="the upload storage account")
    ap.add_argument("--container", default="app-versions")
    ap.add_argument("--app", required=True, choices=sorted(TEMPLATES))
    ap.add_argument("--version", required=True)
    ap.add_argument("--platform", default="x86_64-pc-linux-gnu__docker")
    ap.add_argument("--project", required=True, type=Path)
    ap.add_argument("--templates", required=True, type=Path, help="the app templates in the image")
    ap.add_argument("--crypt-prog", default="/usr/local/boinc/lib/crypt_prog")
    a = ap.parse_args(argv)
    if not re.fullmatch(r"[1-9][0-9]*\.[0-9]+", a.version):
        print("--version must look like 1.0", file=sys.stderr)
        return 2
    if not re.fullmatch(r"[a-z0-9_]+-[a-z0-9_-]+(__[a-z0-9_]+)?", a.platform):
        print("--platform must be a BOINC platform, optionally with __<plan class>", file=sys.stderr)
        return 2
    prefix = f"{a.app}/{a.version}/{a.platform}/"
    target = a.project / "apps" / a.app / a.version / a.platform
    with tempfile.TemporaryDirectory() as tmp:
        stage = Path(tmp)
        try:
            download(a.account, a.container, prefix, stage)
            files = check(stage, a.project / "keys" / "code_sign_public", a.crypt_prog)
            placed = place(stage, target)
        except Refused as e:
            print(f"app_versions: refused {prefix}: {e}", file=sys.stderr)
            return 1
    copy_templates(a.app, a.templates, a.project)
    print(f"app_versions: {'placed' if placed else 'already placed'} {target} "
          f"({len(files)} signed files: {', '.join(files)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
