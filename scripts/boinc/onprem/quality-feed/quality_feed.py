#!/usr/bin/env python3
"""The on-prem quality feeder (#962): turns scans we already hold into usnm_quality inputs.

It runs in the batch container CT215 on pve-laptop and makes no LoC request. Each page's
scan comes from pve-big's scan server (the keep set on tank, read over the LAN), its texts
from the on-prem curated copy (/srv/curated, read-only), and its ALTO is empty, as in
v1-wave9 (LoC's ALTO would be a LoC request, and CT211's fetcher is the only LoC client).
Our own ALTO, where ocr-local has it, goes beside the texts as <doc_id>.ours.xml.

  quality_feed.py select --index FILE --staged FILE --curated DIR --count N --out FILE
      [--order damage|fetched]
  quality_feed.py run --campaign NAME --pages FILE --curated DIR --scan-server URL
      --outbox DIR --state DIR [--chunk-pages 200] [--max-pending 5] [--workers 4]

select picks pages from the scan server's index (/index/full.jsonl) that no campaign has
staged (--staged: one doc_id a line), worst damage first (damage_rate from the curated
copy's audit/damage/v2), and writes one JSON line a page. --order fetched takes them in the
order the fetcher wrote them instead: the piggyback on OCR's fetches, where every page the
fetcher pulls gets a quality unit as it lands.

run works through --pages in order and writes chunks of --chunk-pages pages under
<outbox>/<campaign>/<chunk>/: scans/<doc_id>.jpg and .xml, texts/<doc_id>.json,
sample.jsonl, langs.txt and SHA256SUMS, then READY last. It waits while --max-pending
chunks are READY and not yet shipped (the host's quality-ship.sh moves them to the BOINC
server and removes them). Pages it has written are listed in <state>/<campaign>/done.txt,
pages it can't use (no LoC text, a scan that doesn't match its index line) in skipped.txt
with the reason, so a restart goes on where it stopped.
"""
import argparse
import concurrent.futures
import glob
import hashlib
import io
import json
import os
import re
import shutil
import sys
import time
import urllib.request

ALL_LANGS = {
    "ces", "dan", "deu", "eng", "fin", "fra", "haw", "hrv", "hun", "ita",
    "lav", "lit", "nor", "pol", "ron", "rus", "slv", "spa", "srp", "swe", "yid",
}
LANG_ALIASES = {"ger": "deu", "fre": "fra", "cze": "ces", "rum": "ron"}
DOC_RE = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
CAMPAIGN_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,40}$")
USER_AGENT = "usnewsmap-quality-feed/1 (+https://usnewsmap.com)"
MAX_TEXT_BYTES = 500_000


def log(msg, **kv):
    print(json.dumps({"msg": msg, **kv}, sort_keys=True), flush=True)


def lang_of(raw):
    lang = LANG_ALIASES.get(raw or "eng", raw or "eng")
    return lang if lang in ALL_LANGS else "eng"


def normalize_text(txt):
    """As the quality runner does: control characters out, at most 500 KB."""
    if not isinstance(txt, str):
        txt = str(txt or "")
    txt = "".join(ch for ch in txt if ch in ("\n", "\r", "\t") or ord(ch) >= 32)
    b = txt.encode("utf-8")
    if len(b) > MAX_TEXT_BYTES:
        txt = b[:MAX_TEXT_BYTES].decode("utf-8", errors="ignore")
    return txt


def reader_name(engine, source):
    low = f"{engine} {source}".lower()
    for name in ("surya", "ndlocr", "paddle"):
        if name in low:
            return f"ours-{name}"
    return "ours-tesseract"


def empty_alto(width, height):
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">'
        "<Description><MeasurementUnit>pixel</MeasurementUnit></Description>"
        f'<Layout><Page ID="P1" WIDTH="{width}" HEIGHT="{height}" PHYSICAL_IMG_NR="1">'
        f'<PrintSpace HPOS="0" VPOS="0" WIDTH="{width}" HEIGHT="{height}"/>'
        "</Page></Layout></alto>\n"
    ).encode()


def sample_row(p):
    dr = p.get("damage_rate")
    dr = 0.15 if dr is None else float(dr)
    year = int(p["date"][:4])
    return {
        "batch": p["batch"],
        "batch_version": int(p["batch_version"]),
        "crash_weight": 1.0,
        "damage": "low" if dr < 0.1 else ("mid" if dr < 0.5 else "high"),
        "damage_rate": dr,
        "decade": (year // 10) * 10,
        "doc_id": p["doc_id"],
        "lang": p["lang"],
        "mode": "ocr",
        "prefix": p["batch"].split("_")[0],
        "stratum_pages": 1,
        "weight": 1.0,
        "year": year,
    }


def scan_path(row):
    """keep/<batch>/<lccn>/<date>/<edition>/<seq>.<jp2|jpg> for an index line."""
    m = DOC_RE.match(row["doc_id"])
    if not m:
        raise ValueError(f"not a page id: {row['doc_id']}")
    ext = "jp2" if row.get("format") == "jp2" else "jpg"
    return f"keep/{row['batch']}/{m['lccn']}/{m['date']}/{int(m['ed'])}/{int(m['seq'])}.{ext}"


def read_lines(path):
    if not path or not os.path.exists(path):
        return set()
    with open(path) as f:
        return {line.split("\t", 1)[0].strip() for line in f if line.strip()}


# --- select ---------------------------------------------------------------------------


def cmd_select(a):
    import pyarrow.parquet as pq

    staged = read_lines(a.staged)
    cand = {}
    with open(a.index) as f:
        for line in f:
            r = json.loads(line)
            if r["doc_id"] in staged or r.get("resolution", "full") != "full":
                continue
            cand[r["doc_id"]] = r
    by_batch = {}
    for r in cand.values():
        by_batch.setdefault((r["batch"], int(r.get("batch_version") or 1)), []).append(r)
    out = []
    for (batch, bv), rows in sorted(by_batch.items()):
        dmg = {}
        path = os.path.join(a.curated, "audit/damage/v2", f"{batch}_ver{bv:02d}.parquet")
        if os.path.exists(path):
            want = {r["doc_id"] for r in rows}
            t = pq.read_table(path, columns=["doc_id", "lang", "damage_rate"]).to_pylist()
            dmg = {x["doc_id"]: x for x in t if x["doc_id"] in want}
        for r in rows:
            m = DOC_RE.match(r["doc_id"])
            if not m:
                continue
            d = dmg.get(r["doc_id"], {})
            out.append({
                "batch": batch,
                "batch_version": bv,
                "bytes": int(r["bytes"]),
                "damage_rate": d.get("damage_rate"),
                "date": m["date"],
                "doc_id": r["doc_id"],
                "edition": int(m["ed"]),
                "fetched_at": r.get("fetched_at", ""),
                "lang": lang_of(d.get("lang")),
                "lccn": m["lccn"],
                "scan": scan_path(r),
                "seq": int(m["seq"]),
                "sha256": r["sha256"],
            })
    if a.order == "fetched":
        out.sort(key=lambda p: (p["fetched_at"], p["doc_id"]))
    else:
        # Worst damage first; pages with no damage score after every scored page.
        out.sort(key=lambda p: (p["damage_rate"] is None, -(p["damage_rate"] or 0.0), p["doc_id"]))
    if a.count:
        out = out[: a.count]
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    tmp = a.out + ".tmp"
    with open(tmp, "w") as f:
        for p in out:
            f.write(json.dumps(p, sort_keys=True) + "\n")
    os.replace(tmp, a.out)
    scored = sum(1 for p in out if p["damage_rate"] is not None)
    log("selected", pages=len(out), scored=scored, candidates=len(cand), out=a.out)


# --- run ------------------------------------------------------------------------------


class Texts:
    """LoC's text, American Stories and ocr-local for a set of pages, from the curated copy."""

    def __init__(self, curated):
        self.curated = curated
        self._ours_alto = None

    def loc(self, batch, bv, docs):
        import pyarrow.parquet as pq

        # Every run directory of the batch, oldest first, so the newest run's text wins.
        files = sorted(glob.glob(os.path.join(self.curated, "pages", batch, f"v{bv:02d}", "*", "*.parquet")))
        got = {}
        for fp in files:
            for x in pq.read_table(fp, columns=["doc_id", "ocr_source", "text"]).to_pylist():
                if x["doc_id"] in docs and x["text"]:
                    got[x["doc_id"]] = x["text"]
        return got, bool(files)

    def american_stories(self, lccn, years, docs):
        import pyarrow.parquet as pq

        got = {}
        for y in sorted(years):
            for fp in sorted(glob.glob(os.path.join(self.curated, "american-stories/pages", lccn, f"{y}-*.parquet"))):
                for x in pq.read_table(fp, columns=["doc_id", "text"]).to_pylist():
                    if x["doc_id"] in docs and x["text"]:
                        got[x["doc_id"]] = x["text"]
        return got

    def ours(self, lccn, docs):
        import pyarrow.parquet as pq

        got = {}
        files = sorted(glob.glob(os.path.join(self.curated, "ocr-local/pages", lccn, "*.parquet")))
        for fp in files:  # names start with the run time, so the newest wins
            for x in pq.read_table(fp, columns=["doc_id", "ocr_engine", "ocr_source", "text"]).to_pylist():
                if x["doc_id"] in docs and x["text"]:
                    got[x["doc_id"]] = (x["ocr_engine"] or "", x["ocr_source"] or "", x["text"])
        return got

    def ours_alto(self, doc_id):
        if self._ours_alto is None:
            idx = {}
            for name in sorted(os.listdir(os.path.join(self.curated, "ocr-local/alto"))):
                if name.endswith(".xml"):
                    idx[name.split(".", 1)[0]] = name  # sorted: the newest file wins
            self._ours_alto = idx
        name = self._ours_alto.get(doc_id)
        return os.path.join(self.curated, "ocr-local/alto", name) if name else None


def fetch_scan(server, p, timeout=120):
    url = f"{server.rstrip('/')}/scans/{p['scan']}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        raw = r.read()
    if len(raw) != p["bytes"] or hashlib.sha256(raw).hexdigest() != p["sha256"]:
        raise ValueError("scan doesn't match its index line")
    return raw


def page_jpeg(raw, is_jp2):
    """The worker's page.jpg, as the quality runner makes it: grey, quality 75, a JP2 over
    4000 px decoded at half size; a grey JPEG under 40 Mpx and 20 MB goes as it is."""
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = None
    with Image.open(io.BytesIO(raw)) as im:
        if is_jp2 or len(raw) > 20_000_000 or im.mode != "L" or im.size[0] * im.size[1] > 40_000_000:
            if getattr(im, "format", "") == "JPEG2000" and max(im.size) > 4000:
                im.reduce = 1
            g = im.convert("L")
            buf = io.BytesIO()
            g.save(buf, "JPEG", quality=75)
            return buf.getvalue(), g.size
        return raw, im.size


def pending_chunks(camp_dir):
    return sorted(d for d in glob.glob(os.path.join(camp_dir, "c*")) if os.path.exists(os.path.join(d, "READY")))


def write_chunk(camp_dir, name, pages, prepared):
    """prepared: doc_id -> (jpeg bytes, alto bytes, texts dict, ours alto path or None)."""
    final = os.path.join(camp_dir, name)
    tmp = os.path.join(camp_dir, f".{name}.tmp")
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(os.path.join(tmp, "scans"))
    os.makedirs(os.path.join(tmp, "texts"))
    sums = []

    def put(rel, data):
        with open(os.path.join(tmp, rel), "wb") as f:
            f.write(data)
        sums.append(f"{hashlib.sha256(data).hexdigest()}  {rel}")

    langs = set()
    rows = []
    for p in pages:
        jpg, alto, texts, ours = prepared[p["doc_id"]]
        put(f"scans/{p['doc_id']}.jpg", jpg)
        put(f"scans/{p['doc_id']}.xml", alto)
        put(f"texts/{p['doc_id']}.json", json.dumps(texts).encode())
        if ours:
            with open(ours, "rb") as f:
                put(f"texts/{p['doc_id']}.ours.xml", f.read())
        rows.append(json.dumps(sample_row(p), sort_keys=True))
        langs.add(p["lang"])
    put("sample.jsonl", ("\n".join(rows) + "\n").encode())
    put("langs.txt", (",".join(sorted(langs)) + "\n").encode())
    with open(os.path.join(tmp, "SHA256SUMS"), "w") as f:
        f.write("\n".join(sums) + "\n")
    os.replace(tmp, final)
    with open(os.path.join(final, "READY"), "w") as f:
        f.write(f"{len(pages)}\n")


def prepare_page(server, texts, p, loc_text, as_text, ours):
    raw = fetch_scan(server, p)
    jpg, (w, h) = page_jpeg(raw, p["scan"].endswith(".jp2"))
    readers = [{"alto": False, "name": "loc-hist", "regions": [], "source": "loc-hist",
                "text": normalize_text(loc_text) or " "}]
    if as_text and normalize_text(as_text).strip():
        readers.append({"alto": False, "name": "as", "regions": [], "source": "american-stories",
                        "text": normalize_text(as_text)})
    if ours and normalize_text(ours[2]).strip():
        readers.append({"alto": False, "name": reader_name(ours[0], ours[1]), "regions": [],
                        "source": "ocr-local:" + (ours[1] or "tesseract"), "text": normalize_text(ours[2])})
    return jpg, empty_alto(w, h), {"page": None, "readers": readers}, texts.ours_alto(p["doc_id"])


def cmd_run(a):
    if not CAMPAIGN_RE.match(a.campaign):
        sys.exit("--campaign: 1-41 of [a-z0-9-]")
    if not os.path.isdir(os.path.join(a.curated, "pages")):
        sys.exit(f"{a.curated} has no pages/: the curated copy isn't mounted")
    state = os.path.join(a.state, a.campaign)
    camp_dir = os.path.join(a.outbox, a.campaign)
    os.makedirs(state, exist_ok=True)
    os.makedirs(camp_dir, exist_ok=True)
    done_path = os.path.join(state, "done.txt")
    skip_path = os.path.join(state, "skipped.txt")
    done = read_lines(done_path) | read_lines(skip_path)
    with open(a.pages) as f:
        todo = [p for p in map(json.loads, f) if p["doc_id"] not in done]
    log("start", campaign=a.campaign, todo=len(todo), done=len(done))
    seq = 1 + max([int(os.path.basename(d)[1:]) for d in glob.glob(os.path.join(camp_dir, "c[0-9]*"))]
                  + [int(x) for x in read_lines(os.path.join(state, "chunks.txt"))] + [0])
    texts = Texts(a.curated)
    retry = 0
    pos = 0
    while pos < len(todo):
        while len(pending_chunks(camp_dir)) >= a.max_pending:
            if a.once:
                log("pending chunks at the limit; stopping", pending=a.max_pending)
                return
            time.sleep(60)
        chunk = todo[pos : pos + a.chunk_pages]
        pos += len(chunk)
        docs = {p["doc_id"] for p in chunk}
        loc, as_map, ours = {}, {}, {}
        complete = True
        for (batch, bv) in sorted({(p["batch"], p["batch_version"]) for p in chunk}):
            got, any_files = texts.loc(batch, bv, docs)
            loc.update(got)
            complete = complete and any_files
        for lccn in sorted({p["lccn"] for p in chunk}):
            years = {p["date"][:4] for p in chunk if p["lccn"] == lccn}
            as_map.update(texts.american_stories(lccn, years, docs))
            ours.update(texts.ours(lccn, docs))
        skipped = []
        prepared = {}
        usable = []
        for p in chunk:
            if p["doc_id"] not in loc:
                if complete:
                    skipped.append((p["doc_id"], "no LoC text in the curated copy"))
                continue
            usable.append(p)
        with concurrent.futures.ThreadPoolExecutor(max_workers=a.workers) as ex:
            futs = {ex.submit(prepare_page, a.scan_server, texts, p, loc[p["doc_id"]],
                              as_map.get(p["doc_id"]), ours.get(p["doc_id"])): p for p in usable}
            for fut in concurrent.futures.as_completed(futs):
                p = futs[fut]
                try:
                    prepared[p["doc_id"]] = fut.result()
                except ValueError as e:
                    skipped.append((p["doc_id"], str(e)))
                except Exception as e:  # a transient read: left for the next run
                    retry += 1
                    log("page failed; will retry", doc_id=p["doc_id"], error=str(e)[:200])
        ok = [p for p in usable if p["doc_id"] in prepared]
        if ok:
            name = f"c{seq:05d}"
            write_chunk(camp_dir, name, ok, prepared)
            with open(os.path.join(state, "chunks.txt"), "a") as f:
                f.write(f"{seq}\n")
            seq += 1
            with open(done_path, "a") as f:
                f.write("".join(f"{p['doc_id']}\t{name}\n" for p in ok))
            log("chunk ready", chunk=name, pages=len(ok), with_as=sum(1 for p in ok if p["doc_id"] in as_map),
                with_ours=sum(1 for p in ok if p["doc_id"] in ours))
        if skipped:
            with open(skip_path, "a") as f:
                f.write("".join(f"{d}\t{why}\n" for d, why in skipped))
            log("skipped", pages=len(skipped))
    if retry:
        # Pages in neither done.txt nor skipped.txt: fail, so systemd's Restart=on-failure
        # runs the list again, which takes only those pages.
        log("pages left to retry", campaign=a.campaign, pages=retry)
        sys.exit(1)
    log("done", campaign=a.campaign)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("select")
    s.add_argument("--index", required=True)
    s.add_argument("--staged", required=True)
    s.add_argument("--curated", required=True)
    s.add_argument("--count", type=int, default=0)
    s.add_argument("--out", required=True)
    s.add_argument("--order", choices=["damage", "fetched"], default="damage")
    r = sub.add_parser("run")
    r.add_argument("--campaign", required=True)
    r.add_argument("--pages", required=True)
    r.add_argument("--curated", required=True)
    r.add_argument("--scan-server", required=True)
    r.add_argument("--outbox", required=True)
    r.add_argument("--state", required=True)
    r.add_argument("--chunk-pages", type=int, default=200)
    r.add_argument("--max-pending", type=int, default=5)
    r.add_argument("--workers", type=int, default=4)
    r.add_argument("--once", action="store_true", help="stop instead of waiting at --max-pending")
    a = ap.parse_args(argv)
    if a.cmd == "select":
        cmd_select(a)
    else:
        if not 1 <= a.chunk_pages <= 500:
            sys.exit("--chunk-pages: 1 to 500")
        cmd_run(a)


if __name__ == "__main__":
    main()
