#!/usr/bin/env python3
"""The hand-off root on the BOINC host, for tools on another host (#1367): the held-page archive
(archive_history.py) and the cross-build check's fetch (onprem/crossbuild-check/fetch_handoff.py).

Where they read and changed the Azure `boinc-handoff` container, they ask the BOINC host's step
runner (onprem-step.sh over the controller's ssh key, `step_command` in the plan's config) for
the same operations on the hand-off root: archive-list, archive-read, archive-delete and
archive-write (handoff_archive.py), and the controller's local expiring lock (handoff_lock.py) in
place of the container's blob lease. The class has the container's methods (list, get, delete,
upload, lease_state, acquire, release), so the tools' own logic is unchanged.

A plan's controller flags decide whether the root is on the BOINC host: `--ssh-reads` in
`<config>.flags` (boinc-priority-controller[.PLAN].flags beside boinc-priority-controller[.PLAN].json),
the flag with which the controller's own tick reads the root over ssh. detect() reads it.
"""

import hashlib
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

LOCK_TTL_SECS = 300
LOCK_HEARTBEAT_SECS = 30
SIDE_BY_SIDE_FILE = "/etc/usnm/boinc-priority-controller.side-by-side"
# Names a hand-off step on the host takes; the runner refuses anything else.
PLAN = re.compile(r"[a-z0-9][a-z0-9-]{0,62}")
DELETE_CHUNK = 400
READ_TRIES = 3
SSH_FAILED = 255


class HandoffRefused(RuntimeError):
    """A step the BOINC host refused or could not run."""


def flag_lines(path):
    """The flags a controller flags file holds, one per line (blank lines and # comments skipped).
    A value follows its flag on the same line, as controller-tick.sh splits them."""
    words = []
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            words.append(line)
    return words


def detect(config_path, side_by_side=SIDE_BY_SIDE_FILE):
    """(on_host, plan_scope): whether the plan's controller reads the hand-off root over ssh, and
    whether its plans tick side by side, so each has its own lock record. A config without a flags
    file beside it, or one without --ssh-reads, is on Azure."""
    flags = Path(config_path).with_suffix(".flags")
    if not flags.is_file():
        return False, False
    on_host = "--ssh-reads" in flag_lines(flags) and "--local-lock" in flag_lines(flags)
    return on_host, on_host and Path(side_by_side).exists()


def run_command(argv, stdin, timeout):
    return subprocess.run(argv, input=stdin, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=timeout, check=False)


class SshHandoff:
    """The container's interface over the BOINC host's hand-off steps."""

    # ssh sessions at once: the host's sshd starts unauthenticated connections a few at a time.
    parallel = 6

    def __init__(self, config, *, plan_scope=False, runner=run_command, clock=time.monotonic,
                 heartbeat_secs=LOCK_HEARTBEAT_SECS):
        command = config.get("step_command")
        if not (isinstance(command, list) and command and all(isinstance(word, str) for word in command)):
            raise HandoffRefused("the plan's config has no step_command: the BOINC host is not reachable")
        if plan_scope and not PLAN.fullmatch(str(config.get("plan"))):
            raise HandoffRefused("--plan-scope needs a plan named in lower-case letters, digits and dashes")
        self.command = command
        self.env = config["env"]
        self.plan = config.get("plan")
        self.plan_scope = plan_scope
        self.runner = runner
        self.clock = clock
        self.heartbeat_secs = heartbeat_secs
        self.fence = None
        self.lost = None
        self.unsure = False
        self._stop = None
        self._thread = None
        self._lock_deadline = None

    # --- one step -------------------------------------------------------------------------

    def argv(self, step, variables, wait_mins):
        argv = [*self.command, "--env", self.env, "--target", "handoff", "--step", step,
                "--wait-mins", str(wait_mins)]
        for key, value in variables.items():
            argv += ["--var", f"{key}={value}"]
        return argv

    def step(self, step, variables=None, *, stdin=None, wait_mins=2, timeout=300, fenced=False):
        """Runs a step on the host and returns its stdout bytes. A fenced step carries the lock's
        holder and generation, and the plan's scope with --plan-scope; the host refuses it when
        the record has moved on. Only a transport failure (ssh's own 255) is tried again; a step
        the host ran and refused is not."""
        variables = dict(variables or {})
        if fenced:
            if self.lost is not None:
                raise HandoffRefused(f"the controller lock was lost: {self.lost}")
            if self.fence is None:
                raise HandoffRefused("no controller lock is held")
            variables["LOCK_HOLDER"], variables["LOCK_GENERATION"] = self.fence
            if self.plan_scope:
                variables["PLAN_SCOPE"] = self.plan
                variables["LOCK_PLAN"] = self.plan
            variables["EXPECTED_HANDOFF_URL"] = "/handoff-state"
        argv = self.argv(step, variables, wait_mins)
        tries = 1 if fenced else READ_TRIES
        failure = ""
        for attempt in range(tries):
            try:
                done = self.runner(argv, stdin, timeout)
            except (OSError, subprocess.SubprocessError) as error:
                failure = type(error).__name__
                if fenced:
                    self.unsure = True
            else:
                if done.returncode == 0:
                    return done.stdout
                text = done.stderr.decode("utf-8", errors="replace").strip().splitlines()
                failure = (text[-1] if text else f"exit status {done.returncode}")[:300]
                if done.returncode != SSH_FAILED:
                    raise HandoffRefused(f"hand-off {step} failed: {failure}")
                if fenced:
                    self.unsure = True
            if attempt + 1 < tries:
                time.sleep(1 + attempt)
        raise HandoffRefused(f"hand-off {step} failed: {failure}")

    # --- the container's methods -----------------------------------------------------------

    def list(self, prefix, suffix=None, max_age_secs=None):
        variables = {"ARCHIVE_PREFIX": prefix}
        if suffix:
            variables["ARCHIVE_SUFFIX"] = suffix
        if max_age_secs is not None:
            variables["ARCHIVE_MAX_AGE_SECS"] = int(max_age_secs)
        out = self.step("archive-list", variables, wait_mins=5, timeout=600)
        try:
            return json.loads(out)["entries"]
        except (ValueError, KeyError, TypeError):
            raise HandoffRefused(f"listing {prefix} gave no listing") from None

    def get(self, name):
        out = self.step("archive-read", {"ARCHIVE_OBJECT": name}, wait_mins=5, timeout=900)
        header, newline, data = out.partition(b"\n")
        try:
            header = json.loads(header)
            if not newline or header["size"] != len(data) or header["sha256"] != hashlib.sha256(data).hexdigest():
                raise ValueError
            return data, header["etag"]
        except (ValueError, KeyError, TypeError):
            raise HandoffRefused(f"reading {name} came short or damaged") from None

    def upload(self, name, data, md5=None):
        """Writes the object only where none is; False if one is already there."""
        variables = {"ARCHIVE_OBJECT": name}
        if md5:
            variables["ARCHIVE_MD5"] = md5
        out = self.step("archive-write", variables, stdin=data, wait_mins=10, timeout=900, fenced=True)
        return bool(json.loads(out)["written"])

    def delete_many(self, pairs):
        """Deletes (name, etag) pairs in order, each only while it is still the version archived;
        the host stops at the first that changed. Returns how many it deleted."""
        deleted = 0
        pairs = list(pairs)
        for start in range(0, len(pairs), DELETE_CHUNK):
            items = [{"name": name, "etag": etag} for name, etag in pairs[start:start + DELETE_CHUNK]]
            out = self.step("archive-delete", stdin=json.dumps({"items": items}).encode(), wait_mins=10,
                            timeout=900, fenced=True)
            deleted += json.loads(out)["deleted"]
        return deleted

    def delete(self, name, etag):
        return bool(self.delete_many([(name, etag)]))

    # --- the controller's lock, in place of the blob lease -----------------------------------

    def lease_state(self, name):
        """available, expired or leased, as the container's lease states read to the tool."""
        out = json.loads(self.step("lock-status", self._lock_vars(), wait_mins=2, timeout=120))
        if out["lock"] is None:
            return "available"
        return "expired" if out["expired"] else "leased"

    def _lock_vars(self):
        return {"LOCK_PLAN": self.plan} if self.plan_scope else {}

    def acquire(self, name):
        plan = re.sub(r"[^A-Za-z0-9._-]", "-", str(self.plan or "archive"))[:60]
        holder = f"archive-{plan}-{uuid.uuid4().hex[:12]}"
        started = self.clock()
        variables = {"LOCK_HOLDER": holder, "LOCK_HOST": re.sub(r"[^A-Za-z0-9._-]", "-", socket.gethostname()) or "archive",
                     "LOCK_PID": os.getpid(), "LOCK_TTL": LOCK_TTL_SECS, **self._lock_vars()}
        try:
            out = self.step("lock-acquire", variables, wait_mins=2, timeout=120)
            generation = int(json.loads(out.strip().splitlines()[-1])["generation"])
        except HandoffRefused as error:
            raise HandoffRefused(f"could not take the controller lock: a controller tick holds it? ({error})") from None
        except (ValueError, KeyError, IndexError, TypeError):
            raise HandoffRefused("lock-acquire returned no generation") from None
        self.fence = (holder, generation)
        self.lost = None
        self.unsure = False
        self._lock_deadline = started + LOCK_TTL_SECS
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._heartbeat, args=(holder, generation, self._stop),
                                        name="archive-lock-heartbeat", daemon=True)
        self._thread.start()
        return holder

    def _heartbeat(self, holder, generation, stop):
        while not stop.wait(self.heartbeat_secs):
            started = self.clock()
            variables = {"LOCK_HOLDER": holder, "LOCK_GENERATION": generation, "LOCK_TTL": LOCK_TTL_SECS,
                         **self._lock_vars()}
            try:
                self.step("lock-renew", variables, wait_mins=2, timeout=max(1, min(120, int(self._lock_deadline - started))))
            except HandoffRefused as error:
                text = str(error)
                if "lock moved on" in text or "lock expired" in text:
                    self.lost = "the controller lock was taken or expired"
                    return
                if self.clock() >= self._lock_deadline:
                    self.lost = f"no lock renewal for {LOCK_TTL_SECS} s"
                    return
            else:
                self._lock_deadline = started + LOCK_TTL_SECS

    def release(self, name, lease):
        if self._stop is not None:
            self._stop.set()
            self._thread.join(timeout=5)
        fence, self.fence = self.fence, None
        if fence is None:
            return
        variables = {"LOCK_HOLDER": fence[0], "LOCK_GENERATION": fence[1], **self._lock_vars()}
        if self.lost is None and not self.unsure:
            variables["LOCK_CLEAN"] = 1
        try:
            self.step("lock-release", variables, wait_mins=2, timeout=120)
        except HandoffRefused as error:
            # A release that fails expires by itself within the TTL.
            print(f"warning: lock-release failed ({error}); it expires in {LOCK_TTL_SECS} s", file=sys.stderr)
