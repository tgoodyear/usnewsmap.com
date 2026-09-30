#!/usr/bin/env python3
"""One-time import of past searches into the search log (ADR-0012).

Before the search log existed, the Quickwit sidecar logged every search
request it received at info, and those lines are in Log Analytics
(ContainerAppConsoleLogs, 30-day retention, no client address). This reads
them, keeps one line per visitor search, and writes one file per UTC day to
searches/import/{day}.jsonl, in the search log's record format with
"source": "import", in random order.

    scripts/import-search-log.py prod --dry-run   # counts only; writes nothing
    scripts/import-search-log.py prod --out DIR   # write the day files to DIR
    scripts/import-search-log.py prod             # upload them (not over existing files)

Not run by CI. Queries are read-only (az rest against the Log Analytics query
API). The upload uses az storage blob upload --auth-mode login, so it needs
write access to the searches container (Storage Blob Data Contributor on the
container, for the upload only) and a network path to the data account's
private endpoint. Search text is never printed, only counts.

What counts as one search, from the log:
- The API asks Quickwit for one summary (a request with the "series"
  aggregation) per /v1/aggregate response it computes. Cube shards and hits
  pages are other requests, and searches the API answered from its caches
  never reached Quickwit, so a search repeated while cached is counted once.
- A summary repeated for the same query on the same replica within 10 s is
  one search (the API coarsens an oversized cube by asking again).
- The warm-up's example searches (web/src/examples.json) are dropped: a
  summary for an example's query on a replica that was warming up (from each
  "warm-up finished" line and its duration, or 6 minutes after "starting
  usnm-api" when no such line follows).

What an imported record can't have: the words as typed (q is the canonical
query), the match mode (folded into the query), the page count (Quickwit
didn't log it) and the index version.
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys
import tempfile
from collections import defaultdict
from datetime import date, datetime, timedelta, timezone
from urllib.parse import parse_qs

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EPOCH = date(1700, 1, 1)
DEDUP = timedelta(seconds=10)
WARM_UP_FALLBACK = timedelta(minutes=6)
SLACK = timedelta(seconds=5)
NOT_SECRETS = 'Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"'

SEARCHES = f"""ContainerAppConsoleLogs
| where ContainerName == "quickwit"
| where {NOT_SECRETS}
| where Log has "rest_handler" and Log contains "SearchRequestQueryString" and Log contains '"series"'
| project TimeGenerated, ContainerGroupName, Log"""

WARM_UPS = f"""ContainerAppConsoleLogs
| where ContainerName == "api"
| where {NOT_SECRETS}
| where Log has "warm-up finished" or Log has "starting usnm-api"
| project TimeGenerated, ContainerGroupName, Log"""

ANSI = re.compile(r"\x1b\[[0-9;]*m")
QUERY = re.compile(r'SearchRequestQueryString \{ query: "((?:[^"\\]|\\.)*)", aggs: ')
SERIES = re.compile(r'"series": Object \{"histogram": Object \{"field": String\("(\w+)"\), "interval": Number\((\d+)\)')
MAX_HITS = re.compile(r"max_hits: (\d+)")
DAY_RANGE = re.compile(r"^day:\[(\d+) TO (\d+)\]$")
SET = re.compile(r"^(state|lccn|language):IN \[([^\]]*)\]$")


def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def setting(env, key):
    out = subprocess.run(
        [os.path.join(ROOT, "scripts", "settings.sh"), env, key],
        capture_output=True, text=True, check=False,
    )
    return out.stdout.strip() if out.returncode == 0 else ""


def query(workspace, kql, days):
    body = json.dumps({"query": kql, "timespan": f"P{days}D"})
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        f.write(body)
    try:
        out = subprocess.run(
            ["az", "rest", "--method", "post", "--resource", "https://api.loganalytics.io",
             "--url", f"https://api.loganalytics.io/v1/workspaces/{workspace}/query",
             "--headers", "Content-Type=application/json", "--body", f"@{f.name}", "-o", "json"],
            capture_output=True, text=True, check=False,
        )
    finally:
        os.unlink(f.name)
    if out.returncode != 0:
        die("the Log Analytics query failed (is az signed in to the environment's tenant?)")
    table = json.loads(out.stdout)["tables"][0]
    names = [c["name"] for c in table["columns"]]
    return [dict(zip(names, row)) for row in table["rows"]]


def when(s):
    # Log Analytics gives up to 7 fractional digits; keep 6.
    s = re.sub(r"(\.\d{6})\d*", r"\1", s.replace("Z", "+00:00"))
    return datetime.fromisoformat(s)


def unescape(s):
    return s.replace('\\"', '"').replace("\\\\", "\\")


def split_top(s, sep):
    """Split on `sep` outside parentheses and quotes."""
    parts, depth, quoted, start, i = [], 0, False, 0, 0
    while i < len(s):
        c = s[i]
        if c == '"':
            quoted = not quoted
        elif not quoted and c == "(":
            depth += 1
        elif not quoted and c == ")":
            depth -= 1
        elif not quoted and depth == 0 and s.startswith(sep, i):
            parts.append(s[start:i])
            i += len(sep)
            start = i
            continue
        i += 1
    parts.append(s[start:])
    return parts


def canonical(qw):
    """The API's canonical query text for Quickwit's rendering of it
    (usnm_search::quickwit::query_string): drop the field names, NOT becomes
    a leading -, and the top level loses its parentheses."""
    q = qw.replace("text:", "")
    q = re.sub(r"NOT (?=[\"(\w])", "-", q)
    if wraps(q):
        q = q[1:-1]
    return q


def wraps(q):
    """Whether one pair of parentheses encloses all of `q`."""
    if not (q.startswith("(") and q.endswith(")")):
        return False
    depth, quoted = 0, False
    for i, c in enumerate(q):
        if c == '"':
            quoted = not quoted
        elif not quoted and c == "(":
            depth += 1
        elif not quoted and c == ")":
            depth -= 1
            if depth == 0 and i < len(q) - 1:
                return False
    return True


def parse_search(log):
    """(quickwit query string, record fields) for a summary line, or None."""
    log = ANSI.sub("", log)
    m, series, hits = QUERY.search(log), SERIES.search(log), MAX_HITS.search(log)
    if not m or not series or not hits or hits.group(1) != "0":
        return None
    full = unescape(m.group(1))
    parts = split_top(full, " AND ")
    # The query's own clauses come first; the filters follow from day:[…].
    at = next((i for i, p in enumerate(parts) if DAY_RANGE.match(p)), None)
    if at is None or at == 0:
        return None
    text = " AND ".join(parts[:at])
    rec = {"state": [], "lccn": [], "lang": [], "front": False}
    lo, hi = DAY_RANGE.match(parts[at]).groups()
    rec["from"] = (EPOCH + timedelta(days=int(lo))).isoformat()
    rec["to"] = (EPOCH + timedelta(days=int(hi))).isoformat()
    for p in parts[at + 1:]:
        s = SET.match(p)
        if s:
            key = {"state": "state", "lccn": "lccn", "language": "lang"}[s.group(1)]
            rec[key] = sorted(v for v in s.group(2).split() if v)
        elif p == "front_page:true":
            rec["front"] = True
        else:
            return None
    field, interval = series.group(1), int(series.group(2))
    bucket = {"year": "year", "ym": "month"}.get(field) or ("week" if interval == 7 else "day")
    rec["query"] = canonical(text)
    rec["bucket"] = bucket
    return full, rec


def example_queries():
    with open(os.path.join(ROOT, "web", "src", "examples.json"), encoding="utf-8") as f:
        examples = json.load(f)
    return {parse_qs(e["aggregate"])["q"][0].strip().lower() for e in examples}


def warm_up_windows(rows):
    """Per replica, the (start, end) spans when a warm-up ran."""
    starts, finishes = defaultdict(list), defaultdict(list)
    for r in rows:
        try:
            line = json.loads(r["Log"])
        except (ValueError, TypeError):
            continue
        fields = line.get("fields", {})
        t = when(r["TimeGenerated"])
        replica = r["ContainerGroupName"]
        if fields.get("message") == "warm-up finished":
            ms = int(fields.get("ms", 0))
            finishes[replica].append((t - timedelta(milliseconds=ms) - SLACK, t + SLACK))
        elif fields.get("message") == "starting usnm-api":
            starts[replica].append(t)
    windows = defaultdict(list)
    for replica, spans in finishes.items():
        windows[replica].extend(spans)
    for replica, ts in starts.items():
        for t in ts:
            if not any(t <= end <= t + WARM_UP_FALLBACK for _, end in finishes[replica]):
                windows[replica].append((t, t + WARM_UP_FALLBACK))
    return windows


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("env")
    ap.add_argument("--days", type=int, default=30, help="how far back (default 30, the log's retention)")
    ap.add_argument("--workspace", help="Log Analytics workspace id (default: the LOG_ANALYTICS_WORKSPACE_ID setting)")
    ap.add_argument("--account", help="data storage account (default: the STORAGE_ACCOUNT setting)")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--dry-run", action="store_true", help="print counts only")
    mode.add_argument("--out", help="write the day files to this directory instead of uploading")
    args = ap.parse_args()
    if not re.fullmatch(r"[a-z][a-z0-9]{0,15}", args.env):
        die("the environment name must be 1-16 lowercase letters and digits")
    if not 1 <= args.days <= 30:
        die("--days must be 1-30")
    workspace = args.workspace or setting(args.env, "LOG_ANALYTICS_WORKSPACE_ID")
    if not re.fullmatch(r"[0-9a-f-]{36}", workspace or ""):
        die("no workspace id; pass --workspace or run scripts/provision.sh to save the stack outputs")

    examples = example_queries()
    windows = warm_up_windows(query(workspace, WARM_UPS, args.days))
    rows = sorted(query(workspace, SEARCHES, args.days), key=lambda r: r["TimeGenerated"])

    unparsed = repeated = warm_up = example_kept = 0
    last = {}
    records = []
    for r in rows:
        parsed = parse_search(r["Log"])
        if parsed is None:
            unparsed += 1
            continue
        full, rec = parsed
        t = when(r["TimeGenerated"])
        replica = r["ContainerGroupName"]
        seen = last.get((replica, full))
        last[(replica, full)] = t
        if seen is not None and t - seen <= DEDUP:
            repeated += 1
            continue
        if rec["query"] in examples and any(a <= t <= b for a, b in windows[replica]):
            warm_up += 1
            continue
        example_kept += rec["query"] in examples
        records.append({
            "v": 1, "day": t.astimezone(timezone.utc).date().isoformat(), "source": "import",
            "q": rec["query"], "query": rec["query"], "mode": None, "near": None, "fuzzy": 0,
            "from": rec["from"], "to": rec["to"], "state": rec["state"], "lccn": rec["lccn"],
            "lang": rec["lang"], "front": rec["front"], "bucket": rec["bucket"], "pages": None,
            "index_version": None,
        })

    by_day = defaultdict(list)
    for rec in records:
        by_day[rec["day"]].append(rec)
    print(f"summary requests in the log: {len(rows)}", file=sys.stderr)
    print(f"  not a search's summary (skipped): {unparsed}", file=sys.stderr)
    print(f"  repeats within {DEDUP.seconds} s (one search): {repeated}", file=sys.stderr)
    print(f"  warm-up example queries (dropped): {warm_up}", file=sys.stderr)
    print(f"searches to import: {len(records)} over {len(by_day)} days", file=sys.stderr)
    print(f"  of which example queries outside a warm-up: {example_kept}", file=sys.stderr)
    for day in sorted(by_day):
        print(f"  {day}: {len(by_day[day])}", file=sys.stderr)
    if args.dry_run or not records:
        return

    out = args.out or tempfile.mkdtemp(prefix="usnm-import-")
    os.makedirs(out, exist_ok=True)
    paths = {}
    for day, recs in by_day.items():
        random.SystemRandom().shuffle(recs)
        paths[day] = os.path.join(out, f"{day}.jsonl")
        with open(paths[day], "w", encoding="utf-8") as f:
            for rec in recs:
                f.write(json.dumps(rec, ensure_ascii=False, separators=(",", ":")) + "\n")
    if args.out:
        print(f"wrote {len(paths)} files to {out}; upload them to searches/import/", file=sys.stderr)
        return
    account = args.account or setting(args.env, "STORAGE_ACCOUNT")
    if not re.fullmatch(r"[a-z0-9]{3,24}", account or ""):
        die("no storage account; pass --account or run scripts/provision.sh to save the stack outputs")
    failed = []
    for day in sorted(paths):
        done = subprocess.run(
            ["az", "storage", "blob", "upload", "--auth-mode", "login", "--account-name", account,
             "--container-name", "searches", "--name", f"import/{day}.jsonl", "--file", paths[day],
             "--content-type", "application/x-ndjson", "--overwrite", "false", "--no-progress", "-o", "none"],
            capture_output=True, text=True, check=False,
        )
        if done.returncode == 0:
            print(f"import/{day}.jsonl: uploaded", file=sys.stderr)
        elif "BlobAlreadyExists" in done.stderr:
            # A rerun: the day was imported before and is left as it is.
            print(f"import/{day}.jsonl: exists already, left alone", file=sys.stderr)
        else:
            failed.append(day)
            print(f"import/{day}.jsonl: upload failed", file=sys.stderr)
    if failed:
        # Keep the files so the operator can see what didn't go.
        die(f"{len(failed)} of {len(paths)} uploads failed (write access to the container? a network path to "
            f"the private endpoint?); the day files are in {out}")
    for p in paths.values():
        os.unlink(p)
    os.rmdir(out)


if __name__ == "__main__":
    main()
