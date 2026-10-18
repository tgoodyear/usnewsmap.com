#!/usr/bin/env python3
"""One bounded BOINC OCR tick; offline validation unless explicitly --apply."""

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import uuid
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
ID = re.compile(r"[0-9a-f]{64}")
STAGES = {"prepared", "reserved", "submitting", "submitted", "published", "failed"}
MAX_JOBS = 10000


def require(ok, message):
    if not ok:
        raise ValueError(message)


def load_config(path):
    config = json.loads(path.read_text())
    fields = {"schema_version", "env", "account", "plan", "batch", "manifest", "image",
              "push_command", "scans_root", "manifests_root", "pages", "batch_size",
              "max_pending", "pages_per_host"}
    require(set(config) == fields and config["schema_version"] == 1, "invalid config fields/version")
    for key, pattern in (
        ("env", r"[a-z][a-z0-9-]{0,30}"), ("account", r"[a-z0-9]{3,24}"),
        ("plan", r"[A-Za-z0-9-]{1,64}"), ("batch", r"[a-z]{2,10}_[A-Za-z0-9_]+"),
        ("image", r"sha256:[0-9a-f]{64}"),
        ("manifest", r"[A-Za-z0-9][A-Za-z0-9._/-]*"),
        ("scans_root", r"/[A-Za-z0-9_./-]+"), ("manifests_root", r"/[A-Za-z0-9_./-]+"),
    ):
        require(isinstance(config[key], str) and re.fullmatch(pattern, config[key])
                and ".." not in config[key], f"invalid {key}")
    require(len(config["batch"]) <= 128 and not re.search(r"_ver[0-9]+$", config["batch"]),
            "batch must be an unversioned base batch of at most 128 bytes")
    require(not config["manifest"].startswith("/") and
            config["manifest"].endswith("/manifest.parquet"), "invalid manifest snapshot path")
    for key, maximum in (("batch_size", 100), ("max_pending", 10000), ("pages_per_host", 100)):
        require(type(config[key]) is int and 1 <= config[key] <= maximum, f"invalid {key}")
    command = config["push_command"]
    require(isinstance(command, list) and command and
            all(isinstance(arg, str) and re.fullmatch(r"[A-Za-z0-9_/@=.:+-]+", arg)
                for arg in command), "push_command must be a trusted, literal argv prefix")
    require(isinstance(config["pages"], list) and 1 <= len(config["pages"]) <= MAX_JOBS,
            "allocate 1-10000 explicit pages")
    seen = set()
    for page in config["pages"]:
        require(set(page) == {"page", "scan"}, "invalid page fields")
        require(isinstance(page["page"], str) and
                re.fullmatch(r"[A-Za-z0-9_-]+/\d{4}-\d{2}-\d{2}/ed-\d+/seq-\d+", page["page"]),
                "invalid page key")
        require(page["page"] not in seen, "duplicate page allocation")
        seen.add(page["page"])
        scan = page["scan"]
        require(isinstance(scan, str) and re.fullmatch(r"/[A-Za-z0-9_./-]+", scan)
                and ".." not in scan and scan.startswith(config["scans_root"].rstrip("/") + "/keep/")
                and scan.endswith((".jp2", ".jpg", ".jpeg", ".png")), "invalid cached scan path")
    return config


def digest(config):
    return hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest()


def save(path, state):
    staging = path.with_suffix(".new")
    with staging.open("w") as out:
        json.dump(state, out, sort_keys=True)
        out.write("\n")
        out.flush()
        os.fsync(out.fileno())
    os.replace(staging, path)
    fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class Transport:
    def __init__(self, config, directory):
        self.config = config
        self.directory = directory

    def run(self, argv, label, timeout=2100):
        try:
            return subprocess.run(argv, check=True, text=True, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=timeout, cwd=REPO).stdout
        except (OSError, subprocess.SubprocessError) as error:
            raise RuntimeError(f"{label} failed; inspect the execution privately before retrying") from error

    def az(self, *args):
        return self.run(["az", "storage", "blob", *args, "--auth-mode", "login",
                         "--account-name", self.config["account"], "--container-name",
                         "boinc-handoff", "--only-show-errors"], "hand-off storage")

    def blob(self, name):
        path = self.directory / "read.json"
        self.az("download", "--name", name, "--file", str(path), "--overwrite", "true", "-o", "none")
        require(path.stat().st_size <= 16 * 1024 * 1024, "hand-off JSON exceeds limit")
        return json.loads(path.read_text())

    @contextlib.contextmanager
    def lock(self):
        name = "scheduler/controller.lock"
        if self.az("exists", "--name", name, "--query", "exists", "-o", "tsv").strip() != "true":
            marker = self.directory / "lease-marker.json"
            marker.write_text('{"schema_version":1}\n')
            self.az("upload", "--name", name, "--file", str(marker), "--overwrite", "false", "-o", "none")
        lease = uuid.uuid4().hex
        lease = str(uuid.UUID(lease))
        self.az("lease", "acquire", "--blob-name", name, "--lease-duration", "-1",
                "--proposed-lease-id", lease, "-o", "none")
        try:
            yield
        finally:
            self.az("lease", "release", "--blob-name", name, "--lease-id", lease, "-o", "none")

    def step(self, target, step, **variables):
        argv = ["bash", str(REPO / "scripts/start-boinc-bridge.sh"), "--env", self.config["env"],
                "--target", target, "--step", step, "--wait-mins", "30"]
        for key, value in variables.items():
            argv += ["--var", f"{key}={value}"]
        self.run(argv, f"{target}/{step}")

    def inventory(self):
        nonce = uuid.uuid4().hex
        self.step("bridge", "batch-inventory", BATCH_NONCE=nonce,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")
        result = self.blob("scheduler/inventory.json")
        require(result.get("nonce") == nonce, "stale/concurrent inventory; another controller may be active")
        return result

    def records(self):
        listing = json.loads(self.az("list", "--prefix", "dispatch/jobs/", "--num-results", "*",
                                     "--query", "[].name", "-o", "json"))
        names = [name for name in listing if re.fullmatch(r"dispatch/jobs/[0-9a-f]{64}/record.json", name)]
        require(len(names) <= MAX_JOBS and len(set(names)) == len(names),
                "dispatch inventory limit/consistency failure; reconcile drained journals")
        cache = self.directory / "journal-cache"
        if cache.exists():
            shutil.rmtree(cache)
        cache.mkdir(mode=0o700)
        if names:
            pattern = "dispatch/jobs/" + "[0-9a-f]" * 64 + "/record.json"
            self.run([
                "az", "storage", "blob", "download-batch", "--source", "boinc-handoff",
                "--destination", str(cache), "--pattern", pattern, "--overwrite", "true",
                "--account-name", self.config["account"], "--auth-mode", "login",
                "--only-show-errors", "-o", "none",
            ], "batch journal inventory")
        actual = {str(path.relative_to(cache)) for path in cache.rglob("*") if path.is_file()}
        require(actual == set(names), "journal inventory changed during download; retry under the owner lock")
        records = []
        for name in sorted(names):
            path = cache / name
            require(not path.is_symlink() and path.stat().st_size <= 131072,
                    "invalid/oversized journal metadata")
            record = json.loads(path.read_text())
            require(record["job"]["id"] == name.split("/")[2], "journal filename/identity mismatch")
            records.append(record)
        return records

    def export(self, pages, job_ids=()):
        argv = self.config["push_command"] + [
            "--account", self.config["account"], "--scans-root", self.config["scans_root"],
            "--manifests-root", self.config["manifests_root"],
            "--manifest", self.config["manifests_root"].rstrip("/") + "/" + self.config["manifest"],
        ]
        for page in pages:
            argv += ["--scan", page["scan"]]
        self.run(argv, "cached scan push")
        self.step("handoff", "export", HANDOFF_PAGES=",".join(page["page"] for page in pages),
                  HANDOFF_BATCH=self.config["batch"], HANDOFF_PLAN=self.config["plan"],
                  HANDOFF_MANIFEST=self.config["manifest"], ENGINE_IMAGE=self.config["image"],
                  HANDOFF_SCANS=",".join(page["scan"][len(self.config["scans_root"].rstrip("/")) + 1:]
                                         for page in pages),
                  HANDOFF_JOBS=",".join(job_ids))

    def submit(self, job_id):
        self.step("bridge", "submit-remote", JOB_ID=job_id,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")

    def collect(self, job_id, spool):
        self.step("bridge", "spool-out", JOB_ID=job_id, HANDOFF_SPOOL=spool,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")
        self.step("handoff", "collect", JOB_ID=job_id)


def owned_records(config, records):
    wanted = {page["page"] for page in config["pages"]}
    owned = {}
    for record in records:
        require(record["stage"] in STAGES and ID.fullmatch(record["job"]["id"]),
                "invalid dispatch journal")
        job = record["job"]
        page = job["message"]["page"]
        if record["plan"] != config["plan"]:
            require(page not in wanted,
                    "page is already allocated to another plan; reconcile the allocation before export")
            continue
        require(job["message"]["batch"] == config["batch"] and page in wanted
                and job["engine"]["image"] == config["image"],
                "plan already exists with a different allocation/image; operator migration required")
        require(page not in owned, "multiple jobs for one allocated page; reconcile before feeding")
        owned[page] = record
    return owned


def tick(config, state, transport, persist, now=None, *, drain=False):
    snapshot = transport.inventory()
    now = time.time() if now is None else now
    require(snapshot["schema_version"] == 1 and -60 <= now - snapshot["created_at"] <= 3600,
            "inventory is not fresh")
    for key in ("pending_workunits", "active_hosts"):
        require(type(snapshot[key]) is int and snapshot[key] >= 0, "invalid capacity inventory")
    pending_jobs = snapshot["pending_jobs"]
    require(isinstance(pending_jobs, list) and len(pending_jobs) <= MAX_JOBS and
            all(isinstance(job_id, str) and ID.fullmatch(job_id) for job_id in pending_jobs) and
            len(set(pending_jobs)) == len(pending_jobs), "invalid pending job inventory")
    pending_jobs = set(pending_jobs)
    existing_jobs = snapshot["existing_jobs"]
    require(isinstance(existing_jobs, list) and len(existing_jobs) <= MAX_JOBS and
            all(isinstance(job_id, str) and ID.fullmatch(job_id) for job_id in existing_jobs) and
            len(set(existing_jobs)) == len(existing_jobs) and pending_jobs <= set(existing_jobs),
            "invalid existing workunit inventory")
    existing_jobs = set(existing_jobs)
    server = snapshot["jobs"]
    require(isinstance(server, dict) and len(server) <= MAX_JOBS, "invalid server job inventory")
    for job_id, job in server.items():
        require(ID.fullmatch(job_id) and isinstance(job, dict) and job.get("stage") in STAGES
                and (job.get("spool") is None or isinstance(job["spool"], str)),
                "invalid server job record")
    records = transport.records()
    owned = owned_records(config, records)
    for record in owned.values():
        job_id = record["job"]["id"]
        require(server.get(job_id, {}).get("stage") != "submitting",
                "uncertain BOINC submission; reconcile explicitly, never automatically resubmit")
        require(record["stage"] in ("published", "failed") or job_id not in existing_jobs or
                server.get(job_id, {}).get("stage") == "submitted",
                "SQL/journal mismatch; reconcile existing work before any automatic submission")
    selected = state.setdefault("selected", {})
    for page in owned:
        selected.setdefault(page, now)
    persist()
    count = 0
    for page, record in owned.items():
        job_id = record["job"]["id"]
        remote = server.get(job_id)
        require(not remote or remote["stage"] != "submitting",
                "uncertain BOINC submission; reconcile explicitly, never automatically resubmit")
        require(record["stage"] != "failed", "terminal failed page; operator investigation required")
        if record["stage"] != "published" and remote and remote["spool"] and count < config["batch_size"]:
            spool = remote["spool"]
            require(re.fullmatch(r"/[A-Za-z0-9_./-]+/results/[A-Za-z0-9_./-]+", spool)
                    and ".." not in spool, "invalid assimilator spool")
            transport.collect(job_id, spool)
            count += 1
    if count:
        records = transport.records()
        owned = owned_records(config, records)
    require(not any(record["stage"] == "failed" for record in owned.values()),
            "terminal BOINC failure; no replacement workload will be manufactured")
    target = min(config["max_pending"], snapshot["active_hosts"] * config["pages_per_host"])
    uncollected_not_pending = sum(
        record["stage"] not in ("published", "failed") and record["job"]["id"] not in pending_jobs
        for record in records
    )
    missing = [page for page in config["pages"] if page["page"] in selected and page["page"] not in owned]
    pending = snapshot["pending_workunits"] + uncollected_not_pending + len(missing)
    uncertain = any(job["stage"] == "submitting" for job in server.values())
    require(not uncertain, "uncertain server submission blocks fresh allocation")
    held = sum(record["stage"] != "published" and
               bool(server.get(record["job"]["id"], {}).get("spool")) for record in owned.values())
    if target and not held and not drain:
        free = max(0, target - pending)
        fresh = [page for page in config["pages"] if page["page"] not in selected][
            :min(config["batch_size"], free)]
        for page in fresh:
            selected[page["page"]] = now
        persist()
        export = [page for page in config["pages"] if page["page"] in selected and
                  (page["page"] not in owned or owned[page["page"]]["stage"] == "prepared")]
        if export:
            export = export[:config["batch_size"]]
            pages = {page["page"] for page in export}
            journals = [record["job"]["id"] for record in records
                        if record["job"]["message"]["page"] in pages]
            require(len(journals) <= 100, "too many existing journals for selected pages; reconcile history")
            transport.export(export, journals)
            records = transport.records()
            owned = owned_records(config, records)
        submitted = 0
        candidates = {record["job"]["id"] for record in owned.values()
                      if record["stage"] == "reserved" and record["job"]["id"] not in existing_jobs
                      and server.get(record["job"]["id"], {}).get("stage") in (None, "reserved")}
        backlog = sum(record["stage"] not in ("published", "failed") and
                      record["job"]["id"] not in pending_jobs | candidates for record in records)
        backlog += sum(page not in owned for page in selected)
        submit_limit = min(config["batch_size"],
                           max(0, target - snapshot["pending_workunits"] - backlog))
        for record in owned.values():
            job_id = record["job"]["id"]
            if record["stage"] == "reserved" and \
                    server.get(job_id, {}).get("stage") in (None, "reserved") and \
                    submitted < submit_limit:
                require(job_id not in existing_jobs,
                        "SQL/journal mismatch; reconcile existing work before any automatic submission")
                transport.submit(job_id)
                submitted += 1
    published = sum(record["stage"] == "published" for record in owned.values())
    previous = state.get("published", 0)
    started = state.setdefault("started_at", now)
    state["published"] = published
    persist()
    return {
        "event": "boinc_batch_tick", "schema_version": 1, "at": int(now), "plan": config["plan"],
        "draining": drain,
        "active_hosts": snapshot["active_hosts"], "target_pending": target,
        "observed_pending": pending, "selected": len(selected), "published": published,
        "collection_backlog": held,
        "newly_published": max(0, published - previous),
        "pages_per_hour": round(published * 3600 / max(1, now - started), 3),
        "oldest_pending_seconds": int(max((now - at for page, at in selected.items()
                                          if page not in owned or owned[page]["stage"] != "published"),
                                         default=0)),
        "backpressure": drain or target == 0 or pending >= target or held > 0,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--state-dir", required=True, type=Path)
    parser.add_argument("--apply", action="store_true",
                        help="mutate the explicitly allocated feed; requires serialized operator approval")
    parser.add_argument("--drain", action="store_true",
                        help="collect existing results without selecting, exporting or submitting work")
    args = parser.parse_args(argv)
    try:
        config = load_config(args.config)
        if not args.apply:
            print(json.dumps({"event": "boinc_batch_plan", "apply": False, "plan": config["plan"],
                              "draining": args.drain,
                              "allocated_pages": len(config["pages"]),
                              "max_pending": config["max_pending"], "config_sha256": digest(config)}))
            return 0
        args.state_dir = args.state_dir.resolve()
        args.state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        with (args.state_dir / "controller.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            path = args.state_dir / "state.json"
            state = json.loads(path.read_text()) if path.exists() else {"config_sha256": digest(config)}
            require(state["config_sha256"] == digest(config),
                    "immutable allocation changed; drain/reconcile before an operator migration")
            transport = Transport(config, args.state_dir)
            with transport.lock():
                event = tick(config, state, transport, lambda: save(path, state), drain=args.drain)
            print(json.dumps(event, sort_keys=True))
        return 0
    except (ValueError, KeyError, TypeError, OSError, RuntimeError) as error:
        # No command output, credentials, page contents or exception chains in telemetry.
        message = str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__
        print(json.dumps({"event": "boinc_batch_failure", "schema_version": 1,
                          "at": int(time.time()), "error": message}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
