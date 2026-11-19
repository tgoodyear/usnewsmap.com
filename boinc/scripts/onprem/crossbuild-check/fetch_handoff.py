#!/usr/bin/env python3
"""Read-only pre-step for the cross-build check: copy recent native results out of the hand-off
store into a local directory with the layout the check expects.

Where the store is. Since the on-prem move (#1367) the hand-off is a directory on the BOINC host,
and this step reads it through that host's step runner (archive-list and archive-read, the
controller's ssh key and `step_command` in --config); --handoff auto (the default) does so when the
plan's flags file beside --config has `--ssh-reads`, as the controller's own tick does. Nothing is
ever written to the store. Before that move, and with --handoff azure, it uses
`az storage blob download-batch --auth-mode login` with this machine's own identity (the one the BOINC
controller reads the store with). No SAS, keys or connection strings.

All result manifests are staged in a staging directory; those from the selected engine images
completed in the window are kept, newest first up to --max-jobs, and their spool/<job>/ files and
dispatch/jobs/<job>/request.json are copied into --dest.

Selection matches the check instance it feeds: with no --image the recorded Windows builds
(native-build-trust.json); with --image a digest prefix, the recorded builds it matches (any platform).
With --host only jobs whose spool/<job>/host_id is that host. The handoff data carries no plan name,
so there is no plan selector: the plan is whatever the --image and --host pair identifies.

Run as root (ExecStartPre=+); the result is chowned to --owner.
"""
import argparse, concurrent.futures, datetime as dt, importlib.util, json, os, re, shutil, subprocess, sys, tempfile
from pathlib import Path

ACCOUNT, CONTAINER = "stusnmoffapzy", "boinc-handoff"
JOB_ID = re.compile(r"[A-Za-z0-9._-]+")  # job ids go into az glob patterns: no wildcards
SPOOL_OBJECT = re.compile(r"spool/([0-9a-f]{64})/([A-Za-z0-9][A-Za-z0-9._-]*)")
REQUEST_OBJECT = re.compile(r"dispatch/jobs/([0-9a-f]{64})/request\.json")


def az_download(pattern, dest):
    cmd = ["az", "storage", "blob", "download-batch", "--auth-mode", "login", "--account-name", ACCOUNT,
           "--source", CONTAINER, "--destination", str(dest), "--pattern", pattern, "--overwrite", "true",
           "--only-show-errors", "-o", "none"]
    subprocess.run(cmd, check=True, timeout=600)


class AzureSource:
    """The Azure container, through az."""

    def stage_results(self, stage, days, hosts):
        az_download("spool/*/*__file_result.json", stage)  # one call: the manifests are small
        if hosts:
            az_download("spool/*/host_id", stage)

    def copy_job(self, new, job):
        az_download(f"spool/{job}/*", new)
        az_download(f"dispatch/jobs/{job}/request.json", new)


class HostSource:
    """The hand-off root on the BOINC host, through its step runner (handoff_ssh.SshHandoff)."""

    def __init__(self, box):
        self.box = box

    def write(self, base, name, data):
        path = base / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def fetch(self, base, items):
        """Reads (name, size) pairs, several to a call, and writes each below `base`."""
        for name, (data, _) in self.box.get_many(items).items():
            self.write(base, name, data)

    def stage_results(self, stage, days, hosts):
        # A result file is written when its job is collected, after its completed_at, so a file
        # changed within the window covers every result completed in it (a day of slack).
        age = (days + 1) * 86400
        entries = self.box.list("spool/", suffix="__file_result.json", max_age_secs=age)
        if hosts:
            entries += self.box.list("spool/", suffix="host_id", max_age_secs=age)
        self.fetch(stage, [(e["name"], e["size"]) for e in entries if SPOOL_OBJECT.fullmatch(e["name"])])

    def copy_job(self, new, job):
        request = f"dispatch/jobs/{job}/request.json"
        requests = [e for e in self.box.list(request) if e["name"] == request]
        # A job without its request is no sample: fail the fetch, which keeps the previous copy.
        if not requests:
            raise RuntimeError(f"{request} is missing from the hand-off")
        entries = [e for e in self.box.list(f"spool/{job}/") if SPOOL_OBJECT.fullmatch(e["name"])] + requests
        self.fetch(new, [(e["name"], e["size"]) for e in entries])


def make_box(args):
    """The step runner's client for the plan in --config; the module is the controller checkout's."""
    path = args.scripts_dir / "handoff_ssh.py"
    spec = importlib.util.spec_from_file_location("handoff_ssh", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, json.loads(args.config.read_text())


def make_source(args):
    # With no flags file beside the config, auto keeps the Azure route without touching the
    # controller checkout or its config: an installation that has only this script and az works.
    if args.handoff == "auto" and not args.config.with_suffix(".flags").is_file():
        return AzureSource()
    if args.handoff != "azure":
        module, config = make_box(args)
        on_host, _ = module.detect(args.config)
        if args.handoff == "onprem" or on_host:
            return HostSource(module.SshHandoff(config))
    return AzureSource()


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dest", type=Path, required=True)
    p.add_argument("--check-script", type=Path, required=True)
    p.add_argument("--trust-file", type=Path, required=True)
    p.add_argument("--days", type=int, default=2)
    p.add_argument("--image", help="digest prefix of the engine build to keep (default: the Windows builds)")
    p.add_argument("--host", action="append", default=[], help="keep only this host id (repeatable)")
    p.add_argument("--max-jobs", type=int, default=30)
    p.add_argument("--owner", default="usnm-crossbuild", help="chown the result to this user (empty: leave it)")
    p.add_argument("--config", type=Path, default=Path("/etc/usnm/boinc-priority-controller.json"),
                   help="a controller plan's config: its step_command reaches the BOINC host")
    p.add_argument("--handoff", choices=("auto", "azure", "onprem"), default="auto",
                   help="where the hand-off is: the BOINC host's root, or the Azure container (auto: the "
                        "plan's flags file beside --config has --ssh-reads)")
    p.add_argument("--scripts-dir", type=Path, default=Path("/opt/usnm/boinc/scripts"),
                   help="the controller checkout's boinc/scripts, for handoff_ssh.py")
    a = p.parse_args(argv)
    spec = importlib.util.spec_from_file_location("check", a.check_script)
    check = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(check)
    images = check.selected_images(a.trust_file, a.image)
    source = make_source(a)
    now = dt.datetime.now(dt.timezone.utc)
    a.dest.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix="xb-stage-", dir=a.dest.parent))
    try:
        source.stage_results(stage, a.days, bool(a.host))
        keep = []
        for f in (stage / "spool").glob("*/*__file_result.json") if (stage / "spool").is_dir() else []:
            try:
                r = json.loads(f.read_text())
                done = dt.datetime.fromisoformat(r["completed_at"])
                if (not JOB_ID.fullmatch(f.parent.name) or r["engine"]["image"] not in images
                        or not dt.timedelta(0) <= now - done <= dt.timedelta(days=a.days)):
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
            source.copy_job(new, job)
        if a.owner:
            subprocess.run(["chown", "-R", f"{a.owner}:", str(new)], check=True)
        shutil.rmtree(a.dest, ignore_errors=True)
        new.rename(a.dest)
        print(f"handoff: {len(keep)} jobs (image {a.image or 'windows'}, hosts {a.host or 'any'}) from the last {a.days} days copied to {a.dest}")
    finally:
        shutil.rmtree(stage, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
