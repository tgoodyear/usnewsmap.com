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
      --outbox DIR --state DIR [--chunk-pages 500] [--max-pending 5] [--workers 4]

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
pages it can't use (no LoC text) in skipped.txt with the reason, so a restart goes on
where it stopped. A scan/index mismatch is retried instead of used. With
--quarantine-decode, three known decoder failures of the same verified scan fingerprint
are recorded separately and excluded until its bytes change.
Quarantines in either mode require operator review.
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
import urllib.error
import urllib.request

import scan_quarantine

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
    staged = read_lines(a.staged)
    excluded = getattr(a, "exclude_language", None) or []
    if isinstance(excluded, str):
        excluded = [excluded]
    cand = {}
    with open(a.index) as f:
        for line in f:
            r = json.loads(line)
            if r["doc_id"] in staged or r.get("resolution", "full") != "full":
                continue
            cand[r["doc_id"]] = r
    records = scan_quarantine.quarantined(a.quarantine_state) if getattr(a, "quarantine_state", None) else []
    blocked_ids = {record["id"] for record in records}
    blocked_docs = {record["source"]["doc_id"] for record in records}
    cand = {doc: row for doc, row in cand.items()
            if doc not in blocked_docs or scan_quarantine.source_id(row) not in blocked_ids}
    by_batch = {}
    for r in cand.values():
        by_batch.setdefault((r["batch"], int(r.get("batch_version") or 1)), []).append(r)
    db_damage = None
    if getattr(a, "damage_dsn_file", None):
        import csv
        import subprocess

        if any(not DOC_RE.fullmatch(doc) for doc in cand):
            raise ValueError("invalid indexed document identity")
        query = ("CREATE TEMP TABLE feed_candidates(doc_id text PRIMARY KEY);\n"
                 "COPY feed_candidates FROM STDIN;\n"
                 + "".join(doc + "\n" for doc in sorted(cand))
                 + "\\.\nCOPY (SELECT s.doc_id,s.lang,s.damage_rate FROM quality.page_summary s "
                 "JOIN feed_candidates c USING(doc_id)) TO STDOUT WITH (FORMAT csv);\n")
        with open(a.damage_dsn_file) as f:
            dsn = f.read().strip()
        result = subprocess.run(["psql", dsn, "-X", "-q", "-v", "ON_ERROR_STOP=1"], input=query,
                                capture_output=True, text=True, timeout=1800)
        if result.returncode:
            raise RuntimeError(f"page-damage metadata query failed: {result.stderr.strip()[-300:]}")
        db_damage = {doc: {"doc_id": doc, "lang": lang, "damage_rate": float(rate) if rate else None}
                     for doc, lang, rate in csv.reader(io.StringIO(result.stdout))}
    out = []
    for (batch, bv), rows in sorted(by_batch.items()):
        dmg = db_damage if db_damage is not None else {}
        path = os.path.join(a.curated, "audit/damage/v2", f"{batch}_ver{bv:02d}.parquet")
        if db_damage is None and os.path.exists(path):
            import pyarrow.parquet as pq

            want = {r["doc_id"] for r in rows}
            t = pq.read_table(path, columns=["doc_id", "lang", "damage_rate"],
                              filters=[("doc_id", "in", sorted(want))]).to_pylist()
            dmg = {x["doc_id"]: x for x in t if x["doc_id"] in want}
        for r in rows:
            m = DOC_RE.match(r["doc_id"])
            if not m:
                continue
            d = dmg.get(r["doc_id"], {})
            if getattr(a, "non_english", False) and (not d.get("lang") or lang_of(d["lang"]) == "eng"):
                continue
            language = lang_of(d.get("lang"))
            if ((getattr(a, "language", None) and language != a.language)
                    or language in excluded):
                continue
            out.append({
                "batch": batch,
                "batch_version": bv,
                "bytes": int(r["bytes"]),
                "damage_rate": d.get("damage_rate"),
                "date": m["date"],
                "doc_id": r["doc_id"],
                "edition": int(m["ed"]),
                "fetched_at": r.get("fetched_at", ""),
                "lang": language,
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
    if getattr(a, "scan_server", None):
        available_pages = []
        for p in out:
            row = {**p, "format": "jp2" if p["scan"].endswith(".jp2") else "jpg"}
            st = scan_set(a.scan_server, row)
            if st:
                p["scan"] = st + "/" + p["scan"].split("/", 1)[1]
                available_pages.append(p)
                if a.count and len(available_pages) >= a.count:
                    break
        out = available_pages
    elif a.count:
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


class ScanGone(Exception):
    """The scan server no longer has a buffer scan: a worker read it and deleted it after the
    page was selected."""


def fetch_scan(server, p, timeout=120):
    url = f"{server.rstrip('/')}/scans/{p['scan']}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
    except urllib.error.HTTPError as e:
        if e.code == 404 and p["scan"].startswith("buffer/"):
            raise ScanGone(f"{p['scan']}: no longer on the scan server") from e
        raise
    if len(raw) != p["bytes"] or hashlib.sha256(raw).hexdigest() != p["sha256"]:
        raise ValueError("scan doesn't match its index line")
    return raw


class ScanDecodeError(Exception):
    def __init__(self, error, codec):
        from PIL import __version__, features

        super().__init__(str(error))
        self.original_type = type(error).__name__
        self.decoder = {"pillow": __version__, "codec": codec, "codec_version": features.version_codec(codec)}


def raise_decode_error(error, codec):
    from PIL import UnidentifiedImageError

    if (isinstance(error, UnidentifiedImageError)
            or "image file is truncated" in str(error)
            or "broken data stream when reading image file" in str(error)):
        raise ScanDecodeError(error, codec) from error
    raise error


def page_jpeg(raw, is_jp2):
    """The worker's page.jpg, as the quality runner makes it: grey, quality 75, a JP2 over
    4000 px decoded at half size; a grey JPEG under 40 Mpx and 20 MB goes as it is."""
    from PIL import Image, features

    Image.MAX_IMAGE_PIXELS = None
    codec = "jpg_2000" if is_jp2 else "jpg"
    if not features.check_codec(codec):
        raise RuntimeError(f"Pillow's {codec} decoder is unavailable")
    try:
        image = Image.open(io.BytesIO(raw))
    except OSError as error:
        raise_decode_error(error, codec)
    with image as im:
        if im.format != ("JPEG2000" if is_jp2 else "JPEG"):
            raise ValueError("scan format differs from its indexed file extension")
        if is_jp2 or len(raw) > 20_000_000 or im.mode != "L" or im.size[0] * im.size[1] > 40_000_000:
            if getattr(im, "format", "") == "JPEG2000" and max(im.size) > 4000:
                im.reduce = 1
            try:
                im.load()
            except OSError as error:
                raise_decode_error(error, codec)
            g = im.convert("L")
            buf = io.BytesIO()
            g.save(buf, "JPEG", quality=75)
            return buf.getvalue(), g.size
        try:
            im.load()
        except OSError as error:
            raise_decode_error(error, codec)
        return raw, im.size


def pending_chunks(camp_dir):
    return sorted(d for d in glob.glob(os.path.join(camp_dir, "c*")) if os.path.exists(os.path.join(d, "READY")))


def write_chunk(camp_dir, name, pages, prepared, alto_cache=None):
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
        put(f"texts/{p['doc_id']}.image-provenance.json", json.dumps({
            "schema": 1,
            "source": {k: p[k] for k in ("doc_id", "batch", "batch_version", "bytes", "sha256")},
            "jpeg": {"bytes": len(jpg), "sha256": hashlib.sha256(jpg).hexdigest()},
        }, sort_keys=True).encode())
        if ours:
            with open(ours, "rb") as f:
                put(f"texts/{p['doc_id']}.ours.xml", f.read())
        if alto_cache:
            with open(os.path.join(alto_cache, p["doc_id"] + ".provenance.json"), "rb") as f:
                put(f"texts/{p['doc_id']}.loc-provenance.json", f.read())
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


def cached_alto(cache, doc):
    """Only original, hash-verified archive ALTO; never manufacture crop coordinates."""
    path = os.path.join(cache, doc + ".xml")
    receipt = os.path.join(cache, doc + ".provenance.json")
    with open(receipt) as f:
        provenance = json.load(f)
    with open(path, "rb") as f:
        raw = f.read()
    if hashlib.sha256(raw).hexdigest() != provenance["sha256"] or provenance["doc_id"] != doc:
        raise ValueError(f"{doc}: cached ALTO differs from its archive receipt")
    return raw


def process_chunk(a, texts, chunk, camp_dir, state, seq, done_path, skip_path):
    """Prepare one chunk of pages and write it to the outbox; returns (pages written, the
    number left for a retry, the next chunk number)."""
    retry = 0
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
    blocked = scan_quarantine.quarantined_ids(state) if getattr(a, "quarantine_decode", False) else set()
    for p in chunk:
        if blocked and scan_quarantine.source_id(p) in blocked:
            log("scan remains quarantined", doc_id=p["doc_id"], sha256=p["sha256"])
            continue
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
                result = fut.result()
            except ScanGone as e:
                # Not a failure of this run: selection takes the page again only if its scan
                # is available again.
                log("scan gone; left for a later selection", doc_id=p["doc_id"], error=str(e)[:200])
                if getattr(a, "quarantine_decode", False):
                    scan_quarantine.record_retryable(state, p, e)
            except ScanDecodeError as e:
                log("scan decode failed", doc_id=p["doc_id"], sha256=p["sha256"],
                    error_type=e.original_type, error=str(e)[:200])
                attempts = 0
                if getattr(a, "quarantine_decode", False):
                    attempts = scan_quarantine.record_failure(state, p, e, e.decoder)
                if attempts >= scan_quarantine.FAILURES_BEFORE_QUARANTINE:
                    log("scan quarantined; operator review required", doc_id=p["doc_id"], sha256=p["sha256"],
                        attempts=attempts, error_type=e.original_type, error=str(e)[:200])
                else:
                    retry += 1
                    log("page failed; will retry", doc_id=p["doc_id"], attempts=attempts,
                        error_type=e.original_type, error=str(e)[:200])
            except Exception as e:  # Not classified as corrupt: left for the next run.
                log("page failed; will retry", doc_id=p["doc_id"], error_type=type(e).__name__, error=str(e)[:200])
                if getattr(a, "quarantine_decode", False):
                    scan_quarantine.record_retryable(state, p, e)
                retry += 1
            else:
                if getattr(a, "quarantine_decode", False):
                    scan_quarantine.record_success(state, p)
                prepared[p["doc_id"]] = result
    if getattr(a, "alto_cache", None):
        # A corrupt source receipt is an integrity failure, not a permanently skipped page.
        for doc, (jpg, _, tx, ours_alto_path) in list(prepared.items()):
            prepared[doc] = (jpg, cached_alto(a.alto_cache, doc), tx, ours_alto_path)
    ok = [p for p in usable if p["doc_id"] in prepared]
    if ok:
        name = f"c{seq:05d}"
        write_chunk(camp_dir, name, ok, prepared, getattr(a, "alto_cache", None))
        with open(os.path.join(state, "chunks.txt"), "a") as f:
            f.write(f"{seq}\n")
        seq += 1
        with open(done_path, "a") as f:
            f.write("".join(f"{p['doc_id']}\t{name}\t{p['scan']}\t{p.get('bytes', '')}\t"
                            f"{'' if p.get('damage_rate') is None else p['damage_rate']}\n" for p in ok))
        log("chunk ready", chunk=name, pages=len(ok), with_as=sum(1 for p in ok if p["doc_id"] in as_map),
            with_ours=sum(1 for p in ok if p["doc_id"] in ours))
    if skipped:
        with open(skip_path, "a") as f:
            f.write("".join(f"{d}\t{why}\n" for d, why in skipped))
        log("skipped", pages=len(skipped))
    return len(ok), retry, seq


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
        candidates = [p for p in map(json.loads, f) if p["doc_id"] not in done]
    blocked = scan_quarantine.quarantined_ids(state) if getattr(a, "quarantine_decode", False) else set()
    todo = [p for p in candidates if not blocked or scan_quarantine.source_id(p) not in blocked]
    log("start", campaign=a.campaign, todo=len(todo), done=len(done), quarantined=len(candidates) - len(todo))
    seq = 1 + max([int(os.path.basename(d)[1:]) for d in glob.glob(os.path.join(camp_dir, "c[0-9]*"))]
                  + [int(x) for x in read_lines(os.path.join(state, "chunks.txt"))] + [0])
    texts = Texts(a.curated)
    retry = 0
    pos = 0
    while pos < len(todo):
        while len(pending_chunks(camp_dir)) >= a.max_pending:
            if a.once:
                log("pending chunks at the limit; stopping", pending=a.max_pending)
                if retry:
                    log("pages left to retry", campaign=a.campaign, pages=retry)
                    sys.exit(1)
                return
            time.sleep(60)
        chunk = todo[pos : pos + a.chunk_pages]
        pos += len(chunk)
        _, r, seq = process_chunk(a, texts, chunk, camp_dir, state, seq, done_path, skip_path)
        retry += r
    if retry:
        # Pages in neither done.txt nor skipped.txt: fail, so systemd's Restart=on-failure
        # runs the list again, which takes only those pages.
        log("pages left to retry", campaign=a.campaign, pages=retry)
        sys.exit(1)
    log("done", campaign=a.campaign)


def scan_set(server, row):
    """`keep` or `buffer`: which set the scan an index line names is in now, from its sidecar
    (a few hundred bytes; the scan itself isn't read). None when neither has it (a buffer
    scan already read and deleted)."""
    m = DOC_RE.match(row["doc_id"])
    ext = "jp2" if row.get("format") == "jp2" else "jpg"
    rel = f"{row['batch']}/{m['lccn']}/{m['date']}/{int(m['ed'])}/{int(m['seq'])}.{ext}.json"
    for st in ("keep", "buffer"):
        req = urllib.request.Request(f"{server.rstrip('/')}/scans/{st}/{rel}", headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                r.read(1 << 16)
                return st
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise
    return None


def index_rows(server):
    req = urllib.request.Request(f"{server.rstrip('/')}/index/full.jsonl", headers={"User-Agent": USER_AGENT})
    out = {}
    with urllib.request.urlopen(req, timeout=300) as r:
        for line in io.TextIOWrapper(r, encoding="utf-8"):
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get("resolution", "full") == "full" and DOC_RE.match(row.get("doc_id") or ""):
                out[row["doc_id"]] = row
    return out


def page_of(row, set_name, damage):
    m = DOC_RE.match(row["doc_id"])
    d = damage.get(row["doc_id"], {})
    ext = "jp2" if row.get("format") == "jp2" else "jpg"
    return {
        "batch": row["batch"],
        "batch_version": int(row.get("batch_version") or 1),
        "bytes": int(row["bytes"]),
        "damage_rate": d.get("damage_rate"),
        "date": m["date"],
        "doc_id": row["doc_id"],
        "edition": int(m["ed"]),
        "fetched_at": row.get("fetched_at", ""),
        "lang": lang_of(d.get("lang")),
        "lccn": m["lccn"],
        "scan": f"{set_name}/{row['batch']}/{m['lccn']}/{m['date']}/{int(m['ed'])}/{int(m['seq'])}.{ext}",
        "seq": int(m["seq"]),
        "sha256": row["sha256"],
    }


def read_kept(path):
    """kept.tsv: doc_id, scan, bytes, damage_rate ('' when unscored), last checked (epoch)."""
    out = {}
    if path and os.path.exists(path):
        with open(path) as f:
            for line in f:
                try:
                    doc, scan, size, damage, checked = line.rstrip("\n").split("\t")
                    out[doc] = (scan, int(size), float(damage) if damage else None, float(checked))
                except ValueError:
                    # Left out, the page is still in done.txt and not deleted, so the next
                    # round weighs it again as newly shipped.
                    log("kept.tsv line unreadable; skipped", line=line[:200])
    return out


def write_kept(path, kept):
    with open(path + ".tmp", "w") as f:
        f.write("".join(f"{doc}\t{scan}\t{size}\t{'' if damage is None else damage}\t{checked:.0f}\n"
                        for doc, (scan, size, damage, checked) in sorted(kept.items())))
    os.replace(path + ".tmp", path)


def scan_exists(server, scan):
    """Whether the scan server still has a scan, from its sidecar (a few hundred bytes)."""
    req = urllib.request.Request(f"{server.rstrip('/')}/scans/{scan}.json", headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            r.read(1 << 16)
            return True
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return False
        raise


def delete_shipped(a, camp_dir, done_path, quality_docs, deleted_path, kept_path=None):
    """The buffer scans of the quality pass's pages whose chunks have shipped (the shipper
    removed the chunk from the outbox after submitting it). Only pages the quality pass asked
    for and OCR didn't (the caller leaves OCR's manifests' pages out of `quality_docs`); OCR's
    pages and the keep set are never touched. With --keep-bytes, the most damaged of them stay
    on the scan server for re-OCR, up to that many bytes in all (listed in `kept_path`), and the
    rest are DELETEd; without, every one is. A kept scan that is gone (an
    OCR worker read it) or that OCR has since asked for leaves the list without a DELETE, so it
    no longer counts against the budget; --keep-verify of the longest-unchecked are looked at a
    round. Returns how many were deleted."""
    deleted = read_lines(deleted_path)
    budget = getattr(a, "keep_bytes", 0) or 0
    kept = read_kept(kept_path)
    now = time.time()
    with open(done_path) as f:
        rows = [line.rstrip("\n").split("\t") for line in f if "\t" in line]
    pending = {os.path.basename(d) for d in glob.glob(os.path.join(camp_dir, "c[0-9]*"))}
    shipped = {}
    for doc, chunk, *rest in rows:
        if doc in deleted or doc in kept or doc not in quality_docs or chunk in pending or not rest:
            continue
        scan = rest[0]
        if not scan.startswith("buffer/"):
            continue
        # Rows written before --keep-bytes have no size, so they can't be budgeted: deleted.
        size = int(rest[1]) if len(rest) > 1 and rest[1] else 0
        damage = float(rest[2]) if len(rest) > 2 and rest[2] else None
        shipped[doc] = (scan, size, damage, now)
    released = [doc for doc in kept if doc not in quality_docs]
    if budget:
        for doc, k in sorted(kept.items(), key=lambda kv: kv[1][3])[: getattr(a, "keep_verify", 0)]:
            if doc not in quality_docs:
                continue
            try:
                there = scan_exists(a.scan_server, k[0])
            except (urllib.error.URLError, OSError) as e:
                log("kept scan check failed; next round", doc_id=doc, error=str(e)[:200])
                break
            if there:
                kept[doc] = (*k[:3], now)
            else:
                released.append(doc)
    for doc in released:
        kept.pop(doc, None)
    pool = {**kept, **shipped}
    want, used = {}, 0
    if budget:
        # Most damaged first (unscored last); the first that doesn't fit ends the list.
        for doc, k in sorted(pool.items(), key=lambda kv: (-(kv[1][2] if kv[1][2] is not None else -1.0), kv[0])):
            if not k[1]:
                continue
            if used + k[1] > budget:
                break
            want[doc] = k
            used += k[1]

    def delete(doc):
        req = urllib.request.Request(f"{a.scan_server.rstrip('/')}/scans/{pool[doc][0]}", method="DELETE",
                                     headers={"User-Agent": USER_AGENT})
        try:
            with urllib.request.urlopen(req, timeout=30):
                pass
        except urllib.error.HTTPError as e:
            if e.code != 404:
                log("buffer delete failed", doc_id=doc, status=e.code)
                return False
        return True

    out = list(released)
    # Kept scans that lost their place go first. One that fails to go stays kept and still
    # counts, so the pages that would have taken its place wait, neither kept nor deleted,
    # for a later round: the kept list never holds more than the budget.
    stuck = {}
    for doc in sorted(d for d in kept if d not in want):
        if delete(doc):
            out.append(doc)
        else:
            stuck[doc] = kept[doc]
    keep = {d: k for d, k in want.items() if d in kept or not stuck}
    keep.update(stuck)
    if stuck:
        log("kept scans not deleted; their replacements wait", pages=len(stuck),
            waiting=sum(1 for d in want if d not in keep))
    for doc in sorted(d for d in shipped if d not in want):
        if delete(doc):
            out.append(doc)
    used = sum(k[1] for k in keep.values())
    n = len(out) - len(released)
    if out:
        with open(deleted_path, "a") as f:
            f.write("".join(f"{d}\n" for d in out))
    if kept_path and (keep or os.path.exists(kept_path)):
        write_kept(kept_path, keep)
        if shipped or released:
            floor = min((k[2] for k in keep.values() if k[2] is not None), default=None)
            log("buffer scans kept for re-OCR", pages=len(keep), bytes=used, budget=budget,
                damage_floor=floor, released=len(released))
    return n


def last_chunk_at(state):
    """When the last chunk was written (chunks.txt's mtime); 0 when none has been."""
    try:
        return os.path.getmtime(os.path.join(state, "chunks.txt"))
    except OSError:
        return 0.0


def staged_from_db(dsn):
    """doc_ids that already have a unit or a staging record in usnm_quality."""
    import subprocess

    p = subprocess.run(["psql", dsn, "-X", "-q", "-v", "ON_ERROR_STOP=1", "-At", "-c",
                        "SELECT doc_id FROM quality.unit_pages UNION SELECT doc_id FROM quality.staging"],
                       capture_output=True, text=True, timeout=1800)
    if p.returncode:
        raise RuntimeError(f"usnm_quality's staged pages: {p.stderr.strip()[-300:]}")
    return {line for line in p.stdout.split("\n") if line}


def cmd_follow(a):
    """The piggyback and the quality pass, as a loop: every page the fetcher writes
    (OCR's and the quality pass's) that no campaign has staged gets a unit, in fetch order,
    in chunks of --chunk-pages: a part chunk only once no chunk has been written for
    --max-wait-secs, so a trickle of fetches doesn't become a stream of small units. The
    quality pass's buffer scans are deleted once their chunk has shipped, except the most
    damaged, up to --keep-bytes, which stay for re-OCR."""
    import pyarrow.parquet as pq

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
    deleted_path = os.path.join(state, "deleted.txt")
    kept_path = os.path.join(state, "kept.tsv")
    texts = Texts(a.curated)
    absent_path = os.path.join(state, "absent.txt")
    while True:
        # Pages with a unit or a staging record anywhere (usnm_quality), plus a static list of
        # older campaigns' pages; with neither, stop rather than stage pages twice.
        staged = read_lines(a.staged) if a.staged else set()
        if a.staged_dsn:
            staged |= staged_from_db(a.staged_dsn)
        elif not a.staged or not os.path.exists(a.staged):
            sys.exit("follow needs --staged-dsn or an existing --staged list")
        # Index lines whose scan was gone when last looked at, by fetch time: skipped until
        # the page is fetched again.
        absent = {}
        if os.path.exists(absent_path):
            with open(absent_path) as f:
                for line in f:
                    d, _, at = line.rstrip("\n").partition("\t")
                    absent[d] = at
        ocr_docs = read_lines(a.ocr_docs) if a.ocr_docs else set()
        done = read_lines(done_path) | read_lines(skip_path)
        quality_docs = read_lines(a.quality_docs) if a.quality_docs else set()
        if os.path.exists(done_path):
            gone = delete_shipped(a, camp_dir, done_path, quality_docs - ocr_docs, deleted_path, kept_path)
            if gone:
                log("buffer scans deleted after shipping", pages=gone)
        quarantined = scan_quarantine.quarantined(state) if getattr(a, "quarantine_decode", False) else []
        blocked_ids = {record["id"] for record in quarantined}
        blocked_docs = {record["source"]["doc_id"] for record in quarantined}
        rows, excluded = [], 0
        for doc, row in index_rows(a.scan_server).items():
            if doc in staged or doc in done or absent.get(doc) == row.get("fetched_at", ""):
                continue
            if doc in blocked_docs and scan_quarantine.source_id(row) in blocked_ids:
                excluded += 1
                continue
            rows.append(row)
        if excluded:
            log("quarantined scans excluded", campaign=a.campaign, pages=excluded)
        rows.sort(key=lambda r: (r.get("fetched_at", ""), r["doc_id"]))
        room = max(0, a.max_pending - len(pending_chunks(camp_dir))) * a.chunk_pages
        overdue = time.time() - last_chunk_at(state) >= a.max_wait_secs
        if len(rows) < a.chunk_pages and not overdue:
            if rows:
                log("waiting for a full chunk", campaign=a.campaign, pages=len(rows), chunk_pages=a.chunk_pages)
            rows = []
        todo = []
        for r in rows:
            if len(todo) >= room:
                break
            st = scan_set(a.scan_server, r)
            if st:
                todo.append((r, st))
            else:
                with open(absent_path, "a") as f:
                    f.write(f"{r['doc_id']}\t{r.get('fetched_at', '')}\n")
        partial = len(todo) % a.chunk_pages
        if partial and (not overdue or (len(todo) > partial and a.max_wait_secs > 0)):
            # The newest pages wait for the next round, which has more; a round that writes a
            # full chunk restarts --max-wait-secs, so its remainder isn't overdue either.
            log("waiting for a full chunk", campaign=a.campaign, pages=partial, chunk_pages=a.chunk_pages)
            todo = todo[: len(todo) - partial]
        if todo:
            damage = {}
            for (batch, bv) in sorted({(r["batch"], int(r.get("batch_version") or 1)) for r, _ in todo}):
                path = os.path.join(a.curated, "audit/damage/v2", f"{batch}_ver{bv:02d}.parquet")
                if os.path.exists(path):
                    want = {r["doc_id"] for r, _ in todo}
                    for x in pq.read_table(path, columns=["doc_id", "lang", "damage_rate"]).to_pylist():
                        if x["doc_id"] in want:
                            damage[x["doc_id"]] = x
            pages = [page_of(r, st, damage) for r, st in todo]
            seq = 1 + max([int(os.path.basename(d)[1:]) for d in glob.glob(os.path.join(camp_dir, "c[0-9]*"))]
                          + [int(x) for x in read_lines(os.path.join(state, "chunks.txt"))] + [0])
            for i in range(0, len(pages), a.chunk_pages):
                _, _, seq = process_chunk(a, texts, pages[i : i + a.chunk_pages], camp_dir, state, seq,
                                          done_path, skip_path)
        if a.once:
            return
        wait_for_room(camp_dir, a.max_pending, a.every_secs, busy=room == 0 or bool(todo))


def wait_for_room(camp_dir, max_pending, every_secs, busy, poll_secs=15, min_secs=60):
    """Waits for the next round: --every-secs when the last one had room and found nothing to
    feed. After a round that made chunks or found the outbox full, only until the shipper takes
    one (polled every 15 s; at least a minute,
    at most --every-secs), since the shipper drains a full outbox in minutes and a flat sleep
    left it empty for the rest of it."""
    if not busy:
        time.sleep(every_secs)
        return
    start = time.monotonic()
    time.sleep(min(min_secs, every_secs))
    while time.monotonic() - start < every_secs and len(pending_chunks(camp_dir)) >= max_pending:
        time.sleep(poll_secs)


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
    s.add_argument("--non-english", action="store_true", help="only pages explicitly scored as non-English")
    s.add_argument("--language", help="only pages in this language")
    s.add_argument("--exclude-language", action="append",
                   help="leave out pages in this language; may repeat")
    s.add_argument("--scan-server", help="check actual scan availability before taking --count pages")
    s.add_argument("--damage-dsn-file", help="query indexed quality.page_summary instead of rereading corpus Parquet")
    s.add_argument("--quarantine-state", help="campaign state whose quarantined scan fingerprints are excluded")
    r = sub.add_parser("run")
    r.add_argument("--campaign", required=True)
    r.add_argument("--pages", required=True)
    r.add_argument("--curated", required=True)
    r.add_argument("--scan-server", required=True)
    r.add_argument("--outbox", required=True)
    r.add_argument("--state", required=True)
    r.add_argument("--chunk-pages", type=int, default=500)
    r.add_argument("--max-pending", type=int, default=5)
    r.add_argument("--workers", type=int, default=4)
    r.add_argument("--once", action="store_true", help="stop instead of waiting at --max-pending")
    r.add_argument("--alto-cache", help="original bulk-archive ALTO with per-page SHA256 provenance receipts")
    r.add_argument("--quarantine-decode", action="store_true",
                   help="quarantine repeated known decoder failures of the same verified scan")
    w = sub.add_parser("follow")
    w.add_argument("--campaign", required=True)
    w.add_argument("--curated", required=True)
    w.add_argument("--scan-server", required=True)
    w.add_argument("--outbox", required=True)
    w.add_argument("--state", required=True)
    w.add_argument("--staged", help="doc_ids older campaigns staged, one a line")
    w.add_argument("--staged-dsn", help="usnm_quality: pages with a unit or a staging record are left out")
    w.add_argument("--ocr-docs", help="doc_ids OCR's manifests asked for (ocr_manifest.py): never deleted")
    w.add_argument("--quality-docs", help="doc_ids the quality pass fetched (ocr_manifest.py build-quality)")
    w.add_argument("--chunk-pages", type=int, default=500)
    w.add_argument("--max-pending", type=int, default=5)
    w.add_argument("--workers", type=int, default=4)
    w.add_argument("--every-secs", type=int, default=600)
    w.add_argument("--max-wait-secs", type=int, default=6 * 3600,
                   help="write a part chunk once no chunk has been written for this long")
    w.add_argument("--keep-bytes", type=int, default=0,
                   help="keep the most damaged shipped quality-pass buffer scans, up to this many bytes")
    w.add_argument("--keep-verify", type=int, default=500,
                   help="kept scans checked a round, so ones read and deleted elsewhere stop counting")
    w.add_argument("--once", action="store_true")
    w.add_argument("--quarantine-decode", action="store_true",
                   help="quarantine repeated verified decoder failures for operator review")
    a = ap.parse_args(argv)
    if a.cmd == "select":
        cmd_select(a)
    else:
        if not 1 <= a.chunk_pages <= 500:
            sys.exit("--chunk-pages: 1 to 500")
        if a.cmd == "follow" and min(a.max_wait_secs, a.keep_bytes, a.keep_verify) < 0:
            sys.exit("--max-wait-secs, --keep-bytes and --keep-verify: 0 or more")
        cmd_follow(a) if a.cmd == "follow" else cmd_run(a)


if __name__ == "__main__":
    main()
