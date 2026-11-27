#!/usr/bin/env python3
"""A time-boxed release window for a held app version (docs/boinc/apps/app-release.md, "Held versions").

For a plan class with no host of the project's own, the release itself has to be the qualification
run. As root on the BOINC host, this:

1. releases the held app_version with app-version-hold.php (--action release);
2. every --poll-secs, reads the version's reported results since the release with the read-only
   `app-version-outcomes` bridge step (no database credentials leave the server container), and
   the validator log for an early count of its Valid results;
3. holds the version again at once (--action hold) on the first client error, validate error or
   invalid result, or when --max-hours pass before --validated results validate;
4. ends the window, leaving the version released, once --validated results have validated.

Every action and every poll goes to a JSONL journal. Without --apply it only prints the commands.
Exit status: 0 qualified, 2 held on a failure, 3 held when the window expired, 1 tool error.
"""

import argparse
import json
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

HOLD_PHP = "/usr/local/lib/usnm-boinc/app-version-hold.php"
PROJECT = "/project/usnmocr"
VALIDATOR_LOG = Path("/srv/usnm-boinc/project/usnmocr/log_usnm-boinc/script_validator_usnm_qwen.log")
STEP = ["usnm-boinc-step", "--env", "prod", "--target", "bridge", "--step", "app-version-outcomes",
        "--wait-mins", "10"]
# The validator's HAV line after a Valid result names the app version: "[HOST#16 AV#112] ..."
HAV = re.compile(r"\[HOST#\d+ AV#(\d+)\]")
VALID = re.compile(r"\[RESULT#(\d+) [^\]]+\] Valid;")


def hold_cmd(app_version, action):
    return ["podman", "exec", "usnm-boinc-server", "php", HOLD_PHP, "--project", PROJECT,
            "--journal-dir", f"{PROJECT}/ops-app-versions", "--app-version", str(app_version),
            "--action", action, "--apply"]


def valid_in_log(lines, app_version):
    """Result ids the validator logged Valid for `app_version` (the HAV line after a Valid line)."""
    found, pending = set(), None
    for line in lines:
        m = VALID.search(line)
        if m:
            pending = m[1]
            continue
        m = HAV.search(line)
        if m and pending is not None:
            if int(m[1]) == app_version:
                found.add(pending)
            pending = None
    return found


def decide(outcomes, validated, now, deadline):
    """'fail', 'expired', 'qualified' or None (keep watching) from an app-version-outcomes document.
    Expiry comes before qualification: a read that lands after the deadline can't qualify."""
    if outcomes["client_errors"] or outcomes["validate_errors"] or outcomes["invalid"]:
        return "fail"
    if now >= deadline:
        return "expired"
    if outcomes["valid"] >= validated:
        return "qualified"
    return None


class Stop(Exception):
    """A signal ended the window; the cleanup holds the version again."""


class Window:
    def __init__(self, args, run=subprocess.run, clock=time.time, sleep=time.sleep, log=VALIDATOR_LOG):
        self.args, self.run, self.clock, self.sleep, self.log = args, run, clock, sleep, log
        # Set once a release may have changed the database; cleared by a confirmed hold or by
        # qualification. While set, every exit holds the version again.
        self.exposed = False
        self.journal = Path(args.journal_dir) / f"release-window-av{args.app_version}-{int(clock())}.jsonl"

    def note(self, event, **fields):
        line = {"at": int(self.clock()), "event": event, "app_version": self.args.app_version, **fields}
        print(json.dumps(line, sort_keys=True), flush=True)
        if self.args.apply:
            self.journal.parent.mkdir(parents=True, exist_ok=True)
            with open(self.journal, "a") as f:
                f.write(json.dumps(line, sort_keys=True) + "\n")

    def act(self, action, reason):
        cmd = hold_cmd(self.args.app_version, action)
        if not self.args.apply:
            self.note(f"would_{action}", reason=reason, command=cmd)
            return
        if action == "release":
            self.exposed = True
        done = self.run(cmd, capture_output=True, text=True)
        if action == "hold" and done.returncode == 0:
            self.exposed = False
        self.note(action, reason=reason, returncode=done.returncode, stderr=(done.stderr or "")[-400:])
        if done.returncode != 0:
            raise RuntimeError(f"app-version-hold.php --action {action} failed")

    def cleanup(self, reason):
        """Hold again after any exit that didn't qualify, without depending on the journal."""
        if not self.exposed:
            return
        try:
            done = self.run(hold_cmd(self.args.app_version, "hold"), capture_output=True, text=True)
            ok = done.returncode == 0
        except Exception as error:  # the hold itself couldn't start
            ok, done = False, None
            print(f"release-window: cleanup hold failed to start: {error}", file=sys.stderr)
        if ok:
            self.exposed = False
        try:
            self.note("cleanup_hold", reason=reason, ok=ok,
                      returncode=None if done is None else done.returncode)
        except Exception:
            print(f"release-window: cleanup hold {'done' if ok else 'FAILED'} ({reason}); "
                  "the journal could not be written", file=sys.stderr)

    def outcomes(self, since):
        done = self.run([*STEP, "--var", f"APP_VERSION_ID={self.args.app_version}", "--var", f"SINCE_UNIX={since}"],
                        capture_output=True, text=True)
        if done.returncode != 0:
            raise RuntimeError("app-version-outcomes failed")
        docs = [json.loads(line) for line in done.stdout.splitlines() if line.startswith("{")]
        if not docs or docs[-1].get("event") != "boinc_app_version_outcomes":
            raise RuntimeError("app-version-outcomes printed no report")
        return docs[-1]

    def log_valid(self, offset):
        try:
            with open(self.log, "rb") as f:
                f.seek(offset)
                data = f.read()
        except OSError:
            return set(), offset
        return valid_in_log(data.decode("utf-8", "replace").splitlines(), self.args.app_version), offset + len(data)

    def run_window(self):
        """Run the window; every exit that doesn't qualify after a release holds the version again."""
        previous = {}

        def stop(signum, _frame):
            raise Stop(signal.Signals(signum).name)
        for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            previous[sig] = signal.signal(sig, stop)
        reason = "window ended"
        try:
            return self.watch()
        except BaseException as error:  # KeyboardInterrupt, Stop, OSError, RuntimeError...
            reason = f"{type(error).__name__}: {error}"
            if not isinstance(error, (Exception, Stop, KeyboardInterrupt)):
                raise
            return 1
        finally:
            self.cleanup(reason)
            for sig, handler in previous.items():
                signal.signal(sig, handler)

    def watch(self):
        start = int(self.clock())
        deadline = start + self.args.max_hours * 3600
        offset = self.log.stat().st_size if self.log.exists() else 0
        self.note("window_open", validated=self.args.validated, max_hours=self.args.max_hours)
        self.act("release", "window opened")
        if not self.args.apply:
            self.note("dry_run_stops_here")
            return 0
        seen = set()
        while True:
            self.sleep(self.args.poll_secs)
            new, offset = self.log_valid(offset)
            seen |= new
            try:
                outcomes = self.outcomes(start)
            except (RuntimeError, ValueError) as error:
                self.act("hold", f"could not read outcomes: {error}")
                return 1
            verdict = decide(outcomes, self.args.validated, int(self.clock()), deadline)
            self.note("poll", outcomes={k: outcomes[k] for k in ("valid", "invalid", "client_errors",
                                                                   "validate_errors")},
                      validator_log_valid=len(seen), verdict=verdict)
            if verdict == "fail":
                self.act("hold", "failed result in the window")
                return 2
            if verdict == "expired":
                self.act("hold", f"fewer than {self.args.validated} valid results in {self.args.max_hours} h")
                return 3
            if verdict == "qualified":
                self.exposed = False
                self.note("window_qualified", valid=outcomes["valid"])
                return 0


def arguments(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--app-version", type=int, required=True, help="the held app_version id")
    p.add_argument("--validated", type=int, default=3, help="valid results that end the window (1-50)")
    p.add_argument("--max-hours", type=int, default=48, help="hold again if not qualified by then (1-168)")
    p.add_argument("--poll-secs", type=int, default=300, help="between reads (60-3600)")
    p.add_argument("--journal-dir", default="/root/release-window")
    p.add_argument("--apply", action="store_true", help="release and hold for real (default: print only)")
    args = p.parse_args(argv)
    if not (args.app_version > 0 and 1 <= args.validated <= 50 and 1 <= args.max_hours <= 168
            and 60 <= args.poll_secs <= 3600):
        p.error("--app-version > 0, --validated 1-50, --max-hours 1-168, --poll-secs 60-3600")
    return args


def main(argv=None):
    args = arguments(argv)
    return Window(args).run_window()


if __name__ == "__main__":
    sys.exit(main())
