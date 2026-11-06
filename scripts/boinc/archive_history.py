#!/usr/bin/env python3
"""Archive drained OCR history out of the batch controller's stores, in lockstep (#1483).

The controller, the bridge and the BOINC database each cap their history at 10,000 jobs
(docs/boinc-batch-scheduler.md#archive-drained-history-1483). This tool moves the oldest
fully published pages' history out of every store, one batch at a time, into a retained
archive directory. Nothing is deleted without a verified copy in the archive first. Every
step is a dry run unless --apply is given, and the dry run only reads.

Three steps, each on the host that holds its stores, in this order for every batch:

  dispatch  On the controller host. Plans the batch (oldest published pages first), then
            archives the pages' controller state (claims, selected, held_since,
            dead_lettered) and then all of their hand-off blobs (dispatch/jobs/<id>/,
            dispatch/quarantine/<id>/ and spool/<id>/).
  bridge    On the BOINC host. Archives the batch's bridge journals ($HANDOFF_DIR/jobs/<id>)
            and their spool outputs under the project's results/.
  db        Where the mysql client reaches the BOINC database. Exports, then deletes, the
            batch's finished workunit rows and their result rows.

Each step writes an intent file before it changes anything and a receipt when it is done, so
a step stopped half way is resumed by running it again with the same arguments.
"""

import argparse
import base64
import concurrent.futures
import contextlib
import email.utils
import fcntl
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import xml.etree.ElementTree as ET
from pathlib import Path


SCHEMA_VERSION = 1
ID = re.compile(r"[0-9a-f]{64}")
BATCH_NAME = re.compile(r"[0-9]{4}")
CONTAINER = "boinc-handoff"
STORAGE_VERSION = "2023-11-03"
LEASE_BLOB = "scheduler/controller.lock"
JOB_ROOTS = ("dispatch/jobs/", "dispatch/quarantine/", "spool/")
JOB_BLOB = re.compile(r"(?:dispatch/jobs|dispatch/quarantine|spool)/([0-9a-f]{64})/[^/].*")
RECORD_BLOB = re.compile(r"dispatch/jobs/([0-9a-f]{64})/record\.json")
REQUESTS = 16
DEFAULT_PAGES = 1000
MAX_PAGES = 5000
DEFAULT_MIN_AGE_HOURS = 24
MAX_RECORD_BYTES = 131072
STAGES = {"prepared", "reserved", "submitting", "submitted", "published", "failed"}
# The bridge's copy of a job stays `submitted` once its result is spooled out; `published` is
# the hand-off's word. Anything earlier means the server has not finished with the job.
DRAINED_SERVER_STAGES = {"submitted", "published", "failed"}
STATE_KEYS = ("claims", "selected", "held_since", "dead_lettered")
UNIT = "usnm-boinc-priority-controller"
# Blob lease states a new lease can be taken in.
FREE_LEASE_STATES = ("available", "expired", "broken")
# The bridge steps' locks on the BOINC host (scripts/boinc/onprem-step.sh).
DEFAULT_BRIDGE_LOCKS = ("/run/usnm-boinc-step.lock", "/run/usnm-boinc-handoff.lock")
SPOOL_FILE = re.compile(
    r"usnm_([0-9a-f]{64})(__file_(result\.json|text\.txt|layout\.xml)|_error|_no_output_files)")
BUNDLE_SPOOL_FILE = re.compile(
    r"usnm_ocrb_([0-9a-f]{64})(__file_(results\.json|texts|layouts)|_error|_no_output_files)")
WORKUNIT = re.compile(r"usnm_(ocrb_)?[0-9a-f]{64}")
DB_CHUNK = 500


class Refused(RuntimeError):
    """The archive can't go on safely; nothing further was changed."""


def require(ok, message):
    if not ok:
        raise Refused(message)


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_atomic(path, data):
    """Writes the bytes to a sibling, fsyncs it, then renames it into place: a file under its
    final name is always complete."""
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    partial = path.with_name(path.name + ".partial")
    # A .partial left by a crash may be a hard link to the file being archived (move): opening
    # it for writing would truncate the source too.
    partial.unlink(missing_ok=True)
    with open(partial, "wb") as out:
        out.write(data)
        out.flush()
        os.fsync(out.fileno())
    os.replace(partial, path)
    fsync_dir(path.parent)


def copy_atomic(source, path):
    """write_atomic for a file's contents, copied a chunk at a time."""
    partial = path.with_name(path.name + ".partial")
    partial.unlink(missing_ok=True)
    with open(source, "rb") as data, open(partial, "wb") as out:
        shutil.copyfileobj(data, out, 1 << 20)
        out.flush()
        os.fsync(out.fileno())
    os.replace(partial, path)
    fsync_dir(path.parent)


def dump(payload):
    return (json.dumps(payload, sort_keys=True, indent=1) + "\n").encode()


def write_json(path, payload):
    write_atomic(path, dump(payload))


def read_json(path):
    require(path.is_file() and not path.is_symlink(), f"{path} must be a regular file, not missing or a symlink")
    return json.loads(path.read_text())


def tree_digest(entries):
    """One digest for a set of archived files: sha256 over their sorted path, size and sha256."""
    lines = "".join(f"{entry['path']}\t{entry['size']}\t{entry['sha256']}\n"
                    for entry in sorted(entries, key=lambda entry: entry["path"]))
    return sha256_bytes(lines.encode())


def archived_entries(root):
    if not root.is_dir():
        return []
    return [{"path": str(path.relative_to(root)), "size": path.stat().st_size, "sha256": sha256_file(path)}
            for path in sorted(root.rglob("*")) if path.is_file() and not path.name.endswith(".partial")]


def emit(payload, stream=None):
    print(json.dumps(payload, sort_keys=True), file=stream or sys.stdout, flush=True)


def progress(message):
    emit({"event": "boinc_archive_progress", "message": message}, sys.stderr)


@contextlib.contextmanager
def flocked(paths, existing=()):
    """Holds a non-blocking exclusive flock on every path: a running holder makes this refuse.
    Every holder creates its lock file as it takes it, so a missing one is created, except one
    in `existing`: its absence means the path isn't the one its holder uses."""
    with contextlib.ExitStack() as stack:
        for path in paths:
            require(path.parent.is_dir(), f"{path.parent} is missing: is {path} the right lock?")
            require(path not in existing or path.is_file(),
                    f"{path} is missing: its holder would have created it; is the path right?")
            handle = stack.enter_context(open(path, "a"))
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise Refused(f"{path} is locked: a controller tick or bridge step is running") from None
            stack.callback(fcntl.flock, handle, fcntl.LOCK_UN)
        yield


def unit_state(unit):
    """systemctl is-active's answer for a unit (it only reads)."""
    try:
        done = subprocess.run(["systemctl", "is-active", unit], check=False, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return "unknown"
    return done.stdout.strip() or "unknown"


def controller_stopped(units):
    """The timer must not be active, and the service must not be running a tick."""
    return units["timer"] in ("inactive", "failed") and units["service"] in ("inactive", "failed")


def require_apply(args, units=None):
    require(args.confirm_controller_paused,
            "--apply needs --confirm-controller-paused: stop the controller's timer first "
            "(docs/boinc-batch-scheduler.md#archive-drained-history-1483)")
    if units is not None:
        require(controller_stopped(units),
                f"the controller is not paused (timer {units['timer']}, service {units['service']}): "
                f"systemctl stop {args.controller_unit}.timer and wait for {args.controller_unit}.service "
                "to be inactive")


# --- The hand-off container ------------------------------------------------------------------


def bare_etag(etag):
    """An ETag as a listing gives it: the header form is the same value in quotes."""
    return etag.strip('"') if etag else etag


class RestContainer:
    """The hand-off container over the storage REST API, with az's token for the identity it
    is logged in with (as the controller's --rest-journals). The token never leaves a header."""

    def __init__(self, account):
        self.base = f"https://{account}.blob.core.windows.net/{CONTAINER}"
        self._token = None

    def token(self):
        if self._token is None or self._token[1] < time.time() + 600:
            try:
                done = subprocess.run(
                    ["az", "account", "get-access-token", "--resource", "https://storage.azure.com/",
                     "--query", "{token:accessToken,expires:expires_on}", "-o", "json"],
                    check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=120)
                token = json.loads(done.stdout)
                self._token = (token["token"], int(token["expires"]))
            except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
                raise RuntimeError("could not get a storage token from az") from None
        return self._token[0]

    def request(self, method, name="", query=None, headers=None, absent=(404,)):
        url = self.base + ("/" + urllib.parse.quote(name) if name else "")
        if query:
            url += "?" + urllib.parse.urlencode(query)
        sent = {"Authorization": "Bearer " + self.token(), "x-ms-version": STORAGE_VERSION, **(headers or {})}
        data = b"" if method == "PUT" else None
        for attempt in range(3):
            try:
                with urllib.request.urlopen(urllib.request.Request(url, data=data, headers=sent, method=method),
                                            timeout=120) as response:
                    return response.status, response.headers, response.read()
            except urllib.error.HTTPError as error:
                if error.code in absent or (error.code < 500 and error.code != 429):
                    return error.code, error.headers, b""
                failure = f"HTTP {error.code}"
            except OSError as error:
                failure = type(error).__name__
            if attempt == 2:
                raise RuntimeError(f"hand-off storage {method} failed: {failure}") from None
            time.sleep(1 + attempt)
        raise AssertionError("unreachable")

    def list(self, prefix):
        entries, marker = [], ""
        while True:
            status, _, body = self.request("GET", query={
                "restype": "container", "comp": "list", "maxresults": 5000, "prefix": prefix, "marker": marker})
            require(status == 200, f"listing {prefix} failed: HTTP {status}")
            root = ET.fromstring(body)
            for blob in root.iterfind("./Blobs/Blob"):
                properties = blob.find("Properties")
                entries.append({
                    "name": blob.findtext("Name"),
                    "etag": bare_etag(properties.findtext("Etag")),
                    "size": int(properties.findtext("Content-Length") or 0),
                    "md5": properties.findtext("Content-MD5") or None,
                    "modified": properties.findtext("Last-Modified"),
                })
            marker = root.findtext("NextMarker") or ""
            if not marker:
                return entries

    def get(self, name):
        status, headers, body = self.request("GET", name)
        require(status == 200, f"reading {name} failed: HTTP {status}")
        return body, bare_etag(headers.get("ETag"))

    def lease_state(self, name):
        status, headers, _ = self.request("HEAD", name)
        if status == 404:
            return "absent"
        require(status == 200, f"reading {name} failed: HTTP {status}")
        return headers.get("x-ms-lease-state", "unknown")

    def acquire(self, name):
        lease = str(uuid.uuid4())
        status, _, _ = self.request("PUT", name, {"comp": "lease"}, {
            "x-ms-lease-action": "acquire", "x-ms-lease-duration": "-1", "x-ms-proposed-lease-id": lease})
        require(status == 201, f"could not lease {name} (HTTP {status}): a controller tick holds it")
        return lease

    def release(self, name, lease):
        status, _, _ = self.request("PUT", name, {"comp": "lease"}, {
            "x-ms-lease-action": "release", "x-ms-lease-id": lease})
        require(status == 200, f"could not release the lease on {name} (HTTP {status})")

    def delete(self, name, etag):
        """Deletes the blob only while it is still the version that was archived."""
        status, _, _ = self.request("DELETE", name, headers={"If-Match": f'"{bare_etag(etag)}"'})
        if status == 404:
            return False
        require(status != 412, f"{name} changed after it was archived; nothing more was deleted")
        require(status == 202, f"deleting {name} failed: HTTP {status}")
        return True


@contextlib.contextmanager
def leased(container, name):
    """The controller's own blob lease, held for the whole step, so no tick can start."""
    lease = container.acquire(name)
    try:
        yield
    finally:
        container.release(name, lease)


def in_parallel(function, items):
    errors = []
    results = [None] * len(items)
    with concurrent.futures.ThreadPoolExecutor(REQUESTS) as pool:
        index = {pool.submit(function, item): i for i, item in enumerate(items)}
        for future in concurrent.futures.as_completed(index):
            try:
                results[index[future]] = future.result()
            except Exception as error:  # every worker finishes; the first failure is raised
                errors.append(error)
    if errors:
        raise errors[0]
    return results


def list_job_blobs(container):
    """Every blob under the per-job prefixes, one list per prefix and leading hex digit."""
    prefixes = [root + digit for root in JOB_ROOTS for digit in "0123456789abcdef"]
    if hasattr(container, "token"):
        container.token()  # before the threads, so they share one
    listed = in_parallel(container.list, prefixes)
    blobs = {}
    for entries in listed:
        for entry in entries:
            match = JOB_BLOB.fullmatch(entry["name"] or "")
            if match:
                require(entry["etag"], f"{entry['name']} has no ETag")
                blobs[entry["name"]] = {**entry, "job": match[1]}
    return blobs


def read_records(container, blobs):
    names = sorted(name for name in blobs if RECORD_BLOB.fullmatch(name))

    def read(name):
        data, etag = container.get(name)
        require(len(data) <= MAX_RECORD_BYTES, f"{name} is over its size limit")
        require(etag == blobs[name]["etag"], f"{name} changed while it was read; nothing was changed")
        record = json.loads(data)
        require(isinstance(record, dict) and isinstance(record.get("job"), dict)
                and record["job"].get("id") == RECORD_BLOB.fullmatch(name)[1]
                and record.get("stage") in STAGES and isinstance(record.get("plan"), str)
                and isinstance(record["job"].get("message"), dict)
                and isinstance(record["job"]["message"].get("page"), str),
                f"{name} is not a valid dispatch journal")
        return record

    return in_parallel(read, names)


# --- Choosing the batch ----------------------------------------------------------------------


def lineage(history):
    """A page's journals in retry order, as owned_records follows them, or None if they don't
    form one chain."""
    roots = [record for record in history if not record.get("retry")]
    if len(roots) != 1:
        return None
    chain = [roots[0]]
    remaining = [record for record in history if record.get("retry")]
    while remaining:
        current = chain[-1]
        children = [record for record in remaining
                    if isinstance(record["retry"], dict)
                    and record["retry"].get("previous_job") == current["job"]["id"]]
        if len(children) != 1 or current["stage"] != "failed":
            return None
        remaining.remove(children[0])
        chain.append(children[0])
    return chain


def claimed_pages(state):
    """The page list a queue-fed tick restores from its claims (QueueFeed.restore)."""
    return [{"page": page, "batch": claim["batch"]}
            for page, claim in sorted(state.get("claims", {}).items()) if claim.get("scan")]


def outstanding(scheduler, config, state, records):
    """The pages a tick would export or submit: allocated pages without a journal of their own,
    or whose journal is only prepared. A queue-fed tick allocates its claimed pages, a fixed
    plan its configured ones. owned_records' refusals are raised as they are."""
    if config.get("source") == "queue":
        config = {**config, "pages": claimed_pages(state)}
    owned = scheduler.owned_records(config, records)
    return {page["page"] for page in config["pages"]
            if page["page"] not in owned or owned[page["page"]]["stage"] == "prepared"}


def modified_at(blob):
    try:
        return email.utils.parsedate_to_datetime(blob["modified"]).timestamp()
    except (TypeError, ValueError):
        return None


def choose(config, state, records, blobs, now, *, pages, min_age_hours):
    """The oldest fully published pages, up to `pages`, as (chosen, summary). A page qualifies
    when every journal it has forms one retry chain that ends `published`, the last write of
    that journal is at least min_age_hours old, and the controller's state agrees: a page of
    this plan is claimed for the batch its journals name, and a page of another plan isn't
    claimed or selected here."""
    queue = config.get("source") == "queue"
    claims, selected = state.get("claims", {}), state.get("selected", {})
    # A fixed plan's configured pages are allocated whether or not they are selected yet.
    configured = set() if queue else {page["page"] for page in config.get("pages", [])}
    by_page = {}
    for record in records:
        by_page.setdefault((record["plan"], record["job"]["message"]["page"]), []).append(record)
    skipped, candidates = {}, []

    def skip(reason):
        skipped[reason] = skipped.get(reason, 0) + 1

    for (plan, page), history in by_page.items():
        chain = lineage(history)
        if chain is None:
            skip("unresolved_retry_lineage")
            continue
        current = chain[-1]
        if current["stage"] != "published":
            skip(f"current_journal_{current['stage']}")
            continue
        blob = blobs[f"dispatch/jobs/{current['job']['id']}/record.json"]
        published_at = modified_at(blob)
        if published_at is None or now - published_at < min_age_hours * 3600:
            skip("published_too_recently")
            continue
        if plan == config["plan"]:
            if not queue:
                # A page of a fixed page list would be selected and exported again.
                skip("fixed_page_allocation")
                continue
            claim = claims.get(page)
            if not isinstance(claim, dict) or claim.get("batch") != current["job"]["message"].get("batch"):
                skip("not_claimed_for_its_batch")
                continue
        elif page in claims or page in selected or page in configured:
            skip("allocated_to_current_plan")
            continue
        candidates.append({"plan": plan, "page": page, "published_at": int(published_at),
                           "stamp": str(current.get("stamp", "")),
                           "jobs": [record["job"]["id"] for record in chain]})
    candidates.sort(key=lambda page: (page["published_at"], page["stamp"], page["plan"], page["page"]))
    chosen = candidates[:pages]
    summary = {"eligible": len(candidates), "skipped": dict(sorted(skipped.items())),
               "eligible_this_plan": sum(1 for page in candidates if page["plan"] == config["plan"]),
               "published": sum(1 for history in by_page.values()
                                if (chain := lineage(history)) and chain[-1]["stage"] == "published")}
    return chosen, summary


def state_entries(config, state, chosen):
    entries = {key: {} for key in STATE_KEYS}
    published = 0
    for page in chosen:
        if page["plan"] != config["plan"]:
            continue
        published += 1
        for key in ("claims", "selected"):
            if page["page"] in state.get(key, {}):
                entries[key][page["page"]] = state[key][page["page"]]
        for key in ("held_since", "dead_lettered"):
            for job in page["jobs"]:
                if job in state.get(key, {}):
                    entries[key][job] = state[key][job]
    return {**entries, "published": published}


def without(state, entries):
    after = json.loads(json.dumps(state))
    for key in STATE_KEYS:
        for name in entries[key]:
            after.get(key, {}).pop(name, None)
    if entries["published"]:
        after["published"] = max(0, int(after.get("published", 0)) - entries["published"])
    return after


def state_counts(state):
    return {key: len(state.get(key, {})) for key in STATE_KEYS} | {"published": state.get("published", 0)}


# --- dispatch: controller state, then hand-off blobs -----------------------------------------


def load_scheduler():
    """The controller's own batch_scheduler.py, from beside this file. No bytecode is written
    beside it, so the dry run writes nothing."""
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location(
        "batch_scheduler", Path(__file__).resolve().with_name("batch_scheduler.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def batch_dirs(archive):
    root = archive / "batches"
    if not root.is_dir():
        return []
    return sorted(path for path in root.iterdir() if path.is_dir() and BATCH_NAME.fullmatch(path.name))


def open_batch(archive):
    """The batch to work on: the newest one if its dispatch step never finished, else a new one."""
    existing = batch_dirs(archive)
    for path in existing[:-1]:
        require((path / "receipts/dispatch.json").is_file(),
                f"batch {path.name} has no dispatch receipt; only the newest batch can be unfinished")
    if existing and not (existing[-1] / "receipts/dispatch.json").is_file():
        return existing[-1], (existing[-1] / "manifest.json").is_file()
    number = int(existing[-1].name) + 1 if existing else 1
    return archive / "batches" / f"{number:04d}", False


def plan_batch(scheduler, config, state, container, now, args):
    blobs = list_job_blobs(container)
    records = read_records(container, blobs)
    stages = {}
    for record in records:
        stages[record["stage"]] = stages.get(record["stage"], 0) + 1
    chosen, pages = choose(config, state, records, blobs, now, pages=args.pages,
                           min_age_hours=args.min_age_hours)
    jobs = sorted(job for page in chosen for job in page["jobs"])
    require(len(set(jobs)) == len(jobs), "a job belongs to two pages; reconcile the journals first")
    entries = state_entries(config, state, chosen)
    chosen_jobs = set(jobs)
    batch_blobs = sorted(({key: blob[key] for key in ("name", "etag", "size", "md5")}
                          for blob in blobs.values() if blob["job"] in chosen_jobs), key=lambda blob: blob["name"])
    after_state = without(state, entries)
    remaining = [record for record in records if record["job"]["id"] not in chosen_jobs]
    check = {"before": None, "after": None, "error": None}
    try:
        # The archive must not make a page look like it still needs exporting or submitting:
        # the same pages are outstanding before and after, and owned_records accepts both.
        check["before"] = sorted(outstanding(scheduler, config, state, records))
        check["after"] = sorted(outstanding(scheduler, config, after_state, remaining))
    except ValueError as error:
        check["error"] = str(error)
    journals = sum(1 for name in blobs if RECORD_BLOB.fullmatch(name))
    manifest = {
        "schema_version": SCHEMA_VERSION, "kind": "usnm-boinc-archive-batch",
        "created_at": int(now), "account": config["account"], "plan": config["plan"],
        "config_sha256": scheduler.digest(config), "min_age_hours": args.min_age_hours,
        "pages": chosen, "jobs": jobs, "blobs": batch_blobs, "state": entries,
        "before": {"journals": journals, "blobs": len(blobs),
                   "state_sha256": sha256_bytes((args.state_dir / "state.json").read_bytes()),
                   **{f"state_{key}": value for key, value in state_counts(state).items()}},
    }
    summary = {
        "journals": journals, "journal_cap": scheduler.MAX_JOBS, "blobs": len(blobs),
        "journals_by_stage": dict(sorted(stages.items())), "pages": pages,
        "batch": {"pages": len(chosen), "jobs": len(jobs), "blobs": len(batch_blobs),
                  "bytes": sum(blob["size"] for blob in batch_blobs),
                  "this_plan_pages": entries["published"],
                  "other_plan_pages": len(chosen) - entries["published"],
                  "oldest_published_at": chosen[0]["published_at"] if chosen else None,
                  "newest_published_at": chosen[-1]["published_at"] if chosen else None,
                  "state_entries": {key: len(entries[key]) for key in STATE_KEYS}},
        "after_batch": {"journals": journals - sum(1 for blob in batch_blobs if RECORD_BLOB.fullmatch(blob["name"])),
                        **{f"state_{key}": value for key, value in state_counts(after_state).items()}},
        "outstanding_pages": {"before": None if check["before"] is None else len(check["before"]),
                              "after": None if check["after"] is None else len(check["after"]),
                              "unchanged": check["error"] is None and check["before"] == check["after"],
                              "error": check["error"]},
    }
    return manifest, summary


def controller_step(manifest, state_path, batch):
    """Moves the batch's controller state entries into the archive. Done first: a journal
    whose claim is gone fails the tick closed (owned_records), where a claim whose journal is
    gone would be exported again."""
    entries = manifest["state"]
    archive = batch / "controller/state-entries.json"
    state = read_json(state_path)
    require(state.get("config_sha256") == manifest["config_sha256"], "the state belongs to another allocation")
    present = {key: [name for name in entries[key] if name in state.get(key, {})] for key in STATE_KEYS}
    count = sum(len(entries[key]) for key in STATE_KEYS)
    found = sum(len(names) for names in present.values())
    if count and found == 0:
        require(archive.is_file() and json.loads(archive.read_text()) == entries,
                "the batch's state entries are gone but not in the archive; stop and reconcile by hand")
        return {"entries": {key: len(entries[key]) for key in STATE_KEYS}, "resumed": True,
                "archive_sha256": sha256_file(archive)}
    require(found == count, "the controller state changed since the batch was planned; stop and reconcile")
    require(all(state[key][name] == entries[key][name] for key in STATE_KEYS for name in present[key]),
            "the controller state changed since the batch was planned; stop and reconcile")
    write_json(archive, entries)
    if count:
        data = (json.dumps(without(state, entries), sort_keys=True) + "\n").encode()
        write_atomic(state_path, data)
    return {"entries": {key: len(entries[key]) for key in STATE_KEYS}, "resumed": False,
            "archive_sha256": sha256_file(archive)}


def verified_copy(path, digest_path, blob):
    """Whether the archived copy is the blob: its size and Content-MD5 as listed, and the sha256
    recorded when it was downloaded, which is written before the blob is deleted."""
    if not path.is_file() or path.is_symlink() or path.stat().st_size != blob["size"] \
            or not digest_path.is_file():
        return False
    md5, sha256 = hashlib.md5(), hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            md5.update(chunk)
            sha256.update(chunk)
    if blob.get("md5") and base64.b64encode(md5.digest()).decode() != blob["md5"]:
        return False
    return sha256.hexdigest() == digest_path.read_text().strip()


def handoff_step(manifest, container, batch):
    """Copies each of the batch's blobs into the archive, checks the copy, then deletes the
    blob only while it is still the version copied (If-Match). A job's record.json goes last,
    so a job is counted by the controller until all of its blobs are archived."""
    planned = {blob["name"]: blob for blob in manifest["blobs"]}
    jobs = set(manifest["jobs"])
    live = {name: blob for name, blob in list_job_blobs(container).items() if blob["job"] in jobs}
    unexpected = sorted(set(live) - set(planned))
    require(not unexpected, f"{len(unexpected)} blobs appeared under the batch's jobs since it was planned "
                            f"(first: {unexpected[:1]}); stop and reconcile")
    for name, blob in live.items():
        require(blob["etag"] == planned[name]["etag"],
                f"{name} changed since the batch was planned; stop and reconcile")
    root, digests = batch / "handoff", batch / "handoff-sha256"
    by_job = {}
    for name in planned:
        by_job.setdefault(JOB_BLOB.fullmatch(name)[1], []).append(name)

    def archive_job(names):
        deleted = 0
        for name in sorted(names, key=lambda name: (bool(RECORD_BLOB.fullmatch(name)), name)):
            blob, path = planned[name], root / name
            digest_path = digests / (name + ".sha256")
            if name in live:
                if not verified_copy(path, digest_path, blob):
                    data, etag = container.get(name)
                    require(etag == blob["etag"], f"{name} changed while it was copied; stop and reconcile")
                    require(len(data) == blob["size"], f"{name} copy is incomplete")
                    if blob.get("md5"):
                        require(base64.b64encode(hashlib.md5(data).digest()).decode() == blob["md5"],
                                f"{name} copy does not match its Content-MD5")
                    write_atomic(path, data)
                    write_atomic(digest_path, (sha256_bytes(data) + "\n").encode())
                deleted += container.delete(name, blob["etag"])
            else:
                require(verified_copy(path, digest_path, blob),
                        f"{name} is gone from the container but its archived copy is missing or wrong")
        return deleted

    deleted = sum(in_parallel(archive_job, list(by_job.values())))
    left = sorted(name for name, blob in list_job_blobs(container).items() if blob["job"] in jobs)
    require(not left, f"{len(left)} of the batch's blobs are still in the container")
    entries = archived_entries(root)
    require(len(entries) == len(planned), "the archive does not hold every blob of the batch")
    require(all((digests / (entry["path"] + ".sha256")).read_text().strip() == entry["sha256"]
                for entry in entries), "an archived blob no longer matches the sha256 taken when it was copied")
    return {"blobs": len(entries), "bytes": sum(entry["size"] for entry in entries),
            "deleted_now": deleted, "archive_digest": tree_digest(entries)}


def dispatch(args):
    scheduler = load_scheduler()
    config = scheduler.load_config(args.config)
    require(config.get("app") != getattr(scheduler, "SURYA_APP_NAME", "usnm_surya"),
            "a Surya plan has no dispatch journals to archive")
    args.state_dir = args.state_dir.resolve()
    state_path = args.state_dir / "state.json"
    state = read_json(state_path)
    require(state.get("config_sha256") == scheduler.digest(config),
            "the state directory belongs to another allocation; give the controller's own config")
    container = RestContainer(config["account"])
    units = {"timer": unit_state(f"{args.controller_unit}.timer"),
             "service": unit_state(f"{args.controller_unit}.service")}
    lease = container.lease_state(LEASE_BLOB)
    now = time.time()
    controller = {**units, "lease": lease, "paused": controller_stopped(units) and lease in FREE_LEASE_STATES}
    archive = args.archive_dir.resolve() if args.archive_dir else None
    if archive is not None:
        require(not archive.is_relative_to(args.state_dir), "--archive-dir must be outside --state-dir")
    if not args.apply:
        resume = None
        if archive is not None and archive.is_dir():
            batch, planned = open_batch(archive)
            if planned:
                manifest = read_json(batch / "manifest.json")
                resume = {"batch": batch.name, "pages": len(manifest["pages"]), "jobs": len(manifest["jobs"])}
        manifest, summary = plan_batch(scheduler, config, state, container, now, args)
        emit({"event": "boinc_archive_dispatch", "schema_version": SCHEMA_VERSION, "apply": False,
              "plan": config["plan"], "controller": controller, "resume": resume, **summary})
        return 0
    require_apply(args, units)
    require(archive is not None, "--apply needs --archive-dir")
    require(lease in FREE_LEASE_STATES, f"the controller lease is {lease}: a tick may be running")
    lock = args.state_dir / "controller.lock"
    with flocked([lock], existing=[lock]), leased(container, LEASE_BLOB):
        state = read_json(state_path)
        batch, planned = open_batch(archive)
        if planned:
            manifest = read_json(batch / "manifest.json")
            require(manifest["account"] == config["account"] and manifest["plan"] == config["plan"]
                    and manifest["config_sha256"] == scheduler.digest(config),
                    f"batch {batch.name} was planned for another allocation")
            progress(f"resuming batch {batch.name}")
        else:
            manifest, summary = plan_batch(scheduler, config, state, container, now, args)
            outstanding_pages = summary["outstanding_pages"]
            require(outstanding_pages["error"] is None,
                    f"the journals and state don't agree already: {outstanding_pages['error']}")
            require(outstanding_pages["unchanged"], "archiving this batch would leave pages needing work again")
            if not manifest["pages"]:
                emit({"event": "boinc_archive_dispatch", "schema_version": SCHEMA_VERSION, **summary,
                      "apply": True, "batch": None, "message": "no page is drained and old enough to archive"})
                return 0
            need = summary["batch"]["bytes"] * 1.1 + (64 << 20)
            archive.mkdir(parents=True, exist_ok=True, mode=0o700)
            require(shutil.disk_usage(archive).free > need, "not enough free space in --archive-dir for the batch")
            write_json(batch / "manifest.json", manifest)
            progress(f"planned batch {batch.name}: {len(manifest['pages'])} pages, {len(manifest['jobs'])} jobs")
        manifest_sha = sha256_file(batch / "manifest.json")
        controller_result = controller_step(manifest, state_path, batch)
        progress(f"batch {batch.name}: controller state archived")
        handoff_result = handoff_step(manifest, container, batch)
        after = read_json(state_path)
        journals = sum(1 for name in list_job_blobs(container) if RECORD_BLOB.fullmatch(name))
        receipt = {
            "schema_version": SCHEMA_VERSION, "step": "dispatch", "batch": batch.name, "complete": True,
            "finished_at": int(time.time()), "manifest_sha256": manifest_sha,
            "pages": len(manifest["pages"]), "jobs": len(manifest["jobs"]),
            "before": manifest["before"],
            "after": {"journals": journals, "state_sha256": sha256_file(state_path),
                      **{f"state_{key}": value for key, value in state_counts(after).items()}},
            "controller": controller_result, "handoff": handoff_result,
        }
        write_json(batch / "receipts/dispatch.json", receipt)
    emit({"event": "boinc_archive_dispatch", "apply": True, **receipt})
    return 0


# --- bridge: the BOINC host's journals and spool outputs -------------------------------------


def dispatched(batch):
    """The batch's manifest, once its dispatch step is complete (copied from the controller)."""
    manifest_path = batch / "manifest.json"
    receipt = read_json(batch / "receipts/dispatch.json")
    require(receipt.get("complete") is True and receipt.get("manifest_sha256") == sha256_file(manifest_path),
            "copy the batch's manifest.json and receipts/dispatch.json from the controller host unchanged; "
            "the dispatch step must be complete first")
    require(receipt.get("batch") == batch.name,
            f"{batch} holds batch {receipt.get('batch')}'s receipt: copy each batch into the directory of its number")
    manifest = read_json(manifest_path)
    require(all(ID.fullmatch(job) for job in manifest["jobs"]), "invalid batch manifest")
    return manifest


def walk(root):
    """Every entry under root as (path, is_file), without following symlinks."""
    stack = [root]
    while stack:
        with os.scandir(stack.pop()) as entries:
            for entry in entries:
                if entry.is_dir(follow_symlinks=False):
                    stack.append(entry.path)
                    yield Path(entry.path), False
                else:
                    yield Path(entry.path), entry.is_file(follow_symlinks=False)


def server_record(path):
    require(not path.is_symlink() and path.stat().st_size <= MAX_RECORD_BYTES, f"{path} is not a journal")
    return json.loads(path.read_text())


def bridge_plan(manifest, handoff_dir, results):
    jobs = set(manifest["jobs"])
    jobs_root = handoff_dir / "jobs"
    require(jobs_root.is_dir() and not jobs_root.is_symlink() and results.is_dir(),
            "--handoff-dir must hold jobs/ and --results must be a directory")
    planned, absent, bundles, live_bundles, total = {}, [], set(), set(), 0
    for record_path in sorted(jobs_root.glob("*/record.json")):
        total += 1
        job = record_path.parent.name
        record = server_record(record_path)
        refs = {value for value in (record.get("bundle"), record.get("unbundled")) if isinstance(value, str)}
        if job not in jobs:
            live_bundles |= refs
            continue
        require(record.get("job", {}).get("id") == job, f"{record_path} names another job")
        require(record.get("stage") in DRAINED_SERVER_STAGES,
                f"job {job} is {record.get('stage')} on the server, not drained; stop and reconcile")
        bundles |= refs
        files = []
        for path, is_file in walk(record_path.parent):
            require(is_file or (path.is_dir() and not path.is_symlink()), f"{path} is not a regular file")
            if is_file:
                files.append({"path": str(path.relative_to(jobs_root)), "size": path.stat().st_size,
                              "sha256": sha256_file(path)})
        planned[job] = sorted(files, key=lambda entry: entry["path"])
    for job in sorted(jobs - set(planned)):
        require(not (jobs_root / job).exists(), f"{jobs_root / job} has no record.json; stop and reconcile")
        absent.append(job)
    drained = bundles - live_bundles
    spool, entries = [], 0
    for path, is_file in walk(results):
        entries += 1
        if not is_file:
            continue
        match = SPOOL_FILE.fullmatch(path.name)
        owner = match and match[1] in jobs
        bundle = BUNDLE_SPOOL_FILE.fullmatch(path.name)
        if owner or (bundle and bundle[1] in drained):
            spool.append({"path": str(path.relative_to(results)), "size": path.stat().st_size,
                          "sha256": sha256_file(path)})
    workunits = sorted([f"usnm_{job}" for job in jobs] + [f"usnm_ocrb_{bundle}" for bundle in drained])
    return {
        "schema_version": SCHEMA_VERSION, "step": "bridge",
        "jobs": planned, "absent_jobs": absent,
        "spool": sorted(spool, key=lambda entry: entry["path"]),
        "drained_bundles": sorted(drained), "kept_bundles": sorted(bundles & live_bundles),
        "workunits": workunits,
        "before": {"server_jobs": total, "results_entries": entries},
    }


def move(source, target, entry):
    """Moves one file into the archive, resumably: the archived copy is checked against the
    planned hash before the source goes."""
    if source.exists():
        require(source.is_file() and not source.is_symlink() and sha256_file(source) == entry["sha256"],
                f"{source} changed since the batch was planned; stop and reconcile")
        if target.is_file() and sha256_file(target) == entry["sha256"]:
            source.unlink()
            return
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        partial = target.with_name(target.name + ".partial")
        partial.unlink(missing_ok=True)
        try:
            os.link(source, partial)
            os.replace(partial, target)
        except OSError:
            copy_atomic(source, target)
        require(sha256_file(target) == entry["sha256"], f"the archived copy of {source} does not match")
        fsync_dir(target.parent)
        source.unlink()
        return
    require(target.is_file() and sha256_file(target) == entry["sha256"],
            f"{source} is gone but its archived copy is missing or wrong")


def remove_empty(directory):
    """Removes a job's emptied journal directory; a file that wasn't planned stops the step."""
    if not directory.exists():
        return
    stray = [path for path in directory.rglob("*") if not path.is_dir() or path.is_symlink()]
    require(not stray, f"{directory} holds files the batch didn't plan (first: {stray[0].name if stray else ''}); "
                       "stop and reconcile")
    for path in sorted((path for path in directory.rglob("*")), key=lambda p: -len(p.parts)):
        path.rmdir()
    directory.rmdir()


def bridge(args):
    for option, path in (("--handoff-dir", args.handoff_dir), ("--results", args.results)):
        require(not path.is_symlink(), f"{option} {path} is a symlink: name the directory it points to")
    batch = args.batch_dir.resolve()
    handoff_dir, results = args.handoff_dir.resolve(), args.results.resolve()
    for root in (handoff_dir / "jobs", results):
        require(not batch.is_relative_to(root), "--batch-dir must be outside the bridge journals and results")
    manifest = dispatched(batch)
    manifest_sha256 = sha256_file(batch / "manifest.json")
    plan_path = batch / "bridge-plan.json"
    receipt_path = batch / "receipts/bridge.json"

    def planned():
        plan = read_json(plan_path)
        require(plan.get("manifest_sha256") == manifest_sha256, "bridge-plan.json belongs to another batch")
        return plan

    if not args.apply:
        plan = planned() if plan_path.is_file() else bridge_plan(manifest, handoff_dir, results)
        left = [entry["path"] for files in plan["jobs"].values() for entry in files
                if (handoff_dir / "jobs" / entry["path"]).exists()]
        left += [entry["path"] for entry in plan["spool"] if (results / entry["path"]).exists()]
        emit({"event": "boinc_archive_bridge", "schema_version": SCHEMA_VERSION, "apply": False,
              "batch": batch.name, "complete": receipt_path.is_file(), "resume": plan_path.is_file(),
              "jobs": len(plan["jobs"]), "absent_jobs": len(plan["absent_jobs"]),
              "journal_files": sum(len(files) for files in plan["jobs"].values()),
              "spool_files": len(plan["spool"]), "drained_bundles": len(plan["drained_bundles"]),
              "kept_bundles": len(plan["kept_bundles"]), "files_still_in_place": len(left),
              "bytes": sum(entry["size"] for files in plan["jobs"].values() for entry in files)
              + sum(entry["size"] for entry in plan["spool"]),
              "before": plan["before"]})
        return 0
    require_apply(args)
    locks = [Path(path) for path in (args.lock_file or DEFAULT_BRIDGE_LOCKS)] + [handoff_dir / ".lock"]
    # /run is emptied at boot, but the bridge has created its state directory's lock.
    with flocked(locks, existing=[handoff_dir / ".lock"]):
        if plan_path.is_file():
            plan = planned()
            progress(f"resuming bridge step of batch {batch.name}")
        else:
            plan = {**bridge_plan(manifest, handoff_dir, results), "manifest_sha256": manifest_sha256}
            need = sum(entry["size"] for files in plan["jobs"].values() for entry in files) \
                + sum(entry["size"] for entry in plan["spool"])
            batch.mkdir(parents=True, exist_ok=True, mode=0o700)
            require(shutil.disk_usage(batch).free > need * 1.1 + (64 << 20),
                    "not enough free space in --batch-dir for the batch")
            write_json(plan_path, plan)
        for job, files in plan["jobs"].items():
            for entry in files:
                move(handoff_dir / "jobs" / entry["path"], batch / "bridge/jobs" / entry["path"], entry)
            remove_empty(handoff_dir / "jobs" / job)
        for entry in plan["spool"]:
            move(results / entry["path"], batch / "spool" / entry["path"], entry)
        left = [job for job in plan["jobs"] if (handoff_dir / "jobs" / job).exists()]
        require(not left, f"{len(left)} of the batch's bridge journals are still in place")
        journals = archived_entries(batch / "bridge/jobs")
        spool = archived_entries(batch / "spool")
        after_jobs = sum(1 for _ in (handoff_dir / "jobs").glob("*/record.json"))
        after_entries = sum(1 for _ in walk(results))
        receipt = {
            "schema_version": SCHEMA_VERSION, "step": "bridge", "batch": batch.name, "complete": True,
            "finished_at": int(time.time()), "manifest_sha256": sha256_file(batch / "manifest.json"),
            "plan_sha256": sha256_file(plan_path), "before": plan["before"],
            "after": {"server_jobs": after_jobs, "results_entries": after_entries},
            "journals": {"jobs": len(plan["jobs"]), "absent_jobs": len(plan["absent_jobs"]),
                         "files": len(journals), "archive_digest": tree_digest(journals)},
            "spool": {"files": len(spool), "archive_digest": tree_digest(spool)},
            "drained_bundles": plan["drained_bundles"], "workunits": plan["workunits"],
        }
        write_json(receipt_path, receipt)
    emit({"event": "boinc_archive_bridge", "apply": True,
          **{key: value for key, value in receipt.items() if key != "workunits"},
          "workunits": len(receipt["workunits"])})
    return 0


# --- db: the BOINC database's workunit and result rows ---------------------------------------


class MysqlClient:
    """The mysql client, with the TLS settings batch-inventory uses."""

    def __init__(self, args):
        require(re.fullmatch(r"[A-Za-z0-9_]+", args.database), "invalid database name")
        self.argv = [args.mysql, f"--defaults-extra-file={args.mysql_defaults_file}", "--ssl",
                     "--ssl-verify-server-cert", f"--ssl-ca={args.mysql_ca}", "--batch",
                     f"--database={args.database}"]

    def run(self, sql, *, header=False):
        argv = self.argv + ([] if header else ["--raw", "--skip-column-names"]) + ["--execute", sql]
        try:
            done = subprocess.run(argv, check=False, text=True, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, timeout=600)
        except (OSError, subprocess.SubprocessError):
            raise RuntimeError("the mysql client could not run") from None
        require(done.returncode == 0, "a database statement failed; nothing after it ran")
        return done.stdout

    def session(self):
        return MysqlSession(self.argv + ["--unbuffered"])


class MysqlSession:
    """One mysql client session fed a statement at a time, so a transaction's row locks hold
    while the step writes its export. The client stops at a failed statement, and a session
    that ends without COMMIT rolls back."""

    def __init__(self, argv):
        try:
            self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                            stderr=subprocess.DEVNULL, encoding="utf-8",
                                            errors="surrogateescape")
        except OSError:
            raise RuntimeError("the mysql client could not run") from None

    def query(self, sql):
        """The statement's (header, rows), each field as the client escapes it in batch mode;
        ([], []) when it returns no rows."""
        marker = "usnm-archive-" + uuid.uuid4().hex
        try:
            self.process.stdin.write(f"{sql};\nSELECT '{marker}' AS marker;\n")
            self.process.stdin.flush()
        except OSError:
            raise Refused("a database statement failed; nothing after it ran") from None
        lines = []
        while True:
            line = self.process.stdout.readline()
            require(line, "a database statement failed; nothing after it ran")
            line = line.rstrip("\n")
            if line == marker:
                break
            lines.append(line)
        # The marker's own column header ends the statement's output.
        require(lines and lines[-1] == "marker", "unexpected database output")
        lines = [line.split("\t") for line in lines[:-1]]
        return (lines[0], lines[1:]) if lines else ([], [])

    def close(self):
        with contextlib.suppress(OSError):
            self.process.stdin.close()
        try:
            self.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def sql_list(values):
    require(all(re.fullmatch(r"[A-Za-z0-9_]+", str(value)) for value in values), "invalid SQL value")
    return ", ".join(f"'{value}'" if isinstance(value, str) else str(int(value)) for value in values)


def chunks(values):
    return [values[i:i + DB_CHUNK] for i in range(0, len(values), DB_CHUNK)]


EXISTING_SQL = "SELECT COUNT(*) FROM workunit WHERE name REGEXP '^usnm_(ocrb_)?[0-9a-f]{64}$'"
# Finished as db_purge would have it: the workunit assimilated (2) with its input files deleted
# (2), and every result over (5) with its output files deleted (2). BOINC tracks a result's
# output-file cleanup apart from its workunit's; a row deleted before file_deleter is done
# with it would leave its uploads orphaned.
FINISHED_RESULT = "server_state = 5 AND file_delete_state = 2"
FINISHED_WORKUNIT = "assimilate_state = 2 AND file_delete_state = 2"


def db_rows(client, names):
    rows = []
    for part in chunks(sorted(names)):
        output = client.run(
            "SELECT w.id, w.name, w.assimilate_state, w.file_delete_state, "
            "(SELECT COUNT(*) FROM result r WHERE r.workunitid = w.id), "
            f"(SELECT COUNT(*) FROM result r WHERE r.workunitid = w.id AND NOT ({FINISHED_RESULT})) "
            f"FROM workunit w WHERE w.name IN ({sql_list(part)}) ORDER BY w.name")
        for line in output.splitlines():
            fields = line.split("\t")
            require(len(fields) == 6 and WORKUNIT.fullmatch(fields[1])
                    and all(re.fullmatch(r"-?[0-9]+", value) for i, value in enumerate(fields) if i != 1),
                    "unexpected database output")
            rows.append({"id": int(fields[0]), "name": fields[1], "assimilate_state": int(fields[2]),
                         "file_delete_state": int(fields[3]), "results": int(fields[4]),
                         "unfinished_results": int(fields[5])})
    return rows


def finished(row):
    return row["assimilate_state"] == 2 and row["file_delete_state"] == 2 and row["unfinished_results"] == 0


def existing(client):
    output = client.run(EXISTING_SQL).strip()
    require(output.isdecimal(), "unexpected database output")
    return int(output)


def columns(header, names):
    require(all(name in header for name in names), "unexpected database columns")
    return [header.index(name) for name in names]


def tsv(header, rows):
    lines = [header] + rows if rows else []
    return "".join("\t".join(fields) + "\n" for fields in lines).encode("utf-8", "surrogateescape")


def exported_ids(path):
    lines = path.read_bytes().decode("utf-8", "surrogateescape").splitlines()
    if not lines:
        return set()
    require(lines[0].split("\t")[0] == "id", f"{path} is not a workunit export")
    return {int(line.split("\t")[0]) for line in lines[1:]}


def db_chunk(session, batch, index, ids):
    """Archives one chunk of the batch's workunits in one transaction. Its rows are locked,
    checked again and exported to the archive, and only then deleted. A crash before COMMIT
    rolls the deletes back, and the next attempt exports the rows again beside the first
    export; one after COMMIT leaves an export the next attempt finds the deleted rows in."""
    directory = batch / "db"
    earlier = sorted(directory.glob(f"chunk-{index:04d}-attempt-*/workunit.tsv"))
    session.query("START TRANSACTION")
    work_header, work = session.query(
        f"SELECT * FROM workunit WHERE id IN ({sql_list(ids)}) ORDER BY id FOR UPDATE")
    result_header, results = session.query(
        f"SELECT * FROM result WHERE workunitid IN ({sql_list(ids)}) ORDER BY id FOR UPDATE")
    w_id, w_assimilate, w_delete = columns(work_header, ("id", "assimilate_state", "file_delete_state")) \
        if work else (0, 0, 0)
    r_unit, r_server, r_delete = columns(result_header, ("workunitid", "server_state", "file_delete_state")) \
        if results else (0, 0, 0)
    unfinished = {int(row[r_unit]) for row in results if (row[r_server], row[r_delete]) != ("5", "2")}
    ready = sorted(int(row[w_id]) for row in work
                   if (row[w_assimilate], row[w_delete]) == ("2", "2") and int(row[w_id]) not in unfinished)
    present = {int(row[w_id]) for row in work}
    files = {}
    if ready:
        keep = set(ready)
        attempt = directory / f"chunk-{index:04d}-attempt-{len(earlier) + 1}"
        ready_work = [row for row in work if int(row[w_id]) in keep]
        ready_results = [row for row in results if int(row[r_unit]) in keep]
        for name, header, rows in (("workunit.tsv", work_header, ready_work),
                                   ("result.tsv", result_header, ready_results)):
            write_atomic(attempt / name, tsv(header, rows))
        # ROW_COUNT() in the same call: the marker query after each call resets it.
        deleted_results = int(session.query(
            f"DELETE FROM result WHERE workunitid IN ({sql_list(ready)}) AND {FINISHED_RESULT}; "
            "SELECT ROW_COUNT() AS deleted")[1][0][0])
        deleted_work = int(session.query(
            f"DELETE FROM workunit WHERE id IN ({sql_list(ready)}) AND {FINISHED_WORKUNIT} "
            "AND NOT EXISTS (SELECT 1 FROM result r WHERE r.workunitid = workunit.id); "
            "SELECT ROW_COUNT() AS deleted")[1][0][0])
        if (deleted_results, deleted_work) != (len(ready_results), len(ready)):
            session.query("ROLLBACK")
            raise Refused(f"chunk {index} deleted other rows than it exported; rolled back, stop and reconcile")
    session.query("COMMIT")
    before = set().union(*(exported_ids(path) for path in earlier)) if earlier else set()
    gone = set(ids) - present
    for path in sorted(directory.glob(f"chunk-{index:04d}-attempt-*/*.tsv")):
        files[str(path.relative_to(directory))] = sha256_file(path)
    part = {"chunk": index, "ids": len(ids), "deleted": ready,
            "deferred": sorted(present - set(ready)),
            # Deleted by an earlier attempt that stopped after its COMMIT: in that export.
            "deleted_earlier": sorted(gone & before),
            # Gone without an export here, as db_purge would leave them.
            "vanished": sorted(gone - before), "files": files}
    write_json(directory / f"chunk-{index:04d}.json", part)
    return part


def db(args):
    batch = args.batch_dir.resolve()
    dispatched(batch)
    manifest_sha256 = sha256_file(batch / "manifest.json")
    bridge_receipt = read_json(batch / "receipts/bridge.json")
    require(bridge_receipt.get("complete") is True and bridge_receipt.get("manifest_sha256") == manifest_sha256
            and all(WORKUNIT.fullmatch(name) for name in bridge_receipt["workunits"]),
            "the batch's bridge step must be complete first, with this batch's manifest")
    names = bridge_receipt["workunits"]
    client = MysqlClient(args)
    plan_path = batch / "db-plan.json"
    receipt_path = batch / "receipts/db.json"
    rows = db_rows(client, names)
    ready = [row for row in rows if finished(row)]
    before = existing(client)
    if not args.apply:
        plan = read_json(plan_path) if plan_path.is_file() else None
        emit({"event": "boinc_archive_db", "schema_version": SCHEMA_VERSION, "apply": False,
              "batch": batch.name, "complete": receipt_path.is_file(), "resume": plan is not None,
              "chunks_done": len(list((batch / "db").glob("chunk-[0-9][0-9][0-9][0-9].json"))),
              "workunits": len(names), "present": len(rows), "absent": len(names) - len(rows),
              "ready": len(ready), "deferred": len(rows) - len(ready),
              "results": sum(row["results"] for row in ready), "existing_workunits": before})
        return 0
    require_apply(args)
    if plan_path.is_file():
        plan = read_json(plan_path)
        require(plan.get("manifest_sha256") == manifest_sha256, "db-plan.json belongs to another batch")
        progress(f"resuming db step of batch {batch.name}")
    else:
        # Every present row is planned; whether it is finished is decided again under its lock.
        plan = {"schema_version": SCHEMA_VERSION, "step": "db", "manifest_sha256": manifest_sha256,
                "ids": sorted(row["id"] for row in rows),
                "before": {"existing_workunits": before, "present": len(rows), "ready": len(ready)}}
        write_json(plan_path, plan)
    parts = []
    session = client.session()
    try:
        for index, ids in enumerate(chunks(plan["ids"]), 1):
            done = batch / "db" / f"chunk-{index:04d}.json"
            parts.append(read_json(done) if done.is_file() else db_chunk(session, batch, index, ids))
    finally:
        session.close()
    deleted = {row for part in parts for row in part["deleted"] + part["deleted_earlier"]}
    left = [row["name"] for row in db_rows(client, names) if row["id"] in deleted]
    require(not left, f"{len(left)} of the batch's archived workunits are still in the database")
    exports = {path: digest for part in parts for path, digest in part["files"].items()}
    for path, digest in exports.items():
        require(sha256_file(batch / "db" / path) == digest, f"the archived db/{path} changed")
    receipt = {
        "schema_version": SCHEMA_VERSION, "step": "db", "batch": batch.name, "complete": True,
        "finished_at": int(time.time()), "manifest_sha256": manifest_sha256,
        "plan_sha256": sha256_file(plan_path), "before": plan["before"],
        "after": {"existing_workunits": existing(client)},
        "deleted_workunits": len(deleted), "deferred": sum(len(part["deferred"]) for part in parts),
        "vanished": sum(len(part["vanished"]) for part in parts),
        "absent": len(names) - len(plan["ids"]), "exports": exports,
    }
    write_json(receipt_path, receipt)
    emit({"event": "boinc_archive_db", "apply": True, **receipt})
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    steps = parser.add_subparsers(dest="step", required=True)

    def common(step):
        step.add_argument("--apply", action="store_true", help="change the stores (default: a read-only dry run)")
        step.add_argument("--confirm-controller-paused", action="store_true",
                          help=f"the operator stopped {UNIT}.timer and no tick is running")

    one = steps.add_parser("dispatch", help="the controller's state and the hand-off blobs (controller host)")
    one.add_argument("--config", type=Path, required=True, help="the controller's config")
    one.add_argument("--state-dir", type=Path, required=True, help="the controller's state directory")
    one.add_argument("--archive-dir", type=Path, help="where batches are archived (required with --apply)")
    one.add_argument("--pages", type=int, default=DEFAULT_PAGES, help=f"pages per batch (1 to {MAX_PAGES})")
    one.add_argument("--min-age-hours", type=int, default=DEFAULT_MIN_AGE_HOURS,
                     help="archive only pages published at least this long ago")
    one.add_argument("--controller-unit", default=UNIT,
                     help="the controller's systemd service and timer name, without the suffix")
    common(one)
    two = steps.add_parser("bridge", help="the bridge journals and spool outputs (BOINC host)")
    two.add_argument("--batch-dir", type=Path, required=True,
                     help="the batch's directory, with manifest.json and receipts/dispatch.json")
    two.add_argument("--handoff-dir", type=Path, required=True, help="the bridge's HANDOFF_DIR")
    two.add_argument("--results", type=Path, required=True, help="the BOINC project's results directory")
    two.add_argument("--lock-file", action="append",
                     help=f"a bridge step lock to hold (default: {', '.join(DEFAULT_BRIDGE_LOCKS)})")
    common(two)
    three = steps.add_parser("db", help="the BOINC database's workunit and result rows")
    three.add_argument("--batch-dir", type=Path, required=True,
                       help="the batch's directory, after its bridge step")
    three.add_argument("--mysql-defaults-file", required=True, help="a mysql client defaults file")
    three.add_argument("--database", required=True, help="the BOINC database's name")
    three.add_argument("--mysql-ca", default="/usr/local/share/usnm/mysql-ca.pem", help="the server's CA")
    three.add_argument("--mysql", default="mysql", help="the mysql client")
    common(three)
    args = parser.parse_args(argv)
    if args.step == "dispatch" and not 1 <= args.pages <= MAX_PAGES:
        parser.error(f"--pages is 1 to {MAX_PAGES}")
    if args.step == "dispatch" and args.min_age_hours < 0:
        parser.error("--min-age-hours is 0 or more")
    try:
        return {"dispatch": dispatch, "bridge": bridge, "db": db}[args.step](args)
    except (ValueError, KeyError, TypeError, OSError, RuntimeError) as error:
        message = str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__
        emit({"event": "boinc_archive_failure", "schema_version": SCHEMA_VERSION, "step": args.step,
              "apply": args.apply, "error": message}, sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
