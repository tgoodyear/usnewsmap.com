#!/usr/bin/env python3
"""Time the site's pages, assets and a search over HTTP: DNS, connect, TLS, time to first byte.

    scripts/bench-site-speed.py [--base-url URL] [--runs 5] [--json FILE]
    scripts/bench-site-speed.py [--base-url URL] --globalping [--json FILE]

Without --globalping it times the site from where it runs, one connection per
sample so each includes DNS, TCP and TLS, and then once more on the same
connection ("warm"). Run it from every network that matters: a cloud VM, a
laptop, the operator's network. With --globalping it asks Globalping's free
public probes (https://globalping.io, no account, 250 measurements an hour)
to make one request each from about a dozen countries. Probes are public, so
a hostname behind an IP allow rule answers them 403: run the first form from
an allowed network instead, once from the operator's and once from outside it.

What it requests, each by its own name:
  home         `/`, the page's HTML
  search-page  `/?q=...`, the same HTML a search link serves
  datasets     `/datasets`, an app page the API serves without a validator
  meta         `/v1/meta`, a small API answer
  search       `/v1/aggregate?...`, one of the site's own example searches.
               It is usually in the API's cache, so this is the cached path;
               cold searches are bench-cold-searches.py.
  asset:...    each `/assets/` file the home page names (script, stylesheet)

Columns, in milliseconds: dns, tcp and tls are the connection set-up; wait is
the time from sending the request to the first byte of the response (the
server's share plus one round trip); ttfb is dns + tcp + tls + wait, the
"time to first byte" a browser reports for a first visit; dl is the rest of
the body; warm is wait again on the open connection. `cache` is a CDN's
cache status header when there is one.

This measures the network and the origin. It does not render: for the largest
contentful paint, see scripts/bench-site-lcp.mjs, or paste a URL into
https://pagespeed.web.dev. Every request is a GET of a page, a small file or
one cached search; the default of 5 runs sends about 70 requests to the site.
"""

import argparse
import concurrent.futures
import gzip
import http.client
import json
import re
import socket
import ssl
import statistics
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = "usnm-bench/1 (scripts/bench-site-speed.py)"
TIMEOUT_S = 30

# One of the site's own example searches (web/src/examples.json).
SEARCH_QUERY = {"q": "whigs", "mode": "phrase", "from": "1834-01-01", "to": "1856-12-31", "bucket": "month"}

# Globalping probes: (label, location). A country gives a random probe in it.
# The magic string is Globalping's own matcher.
GLOBALPING_LOCATIONS = [
    ("US", {"magic": "united states", "limit": 3}),
    ("CA", {"magic": "canada"}),
    ("GB", {"magic": "united kingdom"}),
    ("DE", {"magic": "germany"}),
    ("IN", {"magic": "india"}),
    ("SG", {"magic": "singapore"}),
    ("JP", {"magic": "japan"}),
    ("AU", {"magic": "australia"}),
    ("BR", {"magic": "brazil"}),
    ("ZA", {"magic": "south africa"}),
]
GLOBALPING_API = "https://api.globalping.io/v1/measurements"
CACHE_HEADERS = ("cf-cache-status", "x-cache", "x-cache-status", "age")


def fetch_text(url):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
        return resp.read().decode("utf-8", "replace")


def ms(seconds):
    return round(seconds * 1000, 1)


def cache_status(headers):
    for name in CACHE_HEADERS:
        value = headers.get(name)
        if value:
            return f"{name}: {value}" if name != "cf-cache-status" else value
    return ""


def sample(host, port, path, tls=True):
    """One request on a new connection, then the same one again on that connection.

    Times are seconds from the start of each step. The body is read as sent
    (compressed when the server compresses it); `bytes` is what crossed the wire.
    """
    headers = {"Host": host, "User-Agent": USER_AGENT, "Accept": "*/*", "Accept-Encoding": "gzip"}
    started = time.perf_counter()
    addr = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)[0]
    dns = time.perf_counter() - started
    step = time.perf_counter()
    sock = socket.create_connection(addr[4][:2], timeout=TIMEOUT_S)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    tcp = time.perf_counter() - step
    tls_s = 0.0
    if tls:
        step = time.perf_counter()
        sock = ssl.create_default_context().wrap_socket(sock, server_hostname=host)
        tls_s = time.perf_counter() - step
        conn = http.client.HTTPSConnection(host, port, timeout=TIMEOUT_S)
    else:
        conn = http.client.HTTPConnection(host, port, timeout=TIMEOUT_S)
    conn.sock = sock
    try:
        out = {"dns": dns, "tcp": tcp, "tls": tls_s}
        for label in ("cold", "warm"):
            step = time.perf_counter()
            conn.request("GET", path, headers=headers)
            resp = conn.getresponse()
            wait = time.perf_counter() - step
            body = resp.read()
            total = time.perf_counter() - step
            if label == "cold":
                out.update(
                    status=resp.status,
                    wait=wait,
                    dl=total - wait,
                    bytes=len(body),
                    cache=cache_status({k.lower(): v for k, v in resp.getheaders()}),
                    cache_control=resp.getheader("cache-control") or "",
                    encoding=resp.getheader("content-encoding") or "",
                    body=(gzip.decompress(body) if resp.getheader("content-encoding") == "gzip" else body) if path == "/" else b"",
                )
            else:
                out["warm_wait"] = wait
        return out
    finally:
        conn.close()


def asset_paths(html):
    """The same-origin /assets/ files a page's HTML names."""
    found = re.findall(r'(?:src|href)="(/assets/[^"]+)"', html)
    return list(dict.fromkeys(found))


def search_path(version):
    return "/v1/aggregate?" + urllib.parse.urlencode({**SEARCH_QUERY, "v": version})


def targets(base, home_html):
    """[(name, path)] for the pages, one search and the home page's assets."""
    meta = json.loads(fetch_text(base + "/v1/meta"))
    items = [
        ("home", "/"),
        ("search-page", "/?" + urllib.parse.urlencode(SEARCH_QUERY)),
        ("datasets", "/datasets"),
        ("meta", "/v1/meta"),
        ("search", search_path(meta["index_version"])),
    ]
    items += [(f"asset:{p.rsplit('/', 1)[-1]}", p) for p in asset_paths(home_html)]
    return items


def med(values):
    return statistics.median(values) if values else float("nan")


def p95(values):
    values = sorted(values)
    return values[min(len(values) - 1, int(round(0.95 * (len(values) - 1))))] if values else float("nan")


def run_direct(base, runs):
    parts = urllib.parse.urlsplit(base)
    host = parts.hostname
    tls = parts.scheme == "https"
    port = parts.port or (443 if tls else 80)
    home = sample(host, port, "/", tls)
    html = home["body"].decode("utf-8", "replace")
    items = targets(base, html)
    print(f"{base}  from here over HTTP/1.1, {runs} runs each")
    print(
        f"{'url':<24} {'status':>6} {'bytes':>8} {'dns':>6} {'tcp':>6} {'tls':>6} {'wait':>6} {'ttfb':>6} "
        f"{'dl':>6} {'warm':>6} {'p95 ttfb':>9}  cache"
    )
    results = []
    for name, path in items:
        rows = []
        for _ in range(runs):
            rows.append(sample(host, port, path, tls))
            time.sleep(0.2)
        ttfb = [r["dns"] + r["tcp"] + r["tls"] + r["wait"] for r in rows]
        last = rows[-1]
        statuses = sorted({r["status"] for r in rows})
        print(
            f"{name:<24} {'/'.join(map(str, statuses)):>6} {last['bytes']:>8} "
            f"{ms(med([r['dns'] for r in rows])):>6} {ms(med([r['tcp'] for r in rows])):>6} "
            f"{ms(med([r['tls'] for r in rows])):>6} {ms(med([r['wait'] for r in rows])):>6} "
            f"{ms(med(ttfb)):>6} {ms(med([r['dl'] for r in rows])):>6} "
            f"{ms(med([r['warm_wait'] for r in rows])):>6} {ms(p95(ttfb)):>9}  {last['cache']}"
        )
        results.append(
            {
                "name": name,
                "path": path,
                "status": statuses,
                "bytes": last["bytes"],
                "cache": last["cache"],
                "cache_control": last["cache_control"],
                "encoding": last["encoding"],
                "runs": [{k: ms(v) for k, v in r.items() if k in ("dns", "tcp", "tls", "wait", "dl", "warm_wait")} for r in rows],
            }
        )
    assets = [r for r in results if r["name"].startswith("asset:")]
    if assets:
        # The first visit's critical path: HTML, then every asset at once on its own connection.
        def one(a):
            t = time.perf_counter()
            sample(host, port, a["path"], tls)
            return time.perf_counter() - t

        walls = []
        for _ in range(runs):
            t = time.perf_counter()
            with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
                list(pool.map(one, assets))
            walls.append(time.perf_counter() - t)
        print(
            f"assets together, one new connection each (as a first visit without HTTP/2): "
            f"median {ms(med(walls))} ms, max {ms(max(walls))} ms"
        )
    return {"base_url": base, "mode": "direct", "runs": runs, "results": results}


def globalping(body):
    req = urllib.request.Request(
        GLOBALPING_API,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "User-Agent": USER_AGENT},
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
            started = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        raise SystemExit(f"Globalping refused the measurement: HTTP {e.code} {e.read()[:300].decode('utf-8', 'replace')}")
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        time.sleep(2)
        done = json.loads(fetch_text(f"{GLOBALPING_API}/{started['id']}"))
        if done["status"] != "in-progress":
            return done
    raise SystemExit("Globalping measurement didn't finish in 90 s")


def run_globalping(base):
    parts = urllib.parse.urlsplit(base)
    home_html = fetch_text(base + "/")
    items = targets(base, home_html)
    locations = [loc for _, loc in GLOBALPING_LOCATIONS]
    print(f"{base}  from Globalping's public probes, one request each")
    summary = []
    for name, path in items:
        parsed = urllib.parse.urlsplit(path)
        request = {"method": "GET", "path": parsed.path or "/", "headers": {"Accept-Encoding": "gzip"}}
        if parsed.query:
            request["query"] = parsed.query
        done = globalping(
            {
                "type": "http",
                "target": parts.hostname,
                "locations": locations,
                "measurementOptions": {"request": request, "protocol": parts.scheme.upper()},
            }
        )
        rows = []
        for r in done["results"]:
            res, probe = r["result"], r["probe"]
            t = res.get("timings") or {}
            if res.get("status") != "finished" or not t:
                rows.append({"probe": probe, "error": res.get("rawOutput", "")[:120] or res.get("status")})
                continue
            ttfb = sum(t.get(k) or 0 for k in ("dns", "tcp", "tls", "firstByte"))
            rows.append(
                {
                    "probe": {k: probe.get(k) for k in ("continent", "country", "city", "network")},
                    "status": res.get("statusCode"),
                    "dns": t.get("dns"),
                    "tcp": t.get("tcp"),
                    "tls": t.get("tls"),
                    "wait": t.get("firstByte"),
                    "dl": t.get("download"),
                    "ttfb": ttfb,
                    "total": t.get("total"),
                    "cache": cache_status({k.lower(): v for k, v in (res.get("headers") or {}).items()}),
                }
            )
        print(f"\n{name}  {path[:80]}")
        print(f"  {'probe':<30} {'status':>6} {'dns':>5} {'tcp':>5} {'tls':>5} {'wait':>5} {'ttfb':>5} {'dl':>5} {'total':>6}  cache")
        for row in sorted(rows, key=lambda x: (x["probe"].get("country") or "", x["probe"].get("city") or "")):
            p = row["probe"]
            label = f"{p.get('country')} {p.get('city')} ({(p.get('network') or '')[:14]})"
            if "error" in row:
                print(f"  {label:<30} {row['error']}")
                continue
            print(
                f"  {label:<30} {row['status']:>6} {row['dns']:>5} {row['tcp']:>5} {row['tls']:>5} {row['wait']:>5} "
                f"{row['ttfb']:>5} {row['dl']:>5} {row['total']:>6}  {row['cache']}"
            )
        ok = [r for r in rows if "error" not in r]
        if ok:
            print(
                f"  median over {len(ok)} probes: wait {med([r['wait'] for r in ok])} ms, "
                f"ttfb {med([r['ttfb'] for r in ok])} ms, total {med([r['total'] for r in ok])} ms; "
                f"worst ttfb {max(r['ttfb'] for r in ok)} ms"
            )
        summary.append({"name": name, "path": path, "rows": rows})
    return {"base_url": base, "mode": "globalping", "results": summary}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--base-url", default="https://usnewsmap.com", help="the site to time (default https://usnewsmap.com)")
    ap.add_argument("--runs", type=int, default=5, help="samples per URL when timing from here (default 5)")
    ap.add_argument("--globalping", action="store_true", help="time from Globalping's public probes instead of from here")
    ap.add_argument("--json", metavar="FILE", help="also write the numbers as JSON")
    a = ap.parse_args()
    base = a.base_url.rstrip("/")
    if a.runs < 1:
        ap.error("--runs must be at least 1")
    out = run_globalping(base) if a.globalping else run_direct(base, a.runs)
    out["at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    if a.json:
        with open(a.json, "w") as f:
            json.dump(out, f, indent=2, sort_keys=True)
            f.write("\n")


if __name__ == "__main__":
    sys.exit(main())
