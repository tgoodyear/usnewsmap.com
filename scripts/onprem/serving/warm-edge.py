#!/usr/bin/env python3
"""Fill Cloudflare's cache with the example searches (06 section 6.5).

Requests each URL in the site's edge-warm.json at the served v and g, as the
web app would, then the coverage cube its response links to. It goes through
Cloudflare, so with Tiered Cache the upper tier fills too and other locations
can answer without the origin. The API doesn't log these as searches: they
carry no browser headers.

It takes one search at a time with a pause between them, so a cold search uses
at most one of the API's computation slots and visitors keep the other. Prints
counts only, never a search's text. Exits 1 if nothing was warmed.
"""

import argparse
import gzip
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = "usnm-edge-warm"
TIMEOUT_SECS = 120
# A cold search can compute for 120 s after waiting for a slot.
COMPUTING_LIMIT_SECS = 600
FAILURES = 6
RETRIED = (429, 502, 503, 504)


def fetch(url, sleep=time.sleep, clock=time.monotonic):
    """The status, body and cf-cache-status of `url`, waiting out a busy API.

    A 202 is asked again until COMPUTING_LIMIT_SECS passes; anything else
    temporary gets FAILURES tries.
    """
    request = urllib.request.Request(url, headers={
        "Accept": "application/json", "Accept-Encoding": "gzip", "User-Agent": USER_AGENT})
    deadline = clock() + COMPUTING_LIMIT_SECS
    failures = 0
    while True:
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECS) as response:
                status, headers, body = response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            status, headers, body = error.code, error.headers, b""
        except OSError:
            status, headers, body = 0, {}, b""
        if status == 202:
            if clock() >= deadline:
                return status, b"", "NONE"
        elif status == 0 or status in RETRIED:
            failures += 1
            if failures >= FAILURES:
                return status, b"", "NONE"
        else:
            if headers.get("Content-Encoding") == "gzip":
                body = gzip.decompress(body)
            return status, body, (headers.get("cf-cache-status") or "NONE").upper()
        sleep(int(headers.get("Retry-After") or 2))


def warm(base, query, version, generation, sleep=time.sleep):
    """Warm one search and its coverage. Returns its cache status, or None."""
    pinned = urllib.parse.urlencode({"v": version, "g": generation})
    status, body, cache = fetch(f"{base}/v1/aggregate?{query}&{pinned}", sleep)
    if status != 200:
        return None
    ref = json.loads(body).get("cube", {}).get("baseline_ref")
    if ref:
        fetch(base + ref.replace("/api/v1/", "/v1/", 1), sleep)
    return cache


def run(base, pause, sleep=time.sleep):
    status, body, _ = fetch(f"{base}/v1/meta?warm={int(time.time())}", sleep)
    if status != 200:
        raise RuntimeError(f"/v1/meta answered {status}")
    meta = json.loads(body)
    version, generation = meta["index_version"], meta.get("edge_generation")
    if not generation:
        raise RuntimeError("/v1/meta has no edge_generation")
    status, body, _ = fetch(f"{base}/edge-warm.json", sleep)
    if status != 200:
        raise RuntimeError(f"/edge-warm.json answered {status}")
    queries = json.loads(body)
    counts = {}
    for query in queries:
        key = warm(base, query, version, generation, sleep) or "FAILED"
        counts[key] = counts.get(key, 0) + 1
        sleep(pause)
    return {"version": version, "generation": generation, "searches": len(queries), "cache": counts}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--base-url", default="https://usnewsmap.com")
    parser.add_argument("--pause-secs", type=float, default=1, help="between searches")
    args = parser.parse_args()
    if args.pause_secs < 0:
        parser.error("--pause-secs can't be negative")
    try:
        report = run(args.base_url.rstrip("/"), args.pause_secs)
    except (RuntimeError, ValueError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True))
    warmed = report["searches"] - report["cache"].get("FAILED", 0)
    return 0 if warmed else 1


if __name__ == "__main__":
    sys.exit(main())
