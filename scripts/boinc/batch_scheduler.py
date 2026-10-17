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


COMMON = {"schema_version", "env", "account", "plan", "manifest", "image", "push_command",
          "scans_root", "manifests_root", "batch_size", "max_pending", "pages_per_host"}
PAGES_FIELDS = COMMON | {"batch", "pages"}
QUEUE_FIELDS = COMMON | {"source", "claim_queues", "lease_hours"}
QUEUES = ("ocr-bulk", "ocr-priority")
PAGE_KEY = re.compile(r"[A-Za-z0-9_-]+/\d{4}-\d{2}-\d{2}/ed-\d+/seq-\d+")


def load_config(path):
    config = json.loads(path.read_text())
    queue = config.get("source") == "queue"
    require(set(config) == (QUEUE_FIELDS if queue else PAGES_FIELDS) and config["schema_version"] == 1,
            "invalid config fields/version")
    for key, pattern in (
        ("env", r"[a-z][a-z0-9-]{0,30}"), ("account", r"[a-z0-9]{3,24}"),
        ("plan", r"[A-Za-z0-9-]{1,64}"),
        ("image", r"sha256:[0-9a-f]{64}"),
        ("manifest", r"[A-Za-z0-9][A-Za-z0-9._/-]*"),
        ("scans_root", r"/[A-Za-z0-9_./-]+"), ("manifests_root", r"/[A-Za-z0-9_./-]+"),
    ):
        require(isinstance(config[key], str) and re.fullmatch(pattern, config[key])
                and ".." not in config[key], f"invalid {key}")
    require(not config["manifest"].startswith("/") and
            config["manifest"].endswith("/manifest.parquet"), "invalid manifest snapshot path")
    for key, maximum in (("batch_size", 100), ("max_pending", 10000), ("pages_per_host", 100)):
        require(type(config[key]) is int and 1 <= config[key] <= maximum, f"invalid {key}")
    command = config["push_command"]
    require(isinstance(command, list) and command and
            all(isinstance(arg, str) and re.fullmatch(r"[A-Za-z0-9_/@=.:+-]+", arg)
                for arg in command), "push_command must be a trusted, literal argv prefix")
    if queue:
        # BOINC is a second consumer of the work queues, beside the on-prem nodes
        # (docs/boinc-batch-scheduler.md#queue-fed-work). The page list is whatever it claims.
        require(config["source"] == "queue", "invalid source")
        queues = config["claim_queues"]
        require(isinstance(queues, list) and queues and len(set(queues)) == len(queues)
                and all(q in QUEUES for q in queues), "claim_queues is ocr-bulk and/or ocr-priority")
        require(type(config["lease_hours"]) is int and 1 <= config["lease_hours"] <= 168,
                "lease_hours is 1 to 168")
        config["pages"] = []
        return config
    for key, pattern in (("batch", r"[a-z]{2,10}_[A-Za-z0-9_]+"),):
        require(isinstance(config[key], str) and re.fullmatch(pattern, config[key])
                and ".." not in config[key], f"invalid {key}")
    require(len(config["batch"]) <= 128 and not re.search(r"_ver[0-9]+$", config["batch"]),
            "batch must be an unversioned base batch of at most 128 bytes")
    require(isinstance(config["pages"], list) and 1 <= len(config["pages"]) <= MAX_JOBS,
            "allocate 1-10000 explicit pages")
    seen = set()
    for page in config["pages"]:
        require(set(page) == {"page", "scan"}, "invalid page fields")
        require(isinstance(page["page"], str) and PAGE_KEY.fullmatch(page["page"]),
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


class ClaimFailed(RuntimeError):
    """The claim step failed after taking messages: `claims` are the ones it did take."""

    def __init__(self, message, claims):
        super().__init__(message)
        self.claims = claims


class Transport:
    def __init__(self, config, directory):
        self.config = config
        self.directory = directory

    def run(self, argv, label, timeout=2100, ok=(0,)):
        try:
            done = subprocess.run(argv, check=False, text=True, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=timeout, cwd=REPO)
        except (OSError, subprocess.SubprocessError) as error:
            raise RuntimeError(f"{label} failed; inspect the execution privately before retrying") from error
        if done.returncode not in ok:
            raise RuntimeError(f"{label} failed; inspect the execution privately before retrying")
        return done.stdout

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

    def push_args(self):
        return self.config["push_command"] + [
            "--account", self.config["account"], "--scans-root", self.config["scans_root"],
            "--manifests-root", self.config["manifests_root"],
            "--manifest", self.config["manifests_root"].rstrip("/") + "/" + self.config["manifest"],
        ]

    def export(self, pages, job_ids=()):
        queue = self.config.get("source") == "queue"
        if not queue:
            argv = self.push_args()
            for page in pages:
                argv += ["--scan", page["scan"]]
            self.run(argv, "cached scan push")
        # The exporter takes one base batch at a time; queued pages come from many.
        batches = {}
        for page in pages:
            batches.setdefault(page.get("batch") or self.config["batch"], []).append(page)
        for batch, group in sorted(batches.items()):
            variables = dict(
                HANDOFF_PAGES=",".join(page["page"] for page in group),
                HANDOFF_BATCH=batch, HANDOFF_PLAN=self.config["plan"],
                HANDOFF_MANIFEST=self.config["manifest"], ENGINE_IMAGE=self.config["image"],
                HANDOFF_SCANS=",".join(page["scan"][len(self.config["scans_root"].rstrip("/")) + 1:]
                                       for page in group),
                HANDOFF_JOBS=",".join(job_ids))
            if queue:
                variables["HANDOFF_CLAIMS"] = "1"
            self.step("handoff", "export", **variables)

    def claim(self, count):
        """Take up to `count` usable pages off the work queues (the hand-off job's claim step)."""
        nonce = uuid.uuid4().hex
        failure = None
        try:
            self.step("handoff", "claim", HANDOFF_MANIFEST=self.config["manifest"], CLAIM_NONCE=nonce,
                      CLAIM_MAX=count, CLAIM_QUEUES=",".join(self.config["claim_queues"]),
                      CLAIM_LEASE_HOURS=self.config["lease_hours"])
        except RuntimeError as error:
            # Messages taken before the failure are hidden now and their claims are written:
            # record them if the job got as far as its summary, then stop the tick.
            failure = error
        try:
            summary = self.blob("scheduler/claims.json")
            require(summary.get("schema_version") == 1 and summary.get("nonce") == nonce,
                    "stale/concurrent claim summary; another controller may be active")
        except (RuntimeError, ValueError):
            if failure:
                raise failure
            raise
        if failure:
            raise ClaimFailed(str(failure), summary["claims"]) from failure
        return summary["claims"]

    def push_pages(self, bases):
        """Push the cached scans of claimed pages, finding each one's format on the scan server.
        Returns ({base: path under scans/}, [bases with no cached scan])."""
        argv = self.push_args()
        for base in bases:
            # The script names a page by where the fetcher files it under keep/, without "keep/".
            require(base.startswith("keep/"), "claimed scan is not under keep/")
            argv += ["--keep-page", base[len("keep/"):]]
        output = self.run(argv, "cached scan push", ok=(0, 3))
        pushed, missing = {}, []
        for line in output.splitlines():
            kind, _, value = line.partition(" ")
            if kind == "pushed":
                pushed[value] = value
            elif kind == "missing":
                missing.append("keep/" + value)
        found = {}
        for base in bases:
            match = [rel for rel in pushed if rel.startswith(f"{base}.")]
            if match:
                found[base] = match[0]
        require(set(found) | set(missing) == set(bases) and not (set(found) & set(missing)),
                "scan push did not account for every claimed page")
        return found, missing

    def release(self, doc_ids):
        self.step("handoff", "release", HANDOFF_DOCS=",".join(doc_ids))

    def submit(self, job_id):
        self.step("bridge", "submit-remote", JOB_ID=job_id,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")

    def collect(self, job_id, spool):
        self.step("bridge", "spool-out", JOB_ID=job_id, HANDOFF_SPOOL=spool,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")
        self.step("handoff", "collect", JOB_ID=job_id)


def owned_records(config, records):
    # Each allocated page's base batch: one for the whole cohort, or the claimed page's own.
    wanted = {page["page"]: page.get("batch") or config["batch"] for page in config["pages"]}
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
        require(wanted.get(page) == job["message"]["batch"]
                and job["engine"]["image"] == config["image"],
                "plan already exists with a different allocation/image; operator migration required")
        require(page not in owned, "multiple jobs for one allocated page; reconcile before feeding")
        owned[page] = record
    return owned


BATCH = re.compile(r"[a-z]{2,10}_[A-Za-z0-9_]+")


class QueueFeed:
    """Pages claimed off the work queues, kept in the controller's state.

    The hand-off job's `claim` step takes usable messages off `ocr-bulk` (and `ocr-priority`
    when asked), as a second consumer beside the on-prem nodes. Each claimed page is recorded
    here *before* anything else is done for it, because its message is hidden from the nodes
    for the lease and only a later `collect` deletes it. A page whose scan turns out not to be
    on the scan server goes back to the queue. The record also stays after the page is
    published: its journal belongs to this plan, and `owned_records` refuses a journal for a
    page the plan doesn't know."""

    def __init__(self, config, state, transport, persist):
        self.config = config
        self.transport = transport
        self.persist = persist
        self.claims = state.setdefault("claims", {})
        require(len(self.claims) <= MAX_JOBS, "too many claimed pages; archive drained history first")

    def restore(self):
        self.config["pages"] = [
            {"page": page, "batch": claim["batch"], "scan": claim["scan"]}
            for page, claim in sorted(self.claims.items()) if claim.get("scan")]

    @staticmethod
    def checked(claim):
        require(isinstance(claim, dict) and {"page", "doc_id", "batch", "scan"} <= set(claim),
                "invalid claim")
        page, batch, scan = claim["page"], claim["batch"], claim["scan"]
        require(isinstance(page, str) and PAGE_KEY.fullmatch(page) and isinstance(batch, str)
                and BATCH.fullmatch(batch) and not re.search(r"_ver[0-9]+$", batch),
                "invalid claimed page or batch")
        lccn, date, edition, seq = page.split("/")
        require(claim["doc_id"] == page.replace("/", "_")
                and scan == f"keep/{batch}/{lccn}/{date}/{edition[3:]}/{seq[4:]}",
                "claim names a different page than its scan")
        return page, {"page": page, "doc_id": claim["doc_id"], "batch": batch,
                      "scan_base": scan, "scan": None, "message": claim.get("message_sha256")}

    def fill(self, count, selected):
        """Make `count` more pages ready: claim them (less any already claimed but not yet
        started), push their scans and give back the ones with none."""
        self.retry_releases()
        started = set(selected)
        waiting = [claim for page, claim in self.claims.items() if page not in started]
        need = max(0, count - len(waiting))
        if need:
            try:
                fresh = self.transport.claim(need)
            except ClaimFailed as error:
                # Their messages are hidden now: keep the claims that were made, then stop.
                self.record(error.claims, need)
                raise
            self.record(fresh, need)
        unpushed = {page: claim for page, claim in self.claims.items()
                    if claim.get("scan") is None and not claim.get("release")}
        if unpushed:
            found, missing = self.transport.push_pages(
                sorted(claim["scan_base"] for claim in unpushed.values()))
            root = self.config["scans_root"].rstrip("/")
            for page, claim in unpushed.items():
                if claim["scan_base"] in found:
                    claim["scan"] = f"{root}/{found[claim['scan_base']]}"
                else:
                    claim["release"] = True
            self.persist()
        self.retry_releases()
        self.restore()

    def record(self, fresh, need):
        require(isinstance(fresh, list) and len(fresh) <= need, "claimed more pages than asked")
        for item in fresh:
            page, claim = self.checked(item)
            if page in self.claims:
                # Its lease ran out while the job was pending and the same message came again,
                # now hidden for a fresh lease: the existing claim, its scan and its job stand.
                # A different message for the page is two attempts at once: stop.
                require(claim["message"] is not None and claim["message"] == self.claims[page].get("message"),
                        "claimed a page already in this plan with a different message")
                continue
            self.claims[page] = claim
        self.persist()

    def retry_releases(self):
        pages = sorted(page for page, claim in self.claims.items() if claim.get("release"))
        if pages:
            self.transport.release([self.claims[page]["doc_id"] for page in pages])
            for page in pages:
                del self.claims[page]
            self.persist()


def tick(config, state, transport, persist, now=None):
    feed = QueueFeed(config, state, transport, persist) if config.get("source") == "queue" else None
    if feed:
        feed.restore()
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
    if target and not held:
        free = max(0, target - pending)
        if feed:
            feed.fill(min(config["batch_size"], free), selected)
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
        "active_hosts": snapshot["active_hosts"], "target_pending": target,
        "observed_pending": pending, "selected": len(selected), "published": published,
        "collection_backlog": held,
        "newly_published": max(0, published - previous),
        "pages_per_hour": round(published * 3600 / max(1, now - started), 3),
        "oldest_pending_seconds": int(max((now - at for page, at in selected.items()
                                          if page not in owned or owned[page]["stage"] != "published"),
                                         default=0)),
        "backpressure": target == 0 or pending >= target or held > 0,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--state-dir", required=True, type=Path)
    parser.add_argument("--apply", action="store_true",
                        help="mutate the explicitly allocated feed; requires serialized operator approval")
    args = parser.parse_args(argv)
    try:
        config = load_config(args.config)
        if not args.apply:
            print(json.dumps({"event": "boinc_batch_plan", "apply": False, "plan": config["plan"],
                              "source": config.get("source", "pages"),
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
                event = tick(config, state, transport, lambda: save(path, state))
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
