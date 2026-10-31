#!/usr/bin/env python3
"""Fill Cloudflare's cache with the example searches (06 section 6.5).

Requests each URL in the site's edge-warm.json at the served v and g, as the
web app would, then the coverage cube its response links to. It goes through
Cloudflare, so with Tiered Cache the upper tier fills too and other locations
can answer without the origin. The API doesn't log these as searches: they
carry no browser headers.

Prints counts only, never a search's text. Exits 1 if nothing was warmed.
"""

import argparse
import concurrent.futures
import gzip
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = "usnm-edge-warm"
TIMEOUT_SECS = 120
ATTEMPTS = 6
RETRIED = (202, 429, 502, 503, 504)


def fetch(url, sleep=time.sleep):
    """The status, body and cf-cache-status of `url`, waiting out a busy API."""
    request = urllib.request.Request(url, headers={
        "Accept": "application/json", "Accept-Encoding": "gzip", "User-Agent": USER_AGENT})
    status = 0
    for attempt in range(ATTEMPTS):
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECS) as response:
                status, headers, body = response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            status, headers, body = error.code, error.headers, b""
        except OSError:
            status, headers, body = 0, {}, b""
        if status not in RETRIED and status != 0:
            if headers.get("Content-Encoding") == "gzip":
                body = gzip.decompress(body)
            return status, body, (headers.get("cf-cache-status") or "NONE").upper()
        if attempt < ATTEMPTS - 1:
            sleep(int(headers.get("Retry-After") or 2))
    return status, b"", "NONE"


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


def run(base, workers, sleep=time.sleep):
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
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(lambda q: warm(base, q, version, generation, sleep), queries))
    counts = {}
    for result in results:
        key = result or "FAILED"
        counts[key] = counts.get(key, 0) + 1
    return {"version": version, "generation": generation, "searches": len(queries), "cache": counts}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--base-url", default="https://usnewsmap.com")
    parser.add_argument("--workers", type=int, default=2)
    args = parser.parse_args()
    if args.workers < 1:
        parser.error("--workers must be at least 1")
    try:
        report = run(args.base_url.rstrip("/"), args.workers)
    except (RuntimeError, ValueError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True))
    warmed = report["searches"] - report["cache"].get("FAILED", 0)
    return 0 if warmed else 1


if __name__ == "__main__":
    sys.exit(main())
