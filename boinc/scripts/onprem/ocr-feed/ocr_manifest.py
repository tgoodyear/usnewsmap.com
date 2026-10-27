#!/usr/bin/env python3
"""Prefetch manifests for OCR's queued pages (#1055, #962), for the scan fetcher's queue.

User OCR requests (the `ocr-priority` queue) wait for scans nobody stages. This finds the
pages queued for OCR that have no outcome yet and no scan on pve-big's scan server, users'
first, and PUTs them through the scan server as /manifests/queue-ocr-<stamp>/manifest.parquet,
which the fetcher (usnm-loc-fetch, OCR's manifests before the quality pass's) fetches inside
the host's global LoC budget. It runs in the batch container CT215 (usnm-ocr-manifest.timer).

  ocr_manifest.py build --state DIR [--days 14] [--max-pages 1000] [--reasons user,bulk]
      [--curated-url https://stusnmdffapzy.blob.core.windows.net/curated]
      [--curated-copy /srv/curated] [--scan-server http://usnm-scanfetch.tjg.fyi:8380]
      [--what-if]

What OCR queued, and what it finished, are the lake's own records: curated
`ocr-local/queued/<day>/*.parquet` (doc_id, batch, reason, language, queued_at) and
`ocr-local/outcomes/<day>/*.parquet` (doc_id, outcome). They are read live, with this
container's Arc identity (the on-prem group's curated read; a few hundred KB a day), and kept
in --state so each file is read once. The damage score and language come from the on-prem
curated copy. A new manifest is made only when the last one's pages are mostly on the scan
server (90%) or it is a day old, so the fetcher's queue never piles up.
"""
import argparse
import datetime as dt
import glob
import io
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

DOC_RE = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
FORMAT = "usnm-prefetch-manifest/1"
USER_AGENT = "usnewsmap-ocr-manifest/1 (+https://usnewsmap.com)"
# A page goes into at most this many manifests a week without its scan arriving.
MAX_TRIES = 3


def log(msg, **kv):
    print(json.dumps({"msg": msg, **kv}, sort_keys=True), flush=True)


def arc_token(resource="https://storage.azure.com/"):
    """A token for this machine's Arc identity (the challenge flow: a 401 names a file only
    root can read, whose contents prove the caller is local)."""
    url = ("http://localhost:40342/metadata/identity/oauth2/token?api-version=2020-06-01&resource="
           + urllib.parse.quote(resource, safe=""))
    try:
        urllib.request.urlopen(urllib.request.Request(url, headers={"Metadata": "true"}), timeout=15)
        raise RuntimeError("the Arc agent answered without a challenge")
    except urllib.error.HTTPError as e:
        chal = e.headers.get("WWW-Authenticate", "")
        if "realm=" not in chal:
            raise
        path = chal.split("realm=", 1)[1].strip()
        if not path.startswith("/var/opt/azcmagent/tokens/"):
            raise RuntimeError(f"unexpected challenge path {path}")
        with open(path) as f:
            secret = f.read().strip()
    req = urllib.request.Request(url, headers={"Metadata": "true", "Authorization": f"Basic {secret}"})
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.loads(r.read())["access_token"]


class Lake:
    def __init__(self, base):
        self.base = base.rstrip("/")
        self.token = None

    def _req(self, url):
        if self.token is None:
            self.token = arc_token()
        return urllib.request.Request(url, headers={
            "Authorization": "Bearer " + self.token, "x-ms-version": "2021-08-06", "User-Agent": USER_AGENT})

    def _open(self, url, timeout):
        """urlopen with three tries: the lake's TLS handshake from home times out now and then."""
        import time

        for attempt in range(3):
            try:
                return urllib.request.urlopen(self._req(url), timeout=timeout)
            except (urllib.error.URLError, TimeoutError) as e:
                if isinstance(e, urllib.error.HTTPError) and e.code < 500:
                    raise
                if attempt == 2:
                    raise
                time.sleep(10 * (attempt + 1))

    def list(self, prefix):
        names, marker = [], ""
        while True:
            q = f"{self.base}?restype=container&comp=list&prefix={urllib.parse.quote(prefix)}"
            if marker:
                q += f"&marker={urllib.parse.quote(marker)}"
            with self._open(q, 60) as r:
                root = ET.fromstring(r.read())
            names += [n.text for n in root.iter("Name")]
            marker = (root.findtext("NextMarker") or "").strip()
            if not marker:
                return names

    def get(self, name):
        with self._open(f"{self.base}/{urllib.parse.quote(name)}", 120) as r:
            return r.read()


def cached_rows(lake, state, kind, days, columns):
    """Every row of ocr-local/<kind>/<day>/*.parquet for the last `days` days, each file
    read from the lake once and kept under <state>/<kind>/."""
    import pyarrow.parquet as pq

    keep = os.path.join(state, kind)
    os.makedirs(keep, exist_ok=True)
    today = dt.datetime.now(dt.timezone.utc).date()
    rows = []
    for back in range(days):
        day = (today - dt.timedelta(days=back)).isoformat()
        for name in lake.list(f"ocr-local/{kind}/{day}/"):
            if not name.endswith(".parquet"):
                continue
            local = os.path.join(keep, day + "-" + os.path.basename(name))
            if not os.path.exists(local):
                body = lake.get(name)
                with open(local + ".tmp", "wb") as f:
                    f.write(body)
                os.replace(local + ".tmp", local)
    for local in sorted(glob.glob(os.path.join(keep, "*.parquet"))):
        rows += pq.read_table(local, columns=columns).to_pylist()
    return rows


def staged_pairs(scan_server):
    """(batch, doc_id) of every full-size scan the fetcher has written, with the index line:
    the scan server's index, read as a stream. The index is fetch history: a buffer scan
    can have been read and deleted since, so [`available`] checks the ones that matter."""
    out = {}
    req = urllib.request.Request(f"{scan_server.rstrip('/')}/index/full.jsonl", headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=300) as r:
        for line in io.TextIOWrapper(r, encoding="utf-8"):
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get("resolution", "full") == "full" and row.get("doc_id") and row.get("batch"):
                out[(row["batch"], row["doc_id"])] = row
    return out


def available(scan_server, row):
    """Whether the scan an index line names is still on the scan server: its sidecar
    (`<scan>.json`, a few hundred bytes) is there, keep set then buffer set, since the line
    doesn't say which. A read buffer scan's sidecar is renamed `.done` before the scan goes,
    so a missing sidecar means the scan is gone or going. The scan itself is never read."""
    m = DOC_RE.match(row["doc_id"])
    if not m:
        return False
    ext = "jp2" if row.get("format") == "jp2" else "jpg"
    rel = f"{row['batch']}/{m['lccn']}/{m['date']}/{int(m['ed'])}/{int(m['seq'])}.{ext}.json"
    for s in ("keep", "buffer"):
        req = urllib.request.Request(f"{scan_server.rstrip('/')}/scans/{s}/{rel}", headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                if r.status == 200:
                    r.read(1 << 16)
                    return True
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise
    return False


def damage_of(copy, batch, bv, docs):
    import pyarrow.parquet as pq

    path = os.path.join(copy, "audit/damage/v2", f"{batch}_ver{int(bv):02d}.parquet")
    if not os.path.exists(path):
        return {}
    t = pq.read_table(path, columns=["doc_id", "lang", "decision", "damage_rate"]).to_pylist()
    return {x["doc_id"]: x for x in t if x["doc_id"] in docs}


def pending(queued, outcomes, reasons):
    """The newest queue record of each page still outstanding, in priority order. As
    usnm-ocr-local's Ledger::outstanding: a page is outstanding with no outcome, with an
    outcome older than its newest queue record, or when that outcome is a dead letter for
    `unroutable` (it goes round again); any other later outcome (imported, not_sent, a
    terminal dead letter) ends it."""
    latest = {}
    for o in outcomes:
        if not o.get("at"):
            continue
        cur = latest.get(o["doc_id"])
        if cur is None or o["at"] >= cur["at"]:
            latest[o["doc_id"]] = o
    last = {}
    for q in queued:
        if q.get("reason") not in reasons or not DOC_RE.match(q.get("doc_id") or ""):
            continue
        cur = last.get(q["doc_id"])
        if cur is None or q["queued_at"] > cur["queued_at"]:
            last[q["doc_id"]] = q

    def outstanding(q):
        o = latest.get(q["doc_id"])
        return (o is None or o["at"] < q["queued_at"]
                or (o.get("outcome") == "dead_lettered" and o.get("cause") == "unroutable"))

    out = [q for q in last.values() if outstanding(q)]
    rank = {r: i for i, r in enumerate(reasons)}
    out.sort(key=lambda q: (rank[q["reason"]], q["queued_at"], q["doc_id"]))
    return out


def manifest_bytes(rows):
    """The fetcher's manifest (usnm_ocr_contract::prefetch): its columns, types, issue order
    and the usnm.format key."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    rows = sorted(rows, key=lambda r: (r["batch"], r["lccn"], r["date"], r["edition"], r["seq"], r["doc_id"]))
    schema = pa.schema([
        ("doc_id", pa.string()), ("batch", pa.string()), ("batch_version", pa.int16()),
        ("lccn", pa.string()), ("date", pa.date32()), ("edition", pa.int16()), ("seq", pa.int16()),
        ("lang", pa.string()), ("decision", pa.string()), ("damage_rate", pa.float64()),
        ("item_url", pa.string()),
    ], metadata={"usnm.format": FORMAT})
    t = pa.Table.from_pylist(rows, schema=schema)
    buf = io.BytesIO()
    pq.write_table(t, buf, compression="zstd")
    return buf.getvalue()


def row_of(q, dmg):
    m = DOC_RE.match(q["doc_id"])
    d = dmg.get(q["doc_id"], {})
    date = dt.date.fromisoformat(m["date"])
    ed = int(m["ed"])
    rate = d.get("damage_rate")
    return {
        "doc_id": q["doc_id"],
        "batch": q["batch"],
        "batch_version": int(q.get("batch_version") or 1),
        "lccn": m["lccn"],
        "date": date,
        "edition": ed,
        "seq": int(m["seq"]),
        "lang": d.get("lang") or q.get("language"),
        "decision": d.get("decision") or "detected",
        "damage_rate": float(rate) if rate is not None else 0.0,
        "item_url": f"https://www.loc.gov/item/{m['lccn']}/{m['date']}/ed-{ed}/",
    }


def cmd_build(a):
    os.makedirs(a.state, exist_ok=True)
    reasons = [r for r in a.reasons.split(",") if r]
    if not reasons or not all(re.match(r"^[a-z]+$", r) for r in reasons):
        sys.exit("--reasons: comma-separated reasons, such as user,bulk")
    last_path = os.path.join(a.state, "last.json")
    index = staged_pairs(a.scan_server)
    if os.path.exists(last_path):
        with open(last_path) as f:
            last = json.load(f)
        pairs = [tuple(p) for p in last.get("pairs", [])]
        on = sum(1 for p in pairs if p in index)
        if not pairs and last.get("docs"):  # written before pairs were kept
            docs = {d for _, d in index}
            pairs = last["docs"]
            on = sum(1 for d in pairs if d in docs)
        age = dt.datetime.now(dt.timezone.utc) - dt.datetime.fromisoformat(last["at"])
        if on < 0.9 * len(pairs) and age < dt.timedelta(days=1):
            log("the last manifest is still being fetched; nothing new", manifest=last["name"],
                on_server=on, pages=len(pairs))
            return
    lake = Lake(a.curated_url)
    queued = cached_rows(lake, a.state, "queued", a.days,
                         ["doc_id", "batch", "batch_version", "reason", "language", "queued_at"])
    outcomes = cached_rows(lake, a.state, "outcomes", a.days, ["doc_id", "outcome", "cause", "at"])
    # Pages already put in MAX_TRIES manifests that still have no scan wait a week, so a
    # page LoC can't serve doesn't hold the front of every manifest.
    tries_path = os.path.join(a.state, "tries.json")
    tries = {}
    if os.path.exists(tries_path):
        with open(tries_path) as f:
            tries = json.load(f)
    now = dt.datetime.now(dt.timezone.utc)
    todo, checked, deferred = [], 0, 0
    for q in pending(queued, outcomes, reasons):
        if len(todo) >= a.max_pages:
            break
        t = tries.get(q["doc_id"])
        if t and t["n"] >= MAX_TRIES and now - dt.datetime.fromisoformat(t["last"]) < dt.timedelta(days=7):
            deferred += 1
            continue
        row = index.get((q["batch"], q["doc_id"]))
        if row is not None:
            checked += 1
            if available(a.scan_server, row):
                continue  # on the scan server now
        todo.append(q)
    by_reason = {}
    for q in todo:
        by_reason[q["reason"]] = by_reason.get(q["reason"], 0) + 1
    if not todo:
        log("nothing queued is missing its scan", queued=len(queued), outcomes=len(outcomes))
        return
    rows, by_batch = [], {}
    for q in todo:
        by_batch.setdefault((q["batch"], int(q.get("batch_version") or 1)), []).append(q)
    for (batch, bv), qs in sorted(by_batch.items()):
        dmg = damage_of(a.curated_copy, batch, bv, {q["doc_id"] for q in qs})
        rows += [row_of(q, dmg) for q in qs]
    body = manifest_bytes(rows)
    name = "queue-ocr-" + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dt%H%M%S")
    if a.what_if:
        log("would queue a manifest", manifest=name, pages=len(rows), by_reason=by_reason, bytes=len(body))
        return
    url = f"{a.scan_server.rstrip('/')}/manifests/{name}/manifest.parquet"
    req = urllib.request.Request(url, data=body, method="PUT",
                                 headers={"User-Agent": USER_AGENT, "Content-Type": "application/octet-stream"})
    with urllib.request.urlopen(req, timeout=120) as r:
        if r.status not in (200, 201):
            sys.exit(f"the scan server answered {r.status} for {url}")
    with open(last_path + ".tmp", "w") as f:
        json.dump({"name": name, "at": dt.datetime.now(dt.timezone.utc).isoformat(),
                   "pairs": [[r["batch"], r["doc_id"]] for r in rows]}, f)
    os.replace(last_path + ".tmp", last_path)
    # Every page OCR asked for: the quality feeder never deletes these scans.
    with open(os.path.join(a.state, "ocr-docs.txt"), "a") as f:
        f.write("".join(f"{r['doc_id']}\n" for r in rows))
    stamp = now.isoformat()
    for r in rows:
        t = tries.get(r["doc_id"])
        if t and now - dt.datetime.fromisoformat(t["last"]) >= dt.timedelta(days=7):
            t = None  # its week is up: a fresh count
        tries[r["doc_id"]] = {"n": (t["n"] if t else 0) + 1, "last": stamp}
    with open(tries_path + ".tmp", "w") as f:
        json.dump(tries, f)
    os.replace(tries_path + ".tmp", tries_path)
    log("queued a manifest", manifest=name, pages=len(rows), by_reason=by_reason, rechecked=checked,
        deferred=deferred)


QUALITY_SQL = """
COPY (
  WITH p AS (
    SELECT s.doc_id, s.batch, s.batch_version, s.decision, s.damage_rate, s.lccn, s.decade, s.date,
           substring(s.doc_id from '_ed-([0-9]+)_seq-') AS ed,
           (s.damage_rate + coalesce(1 - s.loc_wc_mean, s.damage_rate)) / 2 AS badness
    FROM quality.page_summary s
    WHERE NOT s.has_image_features
      AND NOT EXISTS (SELECT FROM quality.staging g WHERE g.doc_id = s.doc_id){batch_filter}
  ), i AS (
    SELECT lccn, decade, date, ed, max(badness) AS worst,
           row_number() OVER (PARTITION BY lccn, decade
                              ORDER BY max(badness) DESC NULLS LAST, date, ed) AS issue_rank
    FROM p
    GROUP BY lccn, decade, date, ed
  )
  SELECT p.doc_id, p.batch, p.batch_version, coalesce(p.decision, 'detected'),
         coalesce(p.damage_rate, 0), p.lccn, p.decade, i.issue_rank
  FROM p JOIN i USING (lccn, decade, date, ed)
  WHERE i.issue_rank <= {strata} + {slack}
  ORDER BY i.issue_rank, i.worst DESC NULLS LAST, p.lccn, p.date, p.ed, p.doc_id
) TO STDOUT WITH (FORMAT csv)
"""

BATCH_RE = re.compile(r"^[a-z]+_[A-Za-z0-9]+_ver[0-9]{2}$")


def read_batches(path):
    """The batches a pass may take pages from, one name a line (`#` starts a comment); a
    tab-separated line counts only when its second field is 200, so the bucket probe's
    `<batch>\t<HTTP status>\t<time>` file can be given as it is."""
    out = set()
    with open(path) as f:
        for line in f:
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            fields = line.split("\t")
            if len(fields) > 1 and fields[1].strip() != "200":
                continue
            name = fields[0].strip()
            if not BATCH_RE.match(name):
                sys.exit(f"{path}: {name!r} isn't a batch name")
            out.add(name)
    if not out:
        sys.exit(f"{path} names no batch")
    return out


def batch_pair(name):
    """A batch's full name as usnm_quality keeps it: ak_goat_ver01 is ("ak_goat", 1)."""
    base, ver = name.rsplit("_ver", 1)
    return base, int(ver)


def issue_of(doc):
    m = DOC_RE.match(doc)
    return (m["lccn"], m["date"], m["ed"]) if m else None


def pick_issues(rows, queued, index, strata, max_pages):
    """From the ranked rows (doc, batch, bv, decision, rate, lccn, decade, rank: each
    stratum's issues worst first, every page of an issue), the first `strata` issues of each
    (title, decade) stratum that still have a page to fetch, every such page of each, in
    round-robin order, until `max_pages`. An issue is taken whole or not at all (the first
    one always), so its METS and its batch's manifest are asked for once for all its pages;
    a page already queued or in the scan server's index is left out, and an issue with
    nothing left gives way to the stratum's next."""
    issues, order = {}, []
    for r in rows:
        doc, batch = r[0], r[1]
        key = issue_of(doc)
        if key is None:
            continue
        if key not in issues:
            issues[key] = {"stratum": (r[5], r[6]), "pages": []}
            order.append(key)
        if doc in queued or (batch, doc) in index:
            continue
        issues[key]["pages"].append(r)
    out, taken = [], {}
    for key in order:
        it = issues[key]
        if not it["pages"] or taken.get(it["stratum"], 0) >= strata:
            continue
        if out and len(out) + len(it["pages"]) > max_pages:
            continue
        taken[it["stratum"]] = taken.get(it["stratum"], 0) + 1
        out.extend(it["pages"])
        if len(out) >= max_pages:
            break
    return out


def cmd_build_quality(a):
    """The full image pass's next pages (usnm_quality's rank.sql query 2, by issue: worst
    first by damage and LoC's own doubt, round-robin over title and decade, the first
    --strata issues of each, every page of an issue), as a queue-quality-<stamp> manifest
    behind OCR's. Taking whole issues lets the fetcher ask for each issue's METS, and each
    batch's manifest, once for all their pages, about one request a page. --batches limits
    the pass to those batches (LoC's bucket's public ones, say). Every row's lang is `eng`,
    so the fetcher files the scan in its buffer set, which the quality feeder empties once
    the page's unit has shipped; the page's own language stays in usnm_quality. Pages
    already in the scan server's index are left out even if their scan is gone: the
    fetcher's `.done` markers keep it from fetching a consumed buffer page again, so
    queueing one would only stall the pass."""
    import csv
    import subprocess

    os.makedirs(a.state, exist_ok=True)
    batch_filter = ""
    if a.batches:
        # usnm_quality keeps a batch's name without its version (ak_goat, 1 for ak_goat_ver01).
        pairs = sorted(batch_pair(n) for n in read_batches(a.batches))
        batch_filter = ("\n      AND (s.batch, s.batch_version) IN ("
                        + ", ".join(f"('{b}', {v})" for b, v in pairs) + ")")
    last_path = os.path.join(a.state, "last-quality.json")
    docs_path = os.path.join(a.state, "quality-docs.txt")
    index = staged_pairs(a.scan_server)
    if os.path.exists(last_path):
        with open(last_path) as f:
            last = json.load(f)
        pairs = [tuple(p) for p in last["pairs"]]
        on = sum(1 for p in pairs if p in index)
        age = dt.datetime.now(dt.timezone.utc) - dt.datetime.fromisoformat(last["at"])
        if on < 0.9 * len(pairs) and age < dt.timedelta(days=2):
            log("the last quality manifest is still being fetched; nothing new", manifest=last["name"],
                on_server=on, pages=len(pairs))
            return
    queued = set()
    for path in (docs_path, os.path.join(a.state, "ocr-docs.txt")):
        if os.path.exists(path):
            with open(path) as f:
                queued |= {line.strip() for line in f if line.strip()}
    # Each stratum is read --slack issues deep, so an issue left out below (all its pages
    # already queued, or already in the index) gives way to the stratum's next one.
    p = subprocess.run(["psql", a.dsn, "-X", "-q", "-v", "ON_ERROR_STOP=1", "-c",
                        QUALITY_SQL.format(strata=int(a.strata), slack=int(a.slack), batch_filter=batch_filter)],
                       capture_output=True, text=True, timeout=3600)
    if p.returncode:
        sys.exit(f"psql failed: {p.stderr.strip()[-500:]}")
    picked = pick_issues(list(csv.reader(io.StringIO(p.stdout))), queued, index, a.strata, a.max_pages)
    rows = [row_of({"doc_id": doc, "batch": batch, "batch_version": int(bv), "language": "eng"},
                   {doc: {"lang": "eng", "decision": decision, "damage_rate": float(rate)}})
            for doc, batch, bv, decision, rate, *_ in picked]
    if not rows:
        log("no quality pages left in these strata", strata=a.strata)
        return
    body = manifest_bytes(rows)
    name = "queue-quality-" + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dt%H%M%S")
    issues = len({issue_of(r["doc_id"]) for r in rows})
    batches = len({r["batch"] for r in rows})
    if a.what_if:
        log("would queue a quality manifest", manifest=name, pages=len(rows), issues=issues, batches=batches,
            bytes=len(body))
        return
    url = f"{a.scan_server.rstrip('/')}/manifests/{name}/manifest.parquet"
    req = urllib.request.Request(url, data=body, method="PUT",
                                 headers={"User-Agent": USER_AGENT, "Content-Type": "application/octet-stream"})
    with urllib.request.urlopen(req, timeout=120) as r:
        if r.status not in (200, 201):
            sys.exit(f"the scan server answered {r.status} for {url}")
    with open(docs_path, "a") as f:
        f.write("".join(f"{r['doc_id']}\n" for r in rows))
    with open(last_path + ".tmp", "w") as f:
        json.dump({"name": name, "at": dt.datetime.now(dt.timezone.utc).isoformat(),
                   "pairs": [[r["batch"], r["doc_id"]] for r in rows]}, f)
    os.replace(last_path + ".tmp", last_path)
    log("queued a quality manifest", manifest=name, pages=len(rows), issues=issues, batches=batches)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("--state", required=True)
    b.add_argument("--days", type=int, default=14)
    b.add_argument("--max-pages", type=int, default=1000)
    b.add_argument("--reasons", default="user,bulk")
    b.add_argument("--curated-url", default="https://stusnmdffapzy.blob.core.windows.net/curated")
    b.add_argument("--curated-copy", default="/srv/curated")
    b.add_argument("--scan-server", default="http://usnm-scanfetch.tjg.fyi:8380")
    b.add_argument("--what-if", action="store_true")
    q = sub.add_parser("build-quality")
    q.add_argument("--state", required=True)
    q.add_argument("--dsn", required=True, help="usnm_quality, a role that may read page_summary")
    q.add_argument("--strata", type=int, default=1, help="issues per (title, decade) stratum to take")
    q.add_argument("--batches", help="take pages only from these batches: one a line, or the bucket probe's TSV")
    q.add_argument("--max-pages", type=int, default=2000)
    q.add_argument("--slack", type=int, default=20, help="extra issues read per stratum to refill it")
    q.add_argument("--scan-server", default="http://usnm-scanfetch.tjg.fyi:8380")
    q.add_argument("--what-if", action="store_true")
    a = ap.parse_args(argv)
    if a.cmd == "build-quality":
        if not 1 <= a.max_pages <= 20000 or not 1 <= a.strata <= 1000:
            sys.exit("--max-pages 1 to 20000, --strata 1 to 1000")
        cmd_build_quality(a)
        return
    if not 1 <= a.max_pages <= 20000 or not 1 <= a.days <= 60:
        sys.exit("--max-pages 1 to 20000, --days 1 to 60")
    cmd_build(a)


if __name__ == "__main__":
    main()
