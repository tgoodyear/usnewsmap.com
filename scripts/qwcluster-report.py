#!/usr/bin/env python3
"""Gather the search cluster experiment's results into a Markdown report.

    scripts/qwcluster-report.py <env> [--since 2d] [--out report.md] [--from-file lines.txt]

Reads, for the environment's settings (scripts/settings.sh):

- the `qwcluster report` lines the bench job logs (`usnm-qwcluster load`,
  `bench`, `sample`, `members`), and the ingest job's `index layout` and
  `release progress` lines (the single-writer release of the same pages),
  from Log Analytics (`LOG_ANALYTICS_WORKSPACE_ID`);
- each node app's CPU (UsageNanoCores), memory (WorkingSetBytes) and network
  (RxBytes, TxBytes) per minute over each run, from Azure Monitor;
- Container Apps and Blob list prices for the region, from the Azure Retail
  Prices API (public, no sign-in).

If Log Analytics is over its daily cap (or behind), stream the stored
reports instead and pass the capture with --from-file:

    scripts/start-job.sh <env> SEARCH_CLUSTER_JOB dump --hold-secs 600
    az containerapp job logs show -n <job> -g <rg> --container qwbench --follow > lines.txt

The bench job keeps every report in qw-bench (`runs/`, `sample/`); `dump`
prints them. The job executions' start and end times come from the ARM API
and the metrics from Azure Monitor, neither of which needs the workspace.

Run it before scaling the cluster down: Azure Monitor keeps no metrics a
deleted app can still be asked for. Needs az signed in with read access to
the environment's resource group and workspace.
"""

import argparse
import datetime
import json
import os
import statistics
import subprocess
import sys
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SECONDS_PER_MONTH = 730 * 3600
# Below this a replica is billed at the idle rate (Container Apps billing):
# under 0.01 vCPU and under 1,000 bytes/s of network.
IDLE_CPU_NANOCORES = 0.01e9
IDLE_NETWORK_BYTES_PER_SEC = 1000
# Today's searcher: the API app's Quickwit sidecar (infra/modules/containerapp.bicep).
SIDECAR_VCPU, SIDECAR_GIB = 2.0, 4.0
# The bench job (infra/modules/searchcluster.bicep).
BENCH_VCPU, BENCH_GIB = 4.0, 8.0

QUERY_REPORTS = """
ContainerAppConsoleLogs
| where Log has "qwcluster report"
| extend j = parse_json(Log)
| where tostring(j.fields.message) == "qwcluster report"
| project TimeGenerated, Kind = tostring(j.fields.kind), Report = tostring(j.fields.report)
| order by TimeGenerated asc
"""

QUERY_RELEASE = """
ContainerAppConsoleLogs
| where ContainerName == "ingest"
| where Log !has "IDENTITY_HEADER" and Log !has "MSI_SECRET"
| where Log has_any ("index layout", "release progress", "building index", "released", "merged; the index is closed")
| extend j = parse_json(Log)
| extend Message = tostring(j.fields.message)
| where Message in ("index layout", "release progress", "building index", "released",
    "merged; the index is closed to further writes")
| project TimeGenerated, Message, Fields = tostring(j.fields)
| order by TimeGenerated asc
"""


def settings(env, key):
    out = subprocess.run([os.path.join(ROOT, "scripts/settings.sh"), env, key],
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()


def az_json(args):
    out = subprocess.run(["az", *args, "-o", "json"], check=True, capture_output=True, text=True)
    return json.loads(out.stdout or "null")


def log_query(workspace, query, span):
    body = json.dumps({"query": query, "timespan": span})
    v = az_json(["rest", "--method", "post", "--resource", "https://api.loganalytics.io",
                 "--url", f"https://api.loganalytics.io/v1/workspaces/{workspace}/query",
                 "--headers", "Content-Type=application/json", "--body", body])
    return rows_of(v)


def rows_of(v):
    """A Log Analytics query result as dicts."""
    t = v["tables"][0]
    names = [c["name"] for c in t["columns"]]
    return [dict(zip(names, r)) for r in t["rows"]]


def parse_reports(rows):
    """`qwcluster report` rows as (kind, report dict), oldest first. A row
    whose report doesn't parse (a line cut short) is skipped."""
    out = []
    for r in rows:
        try:
            out.append((r["Kind"], json.loads(r["Report"])))
        except (ValueError, TypeError, KeyError):
            continue
    return out


def reports_from_lines(lines):
    """`qwcluster report` records from captured console output: JSON log
    lines, bare or wrapped in a log stream's record (its `Log`), possibly
    after a timestamp. The last record of each (kind, name) wins, so a
    `dump` after the runs doesn't count them twice."""
    found = {}
    for line in lines:
        start = line.find("{")
        if start < 0:
            continue
        try:
            j = json.loads(line[start:])
        except ValueError:
            continue
        if isinstance(j, dict) and "fields" not in j and isinstance(j.get("Log"), str):
            try:
                j = json.loads(j["Log"][j["Log"].find("{"):])
            except ValueError:
                continue
        fields = j.get("fields", {}) if isinstance(j, dict) else {}
        if fields.get("message") != "qwcluster report":
            continue
        try:
            r = json.loads(fields["report"])
        except (ValueError, TypeError, KeyError):
            continue
        kind = fields.get("kind", "")
        name = r.get("label") or r.get("index_id") or r.get("name") or ""
        found[(kind, name)] = r
    return list(found.items())


def executions(sub, rg, job):
    """The bench job's executions: name, status, start, end (ARM, no workspace)."""
    if not job:
        return []
    try:
        v = az_json(["containerapp", "job", "execution", "list", "-n", job, "-g", rg,
                     "--subscription", sub])
    except subprocess.CalledProcessError:
        return []
    out = []
    for e in v or []:
        props = e.get("properties", {})
        out.append([e.get("name"), props.get("status"), props.get("startTime"), props.get("endTime")])
    return sorted(out, key=lambda r: r[2] or "")


def span_of(since):
    """30m / 6h / 2d → ISO 8601."""
    unit = since[-1]
    n = since[:-1]
    if not n.isdigit() or unit not in "mhd":
        raise SystemExit(f"--since {since}: use 30m, 6h or 2d")
    return {"m": f"PT{n}M", "h": f"PT{n}H", "d": f"P{n}D"}[unit]


def prices(region):
    """Container Apps Consumption and Blob Hot LRS list prices for `region`."""
    def items(flt):
        url = "https://prices.azure.com/api/retail/prices?" + urllib.parse.urlencode({"$filter": flt})
        out = []
        while url:
            with urllib.request.urlopen(url, timeout=60) as resp:
                v = json.load(resp)
            out.extend(v.get("Items", []))
            url = v.get("NextPageLink")
        return out

    aca = items(f"serviceName eq 'Azure Container Apps' and armRegionName eq '{region}' "
                "and priceType eq 'Consumption'")
    blob = items(f"serviceName eq 'Storage' and armRegionName eq '{region}' and skuName eq 'Hot LRS' "
                 "and productName eq 'General Block Blob v2' and priceType eq 'Consumption'")
    return pick_prices(aca, blob)


def pick_prices(aca, blob):
    def meter(items, name):
        hits = [i for i in items if i["meterName"] == name and i.get("tierMinimumUnits", 0) == 0]
        if not hits:
            raise SystemExit(f"no list price for {name}")
        return hits[0]["retailPrice"]

    return {
        "vcpu_active": meter(aca, "Standard vCPU Active Usage"),
        "vcpu_idle": meter(aca, "Standard vCPU Idle Usage"),
        "gib_active": meter(aca, "Standard Memory Active Usage"),
        "gib_idle": meter(aca, "Standard Memory Idle Usage"),
        "blob_gb_month": meter(blob, "Hot LRS Data Stored"),
    }


def replica_cost(p, vcpu, gib, active_secs, idle_secs):
    return (vcpu * (p["vcpu_active"] * active_secs + p["vcpu_idle"] * idle_secs)
            + gib * (p["gib_active"] * active_secs + p["gib_idle"] * idle_secs))


def app_metrics(sub, rg, app, start, end):
    """Per-minute CPU (vCPU), memory (MiB) and network (bytes/s) of one app."""
    rid = f"/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.App/containerApps/{app}"
    try:
        v = az_json(["monitor", "metrics", "list", "--resource", rid,
                     "--metrics", "UsageNanoCores", "WorkingSetBytes", "RxBytes", "TxBytes",
                     "--interval", "PT1M", "--aggregation", "Average", "Maximum", "Total",
                     "--start-time", start, "--end-time", end])
    except subprocess.CalledProcessError:
        return None
    return summarize_metrics(v)


def summarize_metrics(v):
    """Per-minute points of each metric, matched by their timestamps."""
    series = {}
    for m in v.get("value", []):
        name = m["name"]["value"]
        points = {d["timeStamp"]: d for ts in m.get("timeseries", []) for d in ts.get("data", [])
                  if "timeStamp" in d}
        series[name] = points

    def value(name, agg, t):
        return series.get(name, {}).get(t, {}).get(agg)

    minutes = sorted(t for t, d in series.get("UsageNanoCores", {}).items() if d.get("average") is not None)
    cpu = [value("UsageNanoCores", "average", t) / 1e9 for t in minutes]
    cpu_max = [x / 1e9 for t in minutes if (x := value("UsageNanoCores", "maximum", t)) is not None]
    mem = [x / 2**20 for t, d in series.get("WorkingSetBytes", {}).items()
           if (x := d.get("maximum")) is not None]
    net = {}
    for t in minutes:
        rx, tx = value("RxBytes", "total", t), value("TxBytes", "total", t)
        if rx is not None and tx is not None:
            net[t] = (rx + tx) / 60
    # Idle only with both network values in: a gap isn't quiet.
    idle = sum(1 for t, c in zip(minutes, cpu)
               if c < IDLE_CPU_NANOCORES / 1e9 and t in net and net[t] < IDLE_NETWORK_BYTES_PER_SEC)
    return {
        "minutes": len(cpu),
        "cpu_avg": statistics.mean(cpu) if cpu else None,
        "cpu_max": max(cpu_max) if cpu_max else None,
        "mem_max_mib": max(mem) if mem else None,
        "net_avg_bps": statistics.mean(net.values()) if net else None,
        "idle_minutes": idle,
    }


def fmt(x, digits=1):
    if x is None:
        return "-"
    if isinstance(x, float):
        return f"{x:,.{digits}f}"
    return f"{x:,}"


def table(header, rows):
    out = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def load_rows(loads):
    rows = []
    for r in loads:
        s = r["splits"]
        per_node = ", ".join(f"{n} {int(c['docs_indexed']):,}" for n, c in sorted(r["nodes"].items())
                             if c.get("docs_indexed"))
        rows.append([
            r["index_id"], len(r["indexers"]), fmt(r["docs"]), fmt(r["send_secs"], 0),
            fmt(r["docs_per_sec"], 0), fmt(r["commit_secs"], 0), fmt(r["settle_secs"], 0),
            fmt(r["seal_secs"], 0), s["splits"], fmt(s["median_docs"]),
            fmt(s["bytes"] / 1e9, 2), fmt(s["footer_bytes"] / 1e6, 1),
            f"{r['retries_429']}/{r['retries_503']}", per_node,
        ])
    return rows


def bench_rows(benches):
    rows = []
    for b in benches:
        for p in b["passes"]:
            leaf = ", ".join(f"{n} {int(c['leaf_splits'])}" for n, c in sorted(p["nodes"].items()))
            rows.append([
                b["label"], b["searchers"], ",".join(b["indexes"]), p["name"], p["concurrency"],
                fmt(p["median_secs"], 2), fmt(p["p90_secs"], 2), fmt(p["max_secs"], 2),
                fmt(p["throughput"], 3), p["failed"], leaf,
                "yes" if p["all_searchers_used"] else "no",
            ])
    return rows


def app_of(node_id):
    """A node's Container App: qw-1 is ca-usnm-qw-1, the version
    comparison's qws-0 is ca-usnm-qws-0."""
    return f"ca-usnm-{node_id}"


def ok_searches(p):
    return max(len(p.get("searches", [])) - p.get("failed", 0), 0)


def pass_load(b, p, node, c):
    """One node's work in one pass (#251): main runtime busy seconds and GB
    downloaded per search, the runtime's busy share, its busy seconds per
    GB, and the search pool's peaks. None for reports from before these
    counters."""
    if "main_busy_ms" not in c:
        return None
    n = ok_searches(p)
    busy = c["main_busy_ms"] / 1000
    gb = c.get("download_bytes", 0) / 1e9
    threads = c.get("main_threads") or 0
    wall = p.get("wall_secs") or 0
    sampled = p.get("sampled", {}).get(node, {})
    return {
        "version": (b.get("quickwit") or {}).get("version") or "-",
        "busy_per_search": busy / n if n else None,
        "gb_per_search": gb / n if n else None,
        "busy_share": busy / (threads * wall) if threads and wall else None,
        "busy_per_gb": busy / gb if gb else None,
        "ongoing_max": sampled.get("search_ongoing_max"),
        "pending_max": sampled.get("search_pending_max"),
        "pending_share": (sampled["samples_pending"] / sampled["samples"]
                          if sampled.get("samples") else None),
        "threads": threads,
    }


def compare_rows(benches):
    """Per bench, pass and node: the version comparison's measures."""
    rows = []
    for b in benches:
        for p in b["passes"]:
            for node, c in sorted(p["nodes"].items()):
                x = pass_load(b, p, node, c)
                if x is None:
                    continue
                rows.append([
                    b["label"], x["version"], ",".join(b["indexes"]),
                    "yes" if b.get("american_stories") else "no", p["name"], p["concurrency"],
                    fmt(p["median_secs"], 2), fmt(p["p90_secs"], 2), fmt(x["threads"], 0),
                    fmt(x["busy_per_search"], 2), fmt(x["busy_share"] and 100 * x["busy_share"], 0),
                    fmt(x["gb_per_search"], 3), fmt(x["busy_per_gb"], 1),
                    fmt(x["ongoing_max"], 0), fmt(x["pending_max"], 0),
                    fmt(x["pending_share"] and 100 * x["pending_share"], 0),
                ])
    return rows


def compare_summary(benches):
    """Each (index, pass) by Quickwit version, averaged over its runs, with
    ratios to 0.9.x: latency, and main runtime busy seconds per search and
    per GB (the CPU-bound measure that carries to production)."""
    groups = {}
    for b in benches:
        for p in b["passes"]:
            for node, c in p["nodes"].items():
                x = pass_load(b, p, node, c)
                if x is None or x["version"] == "-":
                    continue
                key = (",".join(b["indexes"]), p["name"])
                g = groups.setdefault(key, {}).setdefault(x["version"], [])
                g.append((p["median_secs"], x["busy_per_search"], x["busy_per_gb"]))

    def mean(vals):
        vals = [v for v in vals if v is not None]
        return statistics.mean(vals) if vals else None

    def ratio(a, b):
        return a / b if a is not None and b else None

    rows = []
    for (index, name), by_version in sorted(groups.items()):
        means = {v: [mean(col) for col in zip(*runs)] for v, runs in by_version.items()}
        base = next((means[v] for v in sorted(means) if v.startswith("0.9")), None)
        for v, (median, busy, per_gb) in sorted(means.items()):
            rows.append([
                index, name, v, len(by_version[v]), fmt(median, 2),
                fmt(ratio(median, base[0]) if base else None, 2), fmt(busy, 2),
                fmt(ratio(busy, base[1]) if base else None, 2), fmt(per_gb, 1),
                fmt(ratio(per_gb, base[2]) if base else None, 2),
            ])
    return rows


def hit_mismatches(benches):
    """Searches whose page counts differ between Quickwit versions on the
    same index and date windows (the comparison's correctness gate, #251):
    (index, pass, search, {version: pages})."""
    seen = {}
    for b in benches:
        version = (b.get("quickwit") or {}).get("version")
        if not version:
            continue
        for p in b["passes"]:
            for r in p.get("searches", []):
                if r.get("error") is None and r.get("pages") is not None:
                    key = (",".join(b["indexes"]), bool(b.get("american_stories")),
                           p.get("shift_days"), r["name"])
                    seen.setdefault(key, {}).setdefault(version, set()).add(r["pages"])
    rows = []
    for (index, _, shift, name), by_version in sorted(seen.items(), key=lambda kv: str(kv[0])):
        if len(by_version) > 1 and len({n for v in by_version.values() for n in v}) > 1:
            rows.append([index, shift, name,
                         "; ".join(f"{v}: {sorted(n)}" for v, n in sorted(by_version.items()))])
    return rows


def window(report, start_key, end_key):
    return report[start_key], report[end_key]


def secs_between(a, b):
    def t(s):
        return datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))
    return (t(b) - t(a)).total_seconds()


def steady_state(p, nodes, vcpu, idle_share=1.0):
    """Monthly cost of `nodes` replicas of `vcpu` (2 GiB each), `idle_share` idle."""
    active = SECONDS_PER_MONTH * (1 - idle_share)
    idle = SECONDS_PER_MONTH * idle_share
    return nodes * replica_cost(p, vcpu, 2 * vcpu, active, idle)


def report(env, since, out, from_file=None):
    workspace = settings(env, "LOG_ANALYTICS_WORKSPACE_ID")
    sub = settings(env, "AZURE_SUBSCRIPTION_ID")
    rg = settings(env, "AZURE_RESOURCE_GROUP")
    region = settings(env, "AZURE_LOCATION") or "eastus2"
    vcpu = float(settings(env, "USNM_SEARCH_CLUSTER_NODE_VCPU") or 2)
    span = span_of(since)
    if from_file:
        with open(from_file) as f:
            reports = [(k, r) for (k, _), r in reports_from_lines(f)]
        release = []
    else:
        try:
            reports = parse_reports(log_query(workspace, QUERY_REPORTS, span))
            release = log_query(workspace, QUERY_RELEASE, span)
        except subprocess.CalledProcessError as e:
            print(f"warning: Log Analytics query failed ({e.stderr.strip()[:200]}); "
                  "use --from-file with a dump", file=sys.stderr)
            reports, release = [], []
    p = prices(region)
    loads = [r for k, r in reports if k == "load"]
    benches = [r for k, r in reports if k == "bench"]
    samples = [r for k, r in reports if k == "sample"]
    md = [f"# Search cluster experiment: {env}", "",
          f"Generated {datetime.datetime.now(datetime.timezone.utc):%Y-%m-%d %H:%M} UTC from Log Analytics "
          f"({since}), Azure Monitor and the Retail Prices API ({region}).", ""]
    runs = executions(sub, rg, settings(env, "SEARCH_CLUSTER_JOB"))
    if runs:
        md += ["## Bench job executions (ARM)", "",
               table(["execution", "status", "start", "end"], runs), ""]
    if samples:
        s = samples[-1]
        md += ["## Sample", "", table(
            ["name", "version", "pct", "batches", "pages read", "docs", "with AS", "MB (raw)", "secs"],
            [[s.get("name", "-"), s["version"], s["pct"], s["batches"], fmt(s["pages_read"]), fmt(s["docs"]),
              fmt(s["docs_with_as"]), fmt(s["bytes"] / 1e6, 0), fmt(s["secs"], 0)]]), ""]
    if release:
        md += ["## Single-writer release of the same pages (ingest job)", ""]
        layout = [json.loads(r["Fields"]) for r in release if r["Message"] == "index layout"]
        progress = [json.loads(r["Fields"]) for r in release if r["Message"] == "release progress"]
        rates = [f.get("docs_per_sec") for f in progress if f.get("docs_per_sec")]
        started = next((r["TimeGenerated"] for r in release if r["Message"] == "building index"), None)
        merged = next((r["TimeGenerated"] for r in release
                       if r["Message"] == "merged; the index is closed to further writes"), None)
        md += [table(["index", "splits", "docs", "MB", "footer MB", "docs/s (median of progress)",
                      "build to sealed (s)"],
                     [[l.get("index"), l.get("splits"), fmt(l.get("docs")), fmt(l.get("mb")),
                       fmt((l.get("footer_bytes") or 0) / 1e6), fmt(statistics.median(rates), 0) if rates else "-",
                       fmt(secs_between(started, merged), 0) if started and merged else "-"]
                      for l in layout if not str(l.get("index", "")).startswith("pages-ja")]), ""]
    if loads:
        md += ["## Indexing (usnm-qwcluster load)", "", table(
            ["index", "indexers", "docs", "send s", "docs/s", "committed s", "settled s", "sealed s",
             "splits", "median docs/split", "GB", "footers MB", "retries 429/503", "docs by node"],
            load_rows(loads)), ""]
        rows = []
        for r in loads:
            secs = secs_between(r["started_at"], r["sealed_at"])
            nodes = [m["node_id"] for m in r["members"]]
            for n in nodes:
                app = app_of(n)
                m = app_metrics(sub, rg, app, r["started_at"], r["sealed_at"]) or {}
                rows.append([r["index_id"], app, fmt(m.get("cpu_avg"), 2), fmt(m.get("cpu_max"), 2),
                             fmt(m.get("mem_max_mib"), 0), fmt(secs, 0)])
            cost = (len(nodes) * replica_cost(p, vcpu, 2 * vcpu, secs, 0)
                    + replica_cost(p, BENCH_VCPU, BENCH_GIB, secs, 0))
            rows.append([r["index_id"], "run cost (nodes + bench job, all active)", "", "", "", f"${cost:.3f}"])
        md += ["### CPU and memory per node during each load", "",
               table(["index", "app", "vCPU avg", "vCPU max", "memory max MiB", "secs / cost"], rows), ""]
    if benches:
        md += ["## Searching (usnm-qwcluster bench)", "",
               "Each pass runs the 13 benchmark searches (scripts/bench-cold-searches.py and "
               "scripts/load-cold-searches.py) as the API's /v1/aggregate does, against node 0. "
               "Leaf splits: splits each node searched in the pass.", "",
               table(["label", "searchers", "index", "pass", "concurrency", "median s", "p90 s", "max s",
                      "searches/s", "failed", "leaf splits by node", "all searchers used"],
                     bench_rows(benches)), ""]
        rows = []
        for b in benches:
            secs = secs_between(b["started_at"], b["ended_at"])
            for m in b["members"]:
                app = app_of(m["node_id"])
                x = app_metrics(sub, rg, app, b["started_at"], b["ended_at"]) or {}
                rows.append([b["label"], app, fmt(x.get("cpu_avg"), 2), fmt(x.get("cpu_max"), 2),
                             fmt(x.get("mem_max_mib"), 0), fmt(secs, 0)])
        md += ["### CPU and memory per node during each bench", "",
               table(["label", "app", "vCPU avg", "vCPU max", "memory max MiB", "secs"], rows), ""]
        crows = compare_rows(benches)
        if crows:
            md += ["## Quickwit version comparison (#251)", "",
                   "Per pass and node, from its metrics before and after the pass: the main Tokio "
                   "runtime's busy seconds per search (downloads, TLS, split opening), its busy share "
                   "(busy time over threads × wall time), GB downloaded from Blob per search, busy "
                   "seconds per GB, and the search pool's peaks sampled every 2 s. On dev's I/O-bound "
                   "1% the latencies don't carry to production; busy seconds per GB do, since "
                   "production's cold searches wait on that runtime.", "",
                   table(["label", "quickwit", "index", "AS", "pass", "conc.", "median s", "p90 s",
                          "main threads", "main busy s/search", "main busy %", "GB/search",
                          "busy s/GB", "pool running max", "pool queued max", "% samples queued"],
                         crows), "",
                   "By version, averaged over runs (ratios to 0.9.x; under 1 means less):", "",
                   table(["index", "pass", "quickwit", "runs", "median s", "× 0.9", "main busy s/search",
                          "× 0.9", "busy s/GB", "× 0.9"], compare_summary(benches)), ""]
            bad = hit_mismatches(benches)
            md += ["Pages found, by version, for the same searches and windows: "
                   + ("**they differ**, so the versions don't search the same way and the timings "
                      "above don't compare:" if bad else "the same in every search."), ""]
            if bad:
                md += [table(["index", "window shift (days)", "search", "pages by version"], bad), ""]
    # Idle: the last day's minutes at the idle rate, per node app still there.
    now = datetime.datetime.now(datetime.timezone.utc)
    day_ago = (now - datetime.timedelta(days=1)).strftime("%Y-%m-%dT%H:%M:%SZ")
    idle_rows = []
    for app in [f"ca-usnm-qw-{i}" for i in range(4)] + [f"ca-usnm-qws-{i}" for i in range(4)]:
        m = app_metrics(sub, rg, app, day_ago, now.strftime("%Y-%m-%dT%H:%M:%SZ"))
        if m and m["minutes"]:
            idle_rows.append([app, m["minutes"], m["idle_minutes"], fmt(m["cpu_avg"], 3),
                              fmt(m["net_avg_bps"], 0)])
    md += ["## Cost", "", "List prices: vCPU ${vcpu_active}/s active, ${vcpu_idle}/s idle; memory "
           "${gib_active}/GiB-s active, ${gib_idle}/GiB-s idle; Blob Hot LRS ${blob_gb_month}/GB-month.".format(**p), ""]
    if idle_rows:
        md += ["Idle check, last 24 h (a minute is idle under 0.01 vCPU and 1,000 bytes/s):", "",
               table(["app", "minutes", "idle minutes", "vCPU avg", "network bytes/s avg"], idle_rows), ""]
    sidecar = replica_cost(p, SIDECAR_VCPU, SIDECAR_GIB, 0, SECONDS_PER_MONTH)
    rows = [["today: API sidecar (2 vCPU / 4 GiB)", 1, f"${sidecar:.2f}",
             f"${replica_cost(p, SIDECAR_VCPU, SIDECAR_GIB, 0.1 * SECONDS_PER_MONTH, 0.9 * SECONDS_PER_MONTH):.2f}"]]
    for n in (1, 2, 3):
        rows.append([f"cluster: {n} node(s) of {vcpu:g} vCPU / {2 * vcpu:g} GiB", n,
                     f"${steady_state(p, n, vcpu):.2f}", f"${steady_state(p, n, vcpu, 0.9):.2f}"])
    for n in (2, 3):
        extra = n - 1
        rows.append([f"sidecar as root + {extra} node(s) (+ over today)", n,
                     f"+${steady_state(p, extra, vcpu):.2f}", f"+${steady_state(p, extra, vcpu, 0.9):.2f}"])
    md += ["Steady state per month (730 h):", "",
           table(["setup", "replicas", "all idle", "10% active"], rows), ""]
    if loads:
        gb = loads[0]["splits"]["bytes"] / 1e9
        name = loads[0]["sample"].rsplit("/", 1)[-1]
        sample = next((x for x in samples if x.get("name") == name), None)
        # The sample's share of its version; dev's version is itself the 1%.
        pct = sample["pct"] if sample else 1.0
        scale = 100 / pct if pct else 100
        md += [f"Storage: the sample index is {gb:.2f} GB (${gb * p['blob_gb_month']:.2f}/month); "
               f"scaled to 100% about {gb * scale:,.0f} GB (${gb * scale * p['blob_gb_month']:,.2f}/month) "
               "for a second index root. A cluster serving `qw-index` needs no extra storage.", ""]
    text = "\n".join(md)
    if out:
        with open(out, "w") as f:
            f.write(text)
    print(text)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("env")
    ap.add_argument("--since", default="2d", help="how far back to read the logs (30m, 6h, 2d)")
    ap.add_argument("--out", help="also write the report here")
    ap.add_argument("--from-file", help="read the reports from a captured log stream of `dump`")
    args = ap.parse_args()
    report(args.env, args.since, args.out, args.from_file)


if __name__ == "__main__":
    sys.exit(main())
