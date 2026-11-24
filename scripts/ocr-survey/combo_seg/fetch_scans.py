#!/usr/bin/env python3
"""Fetch planned Chronicling America scans once (#2107 combined segmenter, training pages only).

At most one request a second (--min-interval), generic User-Agent, each scan written with a sha256 sidecar
(<stem>.jpg.json: url, sha256, bytes, fetched_at, user_agent), never re-fetched and never deleted. A URL that
returns 404 three times (--max-404) is given up and logged; any other error is retried the same way up to
--max-attempts times. Progress counts go to stdout; every outcome is appended to <scan-dir>/fetch-log.jsonl.

    fetch_scans.py --plan fetch-plan.json --scan-dir D [--max-404 3] [--max-attempts 3] [--min-interval 1.0]
"""
import argparse
import datetime
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request

UA = "research-client/1.0"


class Fetcher:
    def __init__(self, min_interval=1.0, opener=None, clock=time.time, sleep=time.sleep):
        self.min_interval, self.last = min_interval, -1e18
        self.opener = opener or (lambda req: urllib.request.urlopen(req, timeout=120))
        self.clock, self.sleep = clock, sleep

    def get(self, url):
        wait = self.min_interval - (self.clock() - self.last)
        if wait > 0:
            self.sleep(wait)
        self.last = self.clock()
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        with self.opener(req) as r:
            return r.read()


def fetch_entry(f, e, scan_dir, max_404=3, max_attempts=3):
    """(status, info): status 'fetched', 'held', 'gave_up_404' or 'failed'."""
    path = os.path.join(scan_dir, e["stem"] + ".jpg")
    if os.path.exists(path) or os.path.exists(path + ".json"):
        return "held", {}
    n404 = n = 0
    last = None
    while n404 < max_404 and n < max_attempts:
        n += 1
        try:
            data = f.get(e["url"])
        except urllib.error.HTTPError as ex:
            last = "HTTP %d" % ex.code
            if ex.code == 404:
                n404 += 1
            continue
        except Exception as ex:  # network errors: retried, counted as attempts
            last = repr(ex)[:200]
            continue
        tmp = path + ".part"
        with open(tmp, "wb") as fh:
            fh.write(data)
        os.rename(tmp, path)
        side = {"url": e["url"], "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data),
                "fetched_at": datetime.datetime.now(datetime.timezone.utc).isoformat(), "user_agent": UA}
        with open(path + ".json", "w") as fh:
            json.dump(side, fh, sort_keys=True)
        return "fetched", dict(side, attempts=n)
    return ("gave_up_404" if n404 >= max_404 else "failed"), {"attempts": n, "last_error": last}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--plan", required=True)
    ap.add_argument("--scan-dir", required=True)
    ap.add_argument("--max-404", type=int, default=3)
    ap.add_argument("--max-attempts", type=int, default=3)
    ap.add_argument("--min-interval", type=float, default=1.0)
    a = ap.parse_args(argv)
    if a.min_interval < 1.0:
        raise SystemExit("at most one request a second")
    plan = json.load(open(a.plan))["fetch"]
    os.makedirs(a.scan_dir, exist_ok=True)
    f = Fetcher(a.min_interval)
    counts = {}
    t0 = time.time()
    with open(os.path.join(a.scan_dir, "fetch-log.jsonl"), "a") as log:
        for i, e in enumerate(plan, 1):
            st, info = fetch_entry(f, e, a.scan_dir, a.max_404, a.max_attempts)
            counts[st] = counts.get(st, 0) + 1
            log.write(json.dumps(dict(info, page_id=e["page_id"], stem=e["stem"], url=e["url"], status=st), sort_keys=True) + "\n")
            log.flush()
            if i % 10 == 0 or i == len(plan):
                print("scans %d/%d %s %.0fs" % (i, len(plan), json.dumps(counts, sort_keys=True), time.time() - t0), flush=True)
    print(json.dumps(counts, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
