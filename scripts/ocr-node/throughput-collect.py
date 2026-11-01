#!/usr/bin/env python3
"""Hourly pipeline counters for the OCR dashboard's step throughput panel (#704).

    throughput-collect.py --source controller|ship|boinc [--hours 3] [--out FILE]
        [--push-url http://<dashboard>/api/throughput/<source>] [--now EPOCH]

Reads what each host already records and writes one small JSON document of hourly counts:

  controller  the priority controller's journal: ticks, phases, pages claimed and collected
  ship        the shipper's and result loaders' journals: pages shipped, staged, submitted, loaded
  boinc       the BOINC database (two bounded, read-only aggregate queries, on the server's
              own connection settings) and podman's step containers (bridge lock hold time)

Device seconds are every returned replica of the workunits that got a canonical result in the
hour, failures and inconclusive replicas included: what a finished page cost.

It takes no lock, changes nothing, and reads no page text, file names or result output: only
counts, durations and ids of steps. Every hour in the window is recomputed on each run, so a late
or repeated run corrects a bucket and never adds to it. The window ends at the last complete hour,
so a source that stops leaves unknown hours, not a short one. A value is `null` for an hour its
source doesn't reach back to; 0 means the hour was read and nothing happened.

With --push-url it PUTs the document to usnm-ocrdash, which takes it from this host's address
only and keeps the history. --hours 336 once backfills the dashboard's two weeks.
"""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import urllib.request

SCHEMA = 1
HOUR = 3600
MAX_HOURS = 336
# Each journal is read this far before the window, to see a chunk's earlier attempts and the
# last cumulative counter.
LOOKBACK_HOURS = 48
COMMAND_TIMEOUT = 60
CONTROLLER_UNIT = "usnm-boinc-priority-controller.service"
SHIP_UNITS = {"usnm-quality-ship.service": "quality", "usnm-qwen-ship.service": "qwen"}
QUALITY_LOAD_UNIT = "usnm-quality-results.service"
QWEN_LOAD_UNIT = "usnm-qwen-results.service"
LOADERS = {QUALITY_LOAD_UNIT: "quality", QWEN_LOAD_UNIT: "qwen"}
# The apps a page goes through, and the database's result states (boinc/html/inc/common_defs.inc).
APPS = {"usnm_ocr": "ocr", "usnm_quality": "quality", "usnm_qwen": "qwen"}
SUCCESS = 1
VALIDATE_NAMES = {0: "waiting", 1: "valid", 2: "invalid", 3: "invalid", 4: "inconclusive",
                  5: "invalid", 6: "invalid"}
RESULT_SERIES = ("valid", "inconclusive", "invalid", "waiting", "error", "units", "elapsed_seconds",
                 "bundles")
STEP_NAME = re.compile(r"usnm-boinc-step-[0-9]{8}T[0-9]{6}Z-([a-z0-9-]{1,40})")
PHASE_NAME = re.compile(r"[a-z0-9-]{1,40}")
SHIP_LINE = re.compile(r"quality-ship: (c[0-9]{5}): (.*)")
ENV_LINE = re.compile(r"([A-Z_]+)=([0-9]+)")
# Aggregates only. The session is read-only, each statement is cut off after five seconds, and
# the row limit holds even if the grouping were wrong. Scans ind_res_st, some 30,000 rows while
# db_purge keeps 30 days.
PHP = r"""
require_once 'boinc_db.inc';
$db = BoincDb::get(true);
$from = (int)$argv[1];
$read = function ($sql) use ($db) {
  $rows = [];
  $found = $db->do_query($sql);
  if (!$found) throw new Exception('query failed');
  while ($row = $found->fetch_row()) $rows[] = array_map('intval', $row);
  return $rows;
};
$db->do_query('SET SESSION TRANSACTION READ ONLY');
$apps = [];
$found = $db->do_query("SELECT id, name FROM app WHERE name IN ('usnm_ocr','usnm_quality','usnm_qwen')");
while ($row = $found->fetch_row()) $apps[(int)$row[0]] = $row[1];
$ids = implode(',', array_keys($apps));
$results = $read("SELECT /*+ MAX_EXECUTION_TIME(5000) */ appid, FLOOR(received_time/3600)*3600,
    outcome = 1, validate_state, COUNT(*), SUM(name LIKE 'usnm\\_ocrb\\_%')
  FROM result WHERE server_state = 5 AND received_time >= $from AND appid IN ($ids)
  GROUP BY 1, 2, 3, 4 LIMIT 20000");
$units = $read("SELECT /*+ MAX_EXECUTION_TIME(5000) */ c.appid, FLOOR(c.received_time/3600)*3600,
    COUNT(DISTINCT c.id), ROUND(SUM(r.elapsed_time))
  FROM result c JOIN workunit w ON w.id = c.workunitid AND w.canonical_resultid = c.id
  JOIN result r ON r.workunitid = c.workunitid AND r.received_time > 0
  WHERE c.server_state = 5 AND c.received_time >= $from AND c.appid IN ($ids)
  GROUP BY 1, 2 LIMIT 20000");
echo json_encode(['apps' => $apps, 'results' => $results, 'units' => $units]);
"""


class Window:
    """Complete hours first..first + hours - 1, of which those before `start` weren't observed."""

    def __init__(self, now, hours):
        self.now = now
        self.last = now // HOUR * HOUR - HOUR
        self.hours = hours
        self.first = self.last - (hours - 1) * HOUR
        self.start = self.first
        self.series = {}

    def declare(self, name):
        return self.series.setdefault(name, [
            None if self.first + index * HOUR < self.start else 0 for index in range(self.hours)])

    def add(self, name, at, amount=1):
        values = self.declare(name)
        index = (int(at) // HOUR * HOUR - self.first) // HOUR
        if 0 <= index < self.hours and values[index] is not None:
            values[index] += amount

    def spread(self, name, begin, end):
        """Seconds of [begin, end), each in the hour it fell in."""
        at = begin
        while at < end:
            stop = min(end, (at // HOUR + 1) * HOUR)
            self.add(name, at, stop - at)
            at = stop

    def forget_before(self, wanted, oldest):
        """Series `wanted` picks come from a record that begins at `oldest`, None if it is
        empty: its first whole hour is the first observed."""
        start = self.now + HOUR if oldest is None else -(-oldest // HOUR) * HOUR
        for name, values in self.series.items():
            if wanted(name):
                for index in range(self.hours):
                    if self.first + index * HOUR < start:
                        values[index] = None

    def document(self, source, limits):
        return {
            "schema": SCHEMA,
            "source": source,
            "observed_at": datetime.fromtimestamp(self.now, timezone.utc).isoformat().replace("+00:00", "Z"),
            "first_hour": self.first,
            "hours": self.hours,
            "series": {name: [None if v is None else round(v, 3) for v in values]
                       for name, values in sorted(self.series.items())},
            "limits": limits,
        }


def run(command, stdin=None):
    return subprocess.run(command, check=True, text=True, capture_output=True, input=stdin,
                          errors="replace", timeout=COMMAND_TIMEOUT,
                          env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "TZ": "UTC"}).stdout


def journal(units, since):
    """The units' entries since `since`, oldest first, as (epoch seconds, unit, message)."""
    command = ["journalctl", "--no-pager", "--quiet", "--output=json", f"--since=@{int(since)}"]
    for unit in units:
        command += ["--unit", unit]
    for line in run(command).splitlines():
        try:
            entry = json.loads(line)
            at = int(entry["__REALTIME_TIMESTAMP"]) / 1e6
        except (ValueError, KeyError, TypeError):
            continue
        message = entry.get("MESSAGE")
        # systemd's own lines about a unit name it in UNIT, and are init.scope's otherwise.
        if isinstance(message, str):
            yield at, entry.get("UNIT") or entry.get("_SYSTEMD_UNIT", ""), message


def oldest_journal_entry(units):
    """When the units' journal starts, or None if it is empty."""
    command = ["journalctl", "--no-pager", "--quiet", "--output=short-unix", "--lines=all"]
    for unit in units:
        command += ["--unit", unit]
    process = subprocess.Popen(command, stdout=subprocess.PIPE, text=True, errors="replace")
    try:
        first = process.stdout.readline().split(" ", 1)[0]
    finally:
        process.kill()
        process.communicate()
    try:
        return int(float(first))
    except ValueError:
        return None


def json_event(message):
    """A journal line that is a JSON object, or None."""
    if not message.startswith("{"):
        return None
    try:
        event = json.loads(message)
    except ValueError:
        return None
    return event if isinstance(event, dict) else None


def number(value):
    return value if type(value) in (int, float) and value >= 0 else 0


def collect_controller(window, entries):
    for name in ("ticks_ok", "ticks_failed", "tick_seconds", "ocr_claim_pages", "ocr_collect_pages"):
        window.declare(name)
    plan, selected, limits = None, None, {}
    for _, _, message in entries:
        event = json_event(message)
        if event is None or type(event.get("at")) is not int:
            continue
        at, kind = event["at"], event.get("event")
        if kind == "boinc_batch_phase":
            phase = event.get("phase")
            if isinstance(phase, str) and PHASE_NAME.fullmatch(phase):
                window.add(f"phase_runs.{phase}", at)
                window.add(f"phase_seconds.{phase}", at, number(event.get("seconds")))
                window.add(f"phase_failed.{phase}", at, event.get("ok") is not True)
        elif kind in ("boinc_batch_tick", "boinc_batch_failure"):
            good = kind == "boinc_batch_tick"
            window.add("ticks_ok" if good else "ticks_failed", at)
            timings = event.get("timings")
            if isinstance(timings, dict):
                window.add("tick_seconds", at, number(timings.get("tick_seconds")))
            if good:
                window.add("ocr_collect_pages", at, number(event.get("newly_published")))
                # A new plan starts its count again.
                if event.get("plan") == plan and selected is not None:
                    window.add("ocr_claim_pages", at, max(0, number(event.get("selected")) - selected))
                plan, selected = event.get("plan"), number(event.get("selected"))
                limits = {"usnm_ocr": f"{int(number(event.get('high_watermark')))} runnable workunits "
                                      "(controller high watermark)",
                          "ocr_claim": f"{int(number(event.get('high_watermark')))} pending pages; "
                                       f"{int(number(event.get('max_global_pending')))} across all apps"}
    limits["controller"] = "one tick at a time, on a 5 minute timer"
    return limits


def collect_ship(window, entries):
    for lane in SHIP_UNITS.values():
        for name in ("ship_pages", "ship_seconds", "stage_pages", "submit_pages", "deferred"):
            window.declare(f"{lane}_{name}")
    for name in ("quality_load_pages", "quality_load_seconds", "quality_load_failed",
                 "qwen_load_pages", "qwen_load_seconds", "qwen_load_failed"):
        window.declare(name)
    chunks, seen_units = {}, set()
    began = {}
    readings = pending_readings = None
    for at, unit, message in entries:
        if unit in LOADERS:
            lane = LOADERS[unit]
            ended = message.startswith((f"{unit}: Failed with result", f"{unit}: Deactivated successfully"))
            if message.startswith("Starting "):
                began[unit] = at
                pending_readings = None if unit == QWEN_LOAD_UNIT else pending_readings
            elif ended and unit in began:
                window.spread(f"{lane}_load_seconds", began.pop(unit), at)
            if "Failed with result" in message and message.startswith(f"{unit}:"):
                window.add(f"{lane}_load_failed", at)
        if unit in SHIP_UNITS:
            lane = SHIP_UNITS[unit]
            match = SHIP_LINE.fullmatch(message)
            if match is None:
                continue
            chunk = chunks.setdefault((lane, match[1]), {"seen": set()})
            text = match[2]
            announced = re.fullmatch(r"([0-9]+) pages, server unsent .*", text)
            received = re.fullmatch(r"received ([0-9]+) pages .*", text)
            if announced:
                chunk["began"] = at
            elif received:
                chunk["pages"] = int(received[1])
                # A chunk retried counts once, at its first attempt.
                if "ship" not in chunk["seen"]:
                    chunk["seen"].add("ship")
                    window.add(f"{lane}_ship_pages", at, chunk["pages"])
                    if "began" in chunk:
                        window.spread(f"{lane}_ship_seconds", chunk["began"], at)
            elif text.startswith("staged;") and "stage" not in chunk["seen"]:
                chunk["seen"].add("stage")
                window.add(f"{lane}_stage_pages", at, chunk.get("pages", 0))
            elif text.startswith("submitted,") and "submit" not in chunk["seen"]:
                chunk["seen"].add("submit")
                window.add(f"{lane}_submit_pages", at, chunk.get("pages", 0))
            elif "deferred" in text:
                window.add(f"{lane}_deferred", at)
        elif unit == QUALITY_LOAD_UNIT:
            event = json_event(message)
            if event and "selected_unit" in event:
                key = (event.get("campaign"), event.get("selected_unit"))
                # A visit before the results are in loads no pages, and the unit comes back.
                if key not in seen_units and number(event.get("pages")) > 0:
                    seen_units.add(key)
                    window.add("quality_load_pages", at, number(event.get("pages")))
        elif unit == QWEN_LOAD_UNIT:
            event = json_event(message)
            summary = re.match(r"Summary \{ pages: [0-9]+, with_readings: ([0-9]+),", message)
            if summary:
                pending_readings = int(summary[1])
            elif event and "loaded_at" in event and pending_readings is not None:
                # The summary comes before the load, which can still fail.
                if readings is not None:
                    window.add("qwen_load_pages", at, max(0, pending_readings - readings))
                readings, pending_readings = pending_readings, None
    return {}


def ship_limits(env_file, qwen_environment):
    settings = {}
    try:
        for line in Path(env_file).read_text().splitlines():
            match = ENV_LINE.fullmatch(line.strip())
            if match:
                settings[match[1]] = int(match[2])
    except OSError:
        pass
    limits = {}
    if "MAX_CHUNKS" in settings:
        limits["quality_ship"] = f"{settings['MAX_CHUNKS']} chunk a run"
    if "MAX_UNSENT" in settings:
        limits["usnm_quality"] = f"{settings['MAX_UNSENT']} unsent results before shipping waits"
    match = re.search(r"\bQWEN_MAX_READY=([0-9]+)", qwen_environment)
    if match:
        limits["usnm_qwen"] = f"{match[1]} ready replicas before shipping defers"
    return limits


def collect_boinc(window, rows):
    apps = {int(key): APPS[name] for key, name in rows["apps"].items() if name in APPS}
    for lane in apps.values():
        for name in RESULT_SERIES:
            window.declare(f"{lane}_{name}")
    for appid, hour, success, state, count, bundles in rows["results"]:
        lane = apps[appid]
        name = VALIDATE_NAMES.get(state, "invalid") if success == SUCCESS else "error"
        window.add(f"{lane}_{name}", hour, count)
        window.add(f"{lane}_bundles", hour, bundles)
    for appid, hour, count, seconds in rows["units"]:
        window.add(f"{apps[appid]}_units", hour, count)
        window.add(f"{apps[appid]}_elapsed_seconds", hour, seconds)


def collect_steps(window, events):
    """Each step container's run: the bridge lock is held for all of it. `events` are podman's
    start and died events, oldest first."""
    window.declare("lock_seconds")
    started = {}
    for event in events:
        match = STEP_NAME.fullmatch(event.get("Name", ""))
        if match is None:
            continue
        at = datetime.fromisoformat(re.sub(r"(\.[0-9]{6})[0-9]*", r"\1", event["Time"]).replace("Z", "+00:00"))
        at = at.timestamp()
        if event.get("Status") == "start":
            # The lock lets one step run at a time, so an earlier step with no died event ended
            # some time unknown (a restart): it isn't counted.
            started = {event["Name"]: at}
        elif event["Name"] in started:
            begin = started.pop(event["Name"])
            window.spread("lock_seconds", begin, at)
            window.spread(f"step_seconds.{match[1]}", begin, at)
            window.add(f"step_runs.{match[1]}", at)
    # The step still running holds the lock up to now.
    for name, begin in started.items():
        window.spread("lock_seconds", begin, window.now)
        window.spread(f"step_seconds.{STEP_NAME.fullmatch(name)[1]}", begin, window.now)


def boinc_rows(since):
    output = run(["podman", "exec", "--workdir", "/project/usnmocr/html/inc", "usnm-boinc-server",
                  "php", "-r", PHP, str(int(since))])
    return json.loads(output[output.index("{"):])


def step_events(since):
    output = run(["podman", "events", "--stream=false", f"--since={int(since)}", "--filter", "event=start",
                  "--filter", "event=died", "--format", "{{json .}}"])
    events = []
    for line in output.splitlines():
        try:
            events.append(json.loads(line))
        except ValueError:
            continue
    return events


def collect(args):
    window = Window(args.now if args.now is not None else int(datetime.now(timezone.utc).timestamp()), args.hours)
    since = window.first - LOOKBACK_HOURS * HOUR
    if args.source == "controller":
        limits = collect_controller(window, journal([CONTROLLER_UNIT], since))
        window.forget_before(lambda name: True, oldest_journal_entry([CONTROLLER_UNIT]))
    elif args.source == "ship":
        collect_ship(window, journal([*SHIP_UNITS, *LOADERS], since))
        for unit, lane in SHIP_UNITS.items():
            window.forget_before(lambda name, lane=lane: name.startswith(f"{lane}_") and "_load_" not in name,
                                 oldest_journal_entry([unit]))
        for unit, lane in LOADERS.items():
            window.forget_before(lambda name, lane=lane: name.startswith(f"{lane}_load_"),
                                 oldest_journal_entry([unit]))
        environment = run(["systemctl", "show", "--property=Environment", "--value", "usnm-qwen-ship.service"])
        limits = ship_limits(args.ship_env, environment)
    else:
        rows = boinc_rows(window.first)
        collect_boinc(window, rows)
        collect_steps(window, step_events(window.first - HOUR))
        # podman's events are in the journal, which may not reach as far as the database.
        window.forget_before(lambda name: name == "lock_seconds" or name.startswith("step_"),
                             oldest_journal_entry([]))
        limits = {}
    return window.document(args.source, limits)


def write(path, document):
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", dir=target.parent, delete=False) as stream:
        json.dump(document, stream, separators=(",", ":"))
    os.chmod(stream.name, 0o644)
    os.replace(stream.name, target)


def hours(text):
    value = int(text)
    if not 1 <= value <= MAX_HOURS:
        raise argparse.ArgumentTypeError(f"1 to {MAX_HOURS}")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--source", required=True, choices=("controller", "ship", "boinc"))
    parser.add_argument("--hours", type=hours, default=3, help="complete hours to recompute, up to the last one")
    parser.add_argument("--out", help="also write the document here")
    parser.add_argument("--push-url", help="PUT the document to the dashboard")
    parser.add_argument("--ship-env", default="/etc/usnm/quality-ship.env", help="the shipper's settings")
    parser.add_argument("--now", type=int, help="the time to collect at, for tests")
    args = parser.parse_args()
    document = collect(args)
    if args.out:
        write(args.out, document)
    if args.push_url:
        request = urllib.request.Request(args.push_url, data=json.dumps(document, separators=(",", ":")).encode(),
                                         method="PUT", headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=20):
            pass
    print(json.dumps({"event": "throughput_collected", "source": args.source, "hours": args.hours,
                      "series": len(document["series"])}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
