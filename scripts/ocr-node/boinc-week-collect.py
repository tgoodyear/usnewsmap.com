#!/usr/bin/env python3
"""The BOINC week for the OCR dashboard: the last seven days in 3-hour buckets.

    boinc-week-collect.py --push-url http://<dashboard>/api/boinc-week
        [--out FILE] [--now EPOCH]

Reads the BOINC database on the server's own connection settings (four bounded, read-only
aggregate queries over the `result` table, which db_purge keeps for 30 days) and writes one small
JSON document of 56 buckets, aligned to US Eastern 00:00, 03:00, ... The last bucket ends at the
next boundary and is partial. Per bucket:

  queued      unsent results at the bucket's end (now, for the partial one), by app. Rebuilt from
              create_time and sent_time; a result that left the queue without being sent (server
              state 5 with no sent_time) is taken to have left at its mod_time, so this part is
              approximate.
  completed   successful results (outcome 1) received in the bucket, by app.
  volunteers, hosts
              distinct accounts and machines that were sent or reported a result in the bucket.

The document holds counts and app names only: no account, host or result ids, names or addresses
leave the server. With --push-url it PUTs the document to usnm-ocrdash, which takes it only from
the address its `boinc` throughput collector pushes from.
"""

import argparse
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import urllib.request
from zoneinfo import ZoneInfo

SCHEMA = 1
HOUR = 3600
BUCKET_HOURS = 3
DAYS = 7
EASTERN = ZoneInfo("America/New_York")
COMMAND_TIMEOUT = 60
APP_NAME = re.compile(r"[a-z][a-z0-9_]{0,39}")
# Aggregates by hour only. The session is read-only, each statement is cut off after five seconds,
# and the row limits hold even if the grouping were wrong. Each query is one pass over `result`
# (some 80,000 rows while db_purge keeps 30 days). The active machines' ids stay on this host:
# they are counted here and never sent.
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
// One read-only snapshot for every query, so the queue's two scans see the same rows.
if (!$db->do_query('START TRANSACTION READ ONLY, WITH CONSISTENT SNAPSHOT')) throw new Exception('no snapshot');
$apps = [];
$found = $db->do_query("SELECT id, name FROM app");
while ($row = $found->fetch_row()) $apps[(int)$row[0]] = $row[1];
$before = $from - 3600;
$entered = $read("SELECT /*+ MAX_EXECUTION_TIME(5000) */ appid,
    GREATEST(FLOOR(create_time/3600)*3600, $before), COUNT(*)
  FROM result GROUP BY 1, 2 LIMIT 20000");
$left = $read("SELECT /*+ MAX_EXECUTION_TIME(5000) */ appid,
    GREATEST(FLOOR(IF(sent_time > 0, sent_time, UNIX_TIMESTAMP(mod_time))/3600)*3600, $before), COUNT(*)
  FROM result WHERE sent_time > 0 OR server_state = 5 GROUP BY 1, 2 LIMIT 20000");
$completed = $read("SELECT /*+ MAX_EXECUTION_TIME(5000) */ appid, FLOOR(received_time/3600)*3600, COUNT(*)
  FROM result WHERE outcome = 1 AND received_time >= $from GROUP BY 1, 2 LIMIT 20000");
$active = $read("SELECT /*+ MAX_EXECUTION_TIME(5000) */ h, userid, hostid FROM (
    SELECT FLOOR(sent_time/3600)*3600 h, userid, hostid FROM result WHERE sent_time >= $from AND hostid > 0
    UNION SELECT FLOOR(received_time/3600)*3600, userid, hostid FROM result
      WHERE received_time >= $from AND hostid > 0) x LIMIT 200000");
$db->do_query('COMMIT');
echo json_encode(['active' => $active, 'apps' => $apps, 'completed' => $completed,
  'entered' => $entered, 'left' => $left]);
"""


def edges(now):
    """The buckets' starts and the window's end, in epoch seconds: US Eastern 3-hour boundaries,
    the last one after `now`, a week back."""
    local = datetime.fromtimestamp(now, EASTERN)
    end = local.replace(minute=0, second=0, microsecond=0, hour=local.hour - local.hour % BUCKET_HOURS)
    end = end + timedelta(hours=BUCKET_HOURS)
    # Wall-clock steps, so each start is a local boundary across a clock change.
    starts = []
    at = end.replace(tzinfo=None) - timedelta(days=DAYS)
    while at < end.replace(tzinfo=None):
        starts.append(int(at.replace(tzinfo=EASTERN).timestamp()))
        at += timedelta(hours=BUCKET_HOURS)
    return starts, int(end.timestamp())


def offset_minutes(at):
    return int(datetime.fromtimestamp(at, EASTERN).utcoffset().total_seconds() // 60)


def aggregate(rows, now):
    starts, end = edges(now)
    apps = {int(key): name for key, name in rows["apps"].items() if APP_NAME.fullmatch(name)}
    bounds = starts + [end]

    def bucket(hour):
        """The bucket an hour (or anything in it) falls in, or None outside the window."""
        if hour < starts[0] or hour >= end:
            return None
        for index in range(len(starts)):
            if hour < bounds[index + 1]:
                return index
        return None

    # Queue changes by hour, per app; everything before the window is in the hour before it.
    changes = {}
    for appid, hour, count in rows["entered"]:
        changes.setdefault(appid, {}).setdefault(hour, 0)
        changes[appid][hour] += count
    for appid, hour, count in rows["left"]:
        changes.setdefault(appid, {}).setdefault(hour, 0)
        changes[appid][hour] -= count

    buckets = []
    for index, start in enumerate(starts):
        stop = bounds[index + 1]
        partial = stop > now
        queued = {}
        for appid, hours in changes.items():
            name = apps.get(appid)
            if name is None:
                continue
            # At the bucket's end: everything in the hours before it; the partial bucket, all of it.
            size = sum(count for hour, count in hours.items() if partial or hour < stop)
            if size > 0:
                queued[name] = size
        buckets.append({"completed": {}, "partial": partial, "queued": queued, "start": start,
                        "utc_offset_minutes": offset_minutes(start)})

    for appid, hour, count in rows["completed"]:
        index = bucket(hour)
        name = apps.get(appid)
        if index is not None and name is not None:
            done = buckets[index]["completed"]
            done[name] = done.get(name, 0) + count

    active = [(set(), set()) for _ in starts]
    for hour, user, host in rows["active"]:
        index = bucket(hour)
        if index is None:
            continue
        users, hosts = active[index]
        users.add(user)
        hosts.add(host)
    for b, (users, hosts) in zip(buckets, active):
        b["volunteers"] = len(users)
        b["hosts"] = len(hosts)
        b["completed"] = dict(sorted(b["completed"].items()))
        b["queued"] = dict(sorted(b["queued"].items()))
    return {"bucket_hours": BUCKET_HOURS, "buckets": buckets, "generated_at": int(now),
            "generated_utc_offset_minutes": offset_minutes(now), "schema": SCHEMA}


def boinc_rows(since):
    output = subprocess.run(
        ["podman", "exec", "--workdir", "/project/usnmocr/html/inc", "usnm-boinc-server",
         "php", "-r", PHP, str(int(since))],
        check=True, text=True, capture_output=True, errors="replace", timeout=COMMAND_TIMEOUT,
        env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "TZ": "UTC"}).stdout
    return json.loads(output[output.index("{"):])


def write(path, document):
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", dir=target.parent, delete=False) as stream:
        json.dump(document, stream, separators=(",", ":"), sort_keys=True)
    os.chmod(stream.name, 0o644)
    os.replace(stream.name, target)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--push-url", help="PUT the document to the dashboard")
    parser.add_argument("--out", help="also write the document here")
    parser.add_argument("--now", type=int, help="the time to collect at, for tests")
    args = parser.parse_args()
    now = args.now if args.now is not None else int(datetime.now(timezone.utc).timestamp())
    starts, _ = edges(now)
    document = aggregate(boinc_rows(starts[0]), now)
    if args.out:
        write(args.out, document)
    if args.push_url:
        body = json.dumps(document, separators=(",", ":"), sort_keys=True).encode()
        request = urllib.request.Request(args.push_url, data=body, method="PUT",
                                         headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=20):
            pass
    print(json.dumps({"buckets": len(document["buckets"]), "event": "boinc_week_collected"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
