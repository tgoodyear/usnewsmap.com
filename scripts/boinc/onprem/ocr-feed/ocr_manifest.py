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
    a = ap.parse_args(argv)
    if not 1 <= a.max_pages <= 20000 or not 1 <= a.days <= 60:
        sys.exit("--max-pages 1 to 20000, --days 1 to 60")
    cmd_build(a)


if __name__ == "__main__":
    main()
