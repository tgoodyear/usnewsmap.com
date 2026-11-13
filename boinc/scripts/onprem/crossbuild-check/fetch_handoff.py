#!/usr/bin/env python3
"""Read-only pre-step for the cross-build check: copy recent native results out of the Azure handoff
store into a local directory with the layout the check expects.

Uses `az storage blob download-batch --auth-mode login` with this machine's own identity (the one the
BOINC controller reads the store with). No SAS, keys or connection strings; nothing is ever written
to the store. All result manifests are downloaded to a staging directory in one call;
those from the selected engine images completed in the window are kept, newest first up to --max-jobs, and their spool/<job>/ files and dispatch/jobs/<job>/request.json are copied
into --dest.

Selection matches the check instance it feeds: with no --image the recorded Windows builds
(native-build-trust.json); with --image a digest prefix, the recorded builds it matches (any platform).
With --host only jobs whose spool/<job>/host_id is that host. The handoff data carries no plan name,
so there is no plan selector: the plan is whatever the --image and --host pair identifies.

Run as root (ExecStartPre=+); the result is chowned to --owner.
"""
import argparse, datetime as dt, importlib.util, json, os, shutil, subprocess, sys, tempfile
from pathlib import Path

ACCOUNT, CONTAINER = "stusnmoffapzy", "boinc-handoff"


def az_download(pattern, dest):
    cmd = ["az", "storage", "blob", "download-batch", "--auth-mode", "login", "--account-name", ACCOUNT,
           "--source", CONTAINER, "--destination", str(dest), "--pattern", pattern, "--overwrite", "true",
           "--only-show-errors", "-o", "none"]
    subprocess.run(cmd, check=True, timeout=600)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dest", type=Path, required=True)
    p.add_argument("--check-script", type=Path, required=True)
    p.add_argument("--trust-file", type=Path, required=True)
    p.add_argument("--days", type=int, default=2)
    p.add_argument("--image", help="digest prefix of the engine build to keep (default: the Windows builds)")
    p.add_argument("--host", action="append", default=[], help="keep only this host id (repeatable)")
    p.add_argument("--max-jobs", type=int, default=30)
    p.add_argument("--owner", default="usnm-crossbuild")
    a = p.parse_args()
    spec = importlib.util.spec_from_file_location("check", a.check_script)
    check = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(check)
    images = check.selected_images(a.trust_file, a.image)
    now = dt.datetime.now(dt.timezone.utc)
    a.dest.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix="xb-stage-", dir=a.dest.parent))
    try:
        az_download("spool/*/*__file_result.json", stage)  # one call: the manifests are small
        if a.host:
            az_download("spool/*/host_id", stage)
        keep = []
        for f in (stage / "spool").glob("*/*__file_result.json") if (stage / "spool").is_dir() else []:
            try:
                r = json.loads(f.read_text())
                done = dt.datetime.fromisoformat(r["completed_at"])
                if r["engine"]["image"] not in images or now - done > dt.timedelta(days=a.days):
                    continue
                if a.host and (f.parent / "host_id").read_text().strip() not in a.host:
                    continue
                keep.append((done, f.parent.name))
            except (OSError, KeyError, ValueError):
                continue
        keep = [j for _, j in sorted(keep, reverse=True)[: a.max_jobs]]
        new = a.dest.parent / (a.dest.name + ".new")
        shutil.rmtree(new, ignore_errors=True)
        (new / "spool").mkdir(parents=True)
        (new / "dispatch/jobs").mkdir(parents=True)
        for job in keep:
            az_download(f"spool/{job}/*", new)
            az_download(f"dispatch/jobs/{job}/request.json", new)
        subprocess.run(["chown", "-R", f"{a.owner}:", str(new)], check=True)
        shutil.rmtree(a.dest, ignore_errors=True)
        new.rename(a.dest)
        print(f"handoff: {len(keep)} jobs (image {a.image or 'windows'}, hosts {a.host or 'any'}) from the last {a.days} days copied to {a.dest}")
    finally:
        shutil.rmtree(stage, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
