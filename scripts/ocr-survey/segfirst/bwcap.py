#!/usr/bin/env python3
"""Beyond Words caption test (#2122, plan comment 7129161579): LoC ALTO (L) against whole-page Tesseract (W).

    bwcap.py select --bw bw.json --out-dir D
    bwcap.py check-pages --ranking D/ranking.json --held F --n 10 --out F
    bwcap.py plan  --pages D/pages.json --index index-full.jsonl --held F [--store-listing F ...] --locstats-dir D --out F
    bwcap.py alto  --pages P --ids-file F --locstats-dir D --locbulk-dir D --alto-dir D
    bwcap.py run   --pages P --ids-file F --scans F --alto-dir D --out-dir D [--arms L,W] [--workers 3] [--measure-run]
    bwcap.py score --pages P --ids-file F --run-dir D --out F [--measure-run]
    bwcap.py overlay --pages P --run-dir D --page ID --out F

Truth: Beyond Words (Library of Congress, CC0), the verified regions of newspaper-navigator's beyond_words_data.
A caption is a region in the Photograph category whose verified `caption` has at least 20 characters after
score2122.normalise; each caption is one truth article. Beyond Words' box is the marked picture region, which
volunteers drew to take in its caption; the words of an arm inside that box are the arm's text for the caption.

Pages: every page (doc id <lccn>_<date>_ed-N_seq-N, from the region's Chronicling America URL) with at least one
caption, ranked by sha256("2122:bw:" + doc id); the first 600 in that order with at most 10 pages per title (LCCN).

Coordinates: everything is in the scan's pixels. Beyond Words' box is scaled from Beyond Words' image to the
scan (sx = W / width, sy = H / height, as in #2107's bwfalse.py); a page whose scan aspect differs from Beyond
Words' image by more than 2% is left out and counted. ALTO boxes are scaled from the ALTO Page WIDTH/HEIGHT to the
scan, as segfirst.arm_l does.
Arms, per caption:
  L  ALTO String elements whose box centre is inside the caption box, in ALTO order; a HYP goes on its line's last
     String when that String is taken (segfirst.arm_l's rule). Words of one TextLine are joined by spaces, lines by
     newlines (so score2122's line-end hyphen rule applies), blocks by a blank line.
  W  Tesseract 5.5.0 `eng` at PSM 3 on the whole scan, run exactly as segfirst.arm_w (segfirst.tesseract:
     --psm 3 --dpi 300, OMP_THREAD_LIMIT=1); its TSV words (level 5, non-empty text) whose box centre is inside
     the caption box, in TSV order, joined as for L by Tesseract line and block. A Tesseract failure is recorded
     and the page's W text is empty, as in segfirst.
Measures (score2122 unchanged, each caption one article scored against its own arm text): trigram recall
(primary), unigram recall, article CER (pooled, median). Intervals: cluster bootstrap, 1,000 draws, seed 2122:
draw titles with replacement, then within each drawn title its pages with replacement; the same draws for both
arms. Decision rule: W - L trigram >= 0.02 with its 95% interval above 0, and the W - L unigram interval's lower
bound above -0.005.
Descriptive: by decade (of the issue date), and with the 10% of captions in the largest type left out. A
caption's type size is the median HEIGHT, in ALTO inch1200 units, of the L words taken for it (when L took none,
of the W words taken, scaled back to ALTO units); captions are ranked by it, largest first (ties: caption id), and
the first floor(0.1 * captions) are left out. A caption with no words in either arm has no size and is kept.
The 600 pages are refused by `run` and `score` without --measure-run (phase 2 only). Nothing is deleted or
fetched by this script; `plan` writes the fetch list.
"""
import argparse
import collections
import glob
import hashlib
import json
import multiprocessing as mp
import os
import re
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time
import xml.etree.ElementTree as ET

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import score2122  # noqa: E402

PREFIX = "2122:bw:"
N_PAGES, PER_TITLE = 600, 10
MIN_CHARS = 20
CATEGORY = "Photograph"
ASPECT_TOL = 0.02
SEED, DRAWS = 2122, 1000
LARGE_TYPE_SHARE = 0.10
TESS_VERSION = "tesseract 5.5.0"
CA_URL = re.compile(r"^https?://chroniclingamerica\.loc\.gov/lccn/(?P<lccn>[a-z0-9]+)/(?P<date>\d{4}-\d{2}-\d{2})/ed-(?P<ed>\d+)/seq-(?P<seq>\d+)/?$")
DOC = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
ALTO_UNITS = {"inch1200": 1.0, "mm10": 1200 / 254.0}


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def write_json(path, obj, indent=None):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f, sort_keys=True, indent=indent)
    os.replace(tmp, path)


def read_ids(path):
    return [ln.strip() for ln in open(path) if ln.strip() and not ln.startswith("#")]


def rank_key(doc_id):
    return hashlib.sha256((PREFIX + doc_id).encode()).hexdigest()


# ---------------------------------------------------------------- Beyond Words regions and page selection


def region_fields(r):
    """(caption, category) of a Beyond Words region in either of the file's two layouts."""
    d = r["data"]
    if "values" in d:
        if len(d["values"]) != 1:
            raise SystemExit(f"region {r['id']}: {len(d['values'])} values")
        d = d["values"][0]
    return d.get("caption") or "", d.get("category")


def path_tail(url):
    """(lccn, reel, folder, file) from a batch path .../data/<lccn>[/<reel>]/<folder>/<file>.<ext> (bwfalse.py)."""
    rest = url.split("/data/", 1)[1].split("/")
    return (rest[0], rest[1] if len(rest) == 4 else "", rest[-2], rest[-1].split(".")[0])


def scan_stem(url):
    """bwfalse.scan_stem: <batch dir>__<lccn>[__<reel>]__<folder>__<file>, the name of a scan in bw-ca-scans."""
    batchdir = url.split("/data/", 1)[0].rstrip("/").split("/")[-1]
    return "__".join(x for x in (batchdir,) + path_tail(url) if x)


def iiif_url(url):
    """bwfalse.iiif_url: the full-resolution IIIF image of the scan a Beyond Words image URL names."""
    batchdir = url.split("/data/", 1)[0].rstrip("/").split("/")[-1]
    lccn, reel, folder, file = path_tail(url)
    parts = ["service:ndnp:%s:batch_%s:data:%s" % (batchdir.split("_")[0], batchdir, lccn)] + ([reel] if reel else []) + [folder, file]
    return "https://tile.loc.gov/image-services/iiif/%s/full/full/0/default.jpg" % ":".join(parts)


def bw_pages(regions):
    """doc id -> page record with its captions (qualifying regions only), and counts of what was left out."""
    pages, st = {}, collections.Counter()
    for r in regions:
        m = CA_URL.match(r["meta_data"]["subject_url"])
        if not m:
            raise SystemExit(f"region {r['id']}: unrecognised page URL")
        doc = f"{m['lccn']}_{m['date']}_ed-{int(m['ed'])}_seq-{int(m['seq'])}"
        img = r["location"]["standard"]
        p = pages.setdefault(doc, {"alto_url": r["meta_data"]["alto"], "bw_height": r["height"], "bw_image": img,
                                   "bw_width": r["width"], "captions": [], "date": m["date"], "lccn": m["lccn"],
                                   "iiif_url": iiif_url(img), "stem": scan_stem(img)})
        if (p["bw_image"], p["bw_width"], p["bw_height"]) != (img, r["width"], r["height"]):
            raise SystemExit(f"{doc}: regions disagree on Beyond Words' image")
        st["regions"] += 1
        caption, cat = region_fields(r)
        if cat != CATEGORY:
            st["not photograph"] += 1
            continue
        if len(score2122.normalise(caption)) < MIN_CHARS:
            st["photograph, caption under 20 normalised chars"] += 1
            continue
        g = r["region"]
        p["captions"].append({"box_bw": [g["x"], g["y"], g["x"] + g["width"], g["y"] + g["height"]],
                              "caption": caption, "id": r["id"]})
        st["captions"] += 1
    for p in pages.values():
        p["captions"].sort(key=lambda c: c["id"])
    return pages, st


def select(pages):
    """Ranked qualifying pages; the first N_PAGES with at most PER_TITLE per title are selected."""
    ranked = sorted((d for d in pages if pages[d]["captions"]), key=lambda d: (rank_key(d), d))
    per, chosen, out = collections.Counter(), [], []
    for i, d in enumerate(ranked):
        take = len(chosen) < N_PAGES and per[pages[d]["lccn"]] < PER_TITLE
        if take:
            per[pages[d]["lccn"]] += 1
            chosen.append(d)
        out.append({"doc_id": d, "rank": i, "rank_key": rank_key(d), "selected": take})
    return chosen, out


def spread(pages, ids):
    titles = collections.Counter(pages[d]["lccn"] for d in ids)
    return {"captions": sum(len(pages[d]["captions"]) for d in ids), "decades": dict(collections.Counter(pages[d]["date"][:3] + "0s" for d in ids)),
            "pages": len(ids), "pages_per_title": dict(sorted(collections.Counter(titles.values()).items())),
            "titles": len(titles), "years": dict(sorted(collections.Counter(pages[d]["date"][:4] for d in ids).items()))}


def cmd_select(a):
    regions = json.load(open(a.bw))["data"]
    pages, st = bw_pages(regions)
    chosen, ranking = select(pages)
    os.makedirs(a.out_dir, exist_ok=True)
    write_json(os.path.join(a.out_dir, "pages.json"), {d: dict(pages[d], rank=i) for i, d in enumerate(chosen)}, indent=0)
    listing = os.path.join(a.out_dir, "pages-600.txt")
    with open(listing, "w") as f:
        f.write("".join(d + "\n" for d in chosen))
    qual = [r["doc_id"] for r in ranking]
    write_json(os.path.join(a.out_dir, "ranking.json"), {"pages": {d: pages[d] for d in qual}, "ranking": ranking}, indent=0)
    res = {"bw_sha256": sha256_file(a.bw), "counts": dict(st), "pages_600_txt_sha256": sha256_file(listing),
           "pages_json_sha256": sha256_file(os.path.join(a.out_dir, "pages.json")),
           "qualifying": spread(pages, qual), "selected": spread(pages, chosen),
           "skipped_for_title_cap": sum(1 for r in ranking[: ranking.index(next(r for r in ranking if r["doc_id"] == chosen[-1])) + 1] if not r["selected"]),
           "all_pages": len(pages)}
    write_json(os.path.join(a.out_dir, "select.json"), res, indent=1)
    print(json.dumps(res, indent=1, sort_keys=True))
    return 0


def cmd_check_pages(a):
    """N check pages from the far end of the ranking, outside the selected pages, whose scan is held."""
    rk = json.load(open(a.ranking))
    held = set(read_ids(a.held))
    out = [r["doc_id"] for r in reversed(rk["ranking"]) if not r["selected"] and r["doc_id"] in held][: a.n]
    with open(a.out, "w") as f:
        f.write("".join(d + "\n" for d in out))
    print(json.dumps({"check_pages": out, "ranks": {r["doc_id"]: r["rank"] for r in rk["ranking"] if r["doc_id"] in out}}, indent=1))
    return 0


# ---------------------------------------------------------------- fetch plan


def held_rel(doc_id, batch):
    m = DOC.match(doc_id)
    return f"{batch}/{m['lccn']}/{m['date']}/{int(m['ed'])}/{int(m['seq'])}"


def listing_sizes(paths):
    """path -> bytes from `ls -lL`-style listings (the last field a path, the fifth the size)."""
    out = {}
    for lf in paths or []:
        for ln in open(lf):
            p = ln.split()
            if len(p) >= 9 and p[4].isdigit():
                out[p[-1]] = int(p[4])
    return out


def cmd_plan(a):
    import pyarrow.parquet as pq
    pages = json.load(open(a.pages))
    want = set(pages)
    idx = collections.defaultdict(list)
    with open(a.index) as f:
        for ln in f:
            r = json.loads(ln)
            if r["doc_id"] in want:
                idx[r["doc_id"]].append(r)
    held = listing_sizes(a.held)          # held corpus scans: /…/scans/{keep,buffer}/<batch>/<lccn>/<date>/<ed>/<seq>.(jpg|jp2)
    store = listing_sizes(a.store_listing)  # bw-ca-scans and other stores: <stem>.jpg
    held_by_rel = {}
    for p, b in held.items():
        rel = re.sub(r"^.*/scans/(keep|buffer)/", "", p)
        held_by_rel[os.path.splitext(rel)[0]] = (p, b)
    store_by_stem = {os.path.splitext(os.path.basename(p))[0]: (p, b) for p, b in store.items() if p.endswith((".jpg", ".jp2"))}
    locstats = {}
    for b in sorted({r["batch"] for rs in idx.values() for r in rs} | {pages[d]["stem"].split("__")[0].rsplit("_ver", 1)[0] for d in pages}):
        for pth in sorted(glob.glob(os.path.join(a.locstats_dir, f"{b}_ver*.parquet"))):
            for r in pq.read_table(pth, columns=["doc_id", "archive"]).to_pylist():
                if r["doc_id"] in want:
                    locstats.setdefault(r["doc_id"], set()).add(r["archive"])
    rows, st = [], collections.Counter()
    for d in sorted(pages, key=lambda d: pages[d]["rank"]):
        p = pages[d]
        src = None
        for r in idx.get(d, []):
            if r.get("resolution", "full") == "full" and held_rel(d, r["batch"]) in held_by_rel:
                src = {"from": "corpus", "path": held_by_rel[held_rel(d, r["batch"])][0], "bytes": held_by_rel[held_rel(d, r["batch"])][1]}
                break
        if src is None and p["stem"] in store_by_stem:
            src = {"from": "store", "path": store_by_stem[p["stem"]][0], "bytes": store_by_stem[p["stem"]][1]}
        if src is None:
            src = {"from": "fetch", "url": p["iiif_url"], "stem": p["stem"]}
        st["scan " + src["from"]] += 1
        arch = sorted(locstats.get(d, []))
        alto = {"from": "locbulk", "archive": arch[-1]} if arch else {"from": "fetch", "url": p["alto_url"]}
        st["alto " + alto["from"]] += 1
        rows.append({"alto": alto, "doc_id": d, "scan": src})
    held_b = [r["scan"]["bytes"] for r in rows if r["scan"]["from"] != "fetch"]
    store_jpg = [b for _, b in store_by_stem.values()]
    mean = float(np.mean(store_jpg)) if store_jpg else None
    n_fetch = st["scan fetch"]
    res = {"counts": dict(st), "held_scan_bytes": int(sum(held_b)), "store_jpg_mean_bytes": mean, "store_jpg_scans": len(store_jpg),
           "estimated_fetch_bytes": round(mean * n_fetch) if mean else None, "pages": len(rows),
           "alto_archives": dict(collections.Counter(r["alto"]["archive"] for r in rows if r["alto"]["from"] == "locbulk")),
           "rows": rows}
    write_json(a.out, res, indent=1)
    print(json.dumps({k: v for k, v in res.items() if k not in ("rows", "alto_archives")} | {"alto_archive_count": len(res["alto_archives"])}, indent=1, sort_keys=True))
    return 0


# ---------------------------------------------------------------- ALTO from the bulk archive


def member_doc(name):
    """doc id of an archive member <lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml, else None (alto_weak.py)."""
    p = name.split("/")
    if len(p) != 7 or p[6] != "ocr.xml" or not p[4].startswith("ed-") or not p[5].startswith("seq-"):
        return None
    try:
        return f"{p[0]}_{p[1]}-{p[2]}-{p[3]}_ed-{int(p[4][3:])}_seq-{int(p[5][4:])}"
    except ValueError:
        return None


def _archive_job(job):
    tar_path, wanted, alto_dir = job
    want = {d for d in wanted if not os.path.exists(os.path.join(alto_dir, d + ".xml"))}
    got = len(wanted) - len(want)
    if want:
        with tarfile.open(tar_path, "r|bz2") as tf:
            for m in tf:
                d = member_doc(m.name)
                if d not in want:
                    continue
                data = tf.extractfile(m).read()
                p = os.path.join(alto_dir, d + ".xml")
                with open(p + ".part", "wb") as f:
                    f.write(data)
                os.replace(p + ".part", p)
                with open(p + ".json", "w") as f:
                    json.dump({"archive": os.path.basename(tar_path), "member": m.name, "sha256": hashlib.sha256(data).hexdigest()}, f, sort_keys=True)
                want.discard(d)
                got += 1
                if not want:
                    break
    return os.path.basename(tar_path), got, sorted(want)


def cmd_alto(a):
    """ALTO of the given pages from their batch's bulk archive, named by the locstats index; each archive is
    streamed once, stopping at its last wanted page."""
    import pyarrow.parquet as pq
    pages = json.load(open(a.pages))
    pages = pages.get("pages", pages)
    ids = read_ids(a.ids_file)
    refuse_measure(ids, a)
    os.makedirs(a.alto_dir, exist_ok=True)
    jobs = collections.defaultdict(list)
    missing = []
    for d in ids:
        batch = pages[d]["stem"].split("__")[0].rsplit("_ver", 1)[0]
        arch = set()
        for pth in sorted(glob.glob(os.path.join(a.locstats_dir, f"{batch}_ver*.parquet"))):
            arch |= {r["archive"] for r in pq.read_table(pth, columns=["doc_id", "archive"]).to_pylist() if r["doc_id"] == d}
        if not arch:
            missing.append(d)
            continue
        jobs[os.path.join(a.locbulk_dir, sorted(arch)[-1])].append(d)
    with mp.Pool(a.workers) as pool:
        for name, got, left in pool.imap_unordered(_archive_job, [(k, v, a.alto_dir) for k, v in sorted(jobs.items())]):
            print(json.dumps({"archive": name, "got": got, "not_found": left}), flush=True)
    print(json.dumps({"not_in_locstats": missing}))
    return 0


# ---------------------------------------------------------------- arms


def refuse_measure(ids, a):
    """The test pages are phase 2 only."""
    if getattr(a, "measure_run", False):
        return
    listing = os.path.join(os.path.dirname(os.path.abspath(a.pages)), "pages-600.txt")
    if not os.path.exists(listing):
        raise SystemExit(f"{listing} missing: keep the pages file next to the test page list")
    if set(read_ids(listing)) & set(ids):
        raise SystemExit("test pages among the ids: phase 2 only (--measure-run)")


def _local(tag):
    return tag.rsplit("}", 1)[-1]


def alto_words(path, size):
    """ALTO String elements in document order: {text, box (scan px), height_alto, line, block}; HYP on a line's
    last String. Also the page's ALTO size and unit."""
    root = ET.parse(path).getroot()
    unit = next((e.text.strip() for e in root.iter() if _local(e.tag) == "MeasurementUnit" and e.text), None)
    pg = next(e for e in root.iter() if _local(e.tag) == "Page")
    pw, ph = float(pg.get("WIDTH")), float(pg.get("HEIGHT"))
    sx, sy = size[0] / pw, size[1] / ph
    words, li, bi = [], 0, 0
    for tb in root.iter():
        if _local(tb.tag) != "TextBlock":
            continue
        bi += 1
        for tl in tb:
            if _local(tl.tag) != "TextLine":
                continue
            li += 1
            last = None
            for el in tl:
                t = _local(el.tag)
                if t == "String" and el.get("CONTENT"):
                    try:
                        x, y, w, h = (float(el.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
                    except (TypeError, ValueError):
                        last = None
                        continue
                    last = {"block": bi, "box": [x * sx, y * sy, (x + w) * sx, (y + h) * sy], "height_alto": h,
                            "line": li, "text": el.get("CONTENT")}
                    words.append(last)
                elif t == "HYP" and last is not None:
                    last["text"] += el.get("CONTENT") or "-"
    return words, {"height": ph, "unit": unit, "width": pw}


def tess_words(tsv):
    """Tesseract TSV words (level 5, non-empty text) in output order: {text, box, line, block}."""
    out, keys = [], {}
    for ln in tsv.splitlines()[1:]:
        f = ln.split("\t")
        if len(f) < 12 or f[0] != "5" or not f[11].strip():
            continue
        x, y, w, h = (int(v) for v in f[6:10])
        lk = keys.setdefault((f[1], f[2], f[3], f[4]), len(keys))
        out.append({"block": (f[1], f[2]), "box": [x, y, x + w, y + h], "line": lk, "text": f[11]})
    return out


def inside(word, box):
    cx, cy = (word["box"][0] + word["box"][2]) / 2.0, (word["box"][1] + word["box"][3]) / 2.0
    return box[0] <= cx <= box[2] and box[1] <= cy <= box[3]


def join_words(words):
    """Words joined by spaces within a line, newlines between lines, a blank line between blocks."""
    out, prev = [], None
    for w in words:
        if prev is not None:
            out.append(" " if w["line"] == prev["line"] else ("\n" if w["block"] == prev["block"] else "\n\n"))
        out.append(w["text"])
        prev = w
    return "".join(out)


def scan_box(page, size):
    sx, sy = size[0] / page["bw_width"], size[1] / page["bw_height"]
    return sx, sy, [[c["box_bw"][0] * sx, c["box_bw"][1] * sy, c["box_bw"][2] * sx, c["box_bw"][3] * sy] for c in page["captions"]]


def image_size(path):
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    with Image.open(path) as im:
        return im.size


def arm_l_page(page, alto_path, size):
    words, meta = alto_words(alto_path, size)
    _, _, boxes = scan_box(page, size)
    caps = []
    for c, box in zip(page["captions"], boxes):
        sel = [w for w in words if inside(w, box)]
        caps.append({"heights_alto": [w["height_alto"] for w in sel], "id": c["id"], "text": join_words(sel),
                     "words": [[round(v, 1) for v in w["box"]] for w in sel]})
    return {"alto": meta, "captions": caps, "errors": [], "page_words": len(words)}


def arm_w_page(page, image_path, size, workdir):
    import segfirst
    try:
        segfirst.tesseract(image_path, segfirst.PSM_W, workdir)
    except subprocess.CalledProcessError as e:
        return {"captions": [{"heights_px": [], "id": c["id"], "text": "", "words": []} for c in page["captions"]],
                "errors": [segfirst.engine_error(e)], "page_words": 0, "tsv": ""}
    tsv = open(os.path.join(workdir, "out.tsv"), encoding="utf-8").read()
    words = tess_words(tsv)
    _, _, boxes = scan_box(page, size)
    caps = []
    for c, box in zip(page["captions"], boxes):
        sel = [w for w in words if inside(w, box)]
        caps.append({"heights_px": [w["box"][3] - w["box"][1] for w in sel], "id": c["id"], "text": join_words(sel),
                     "words": [w["box"] for w in sel]})
    return {"captions": caps, "errors": [], "page_words": len(words), "tsv": tsv}


def _job(args):
    pid, arm, page, scan, alto_path, out_dir = args
    path = os.path.join(out_dir, arm, pid + ".json")
    if os.path.exists(path):
        return pid, arm, None
    t0 = time.time()
    size = image_size(scan)
    sx, sy, boxes = scan_box(page, size)
    base = {"aspect_ok": abs(sx / sy - 1) <= ASPECT_TOL, "boxes": [[round(v, 1) for v in b] for b in boxes],
            "scale": [sx, sy], "scan": os.path.basename(scan), "size": list(size)}
    if not base["aspect_ok"]:
        res = dict(base, skipped="aspect")
    elif arm == "L":
        res = dict(base, **arm_l_page(page, alto_path, size))
    else:
        with tempfile.TemporaryDirectory(prefix="bwcap-") as wd:
            res = dict(base, **arm_w_page(page, scan, size, wd))
    res["seconds"] = round(time.time() - t0, 1)
    write_json(path, res)
    return pid, arm, res["seconds"]


def run_meta(a, arms):
    v = subprocess.run(["tesseract", "--version"], capture_output=True, text=True)
    version = (v.stdout + v.stderr).splitlines()[0].strip()
    if "W" in arms and version != TESS_VERSION:
        raise SystemExit(f"{version}: the plan needs {TESS_VERSION}")
    import segfirst
    m = segfirst.run_meta(a.pages, arms)
    m.update({"bwcap_sha256": sha256_file(__file__), "ids_sha256": sha256_file(a.ids_file),
              "score2122_sha256": sha256_file(os.path.join(HERE, "score2122.py")), "segfirst_sha256": sha256_file(os.path.join(HERE, "segfirst.py"))})
    return m


def cmd_run(a):
    pages = json.load(open(a.pages))
    pages = pages.get("pages", pages)
    ids = read_ids(a.ids_file)
    refuse_measure(ids, a)
    scans = dict(ln.split("\t") for ln in (x.rstrip("\n") for x in open(a.scans)) if ln)
    arms = [x for x in a.arms.split(",") if x]
    if any(x not in ("L", "W") for x in arms):
        raise SystemExit("arms are L and W")
    for d in arms:
        os.makedirs(os.path.join(a.out_dir, d), exist_ok=True)
    meta = run_meta(a, arms)
    print(json.dumps(meta, sort_keys=True), flush=True)
    write_json(os.path.join(a.out_dir, f"run-{time.strftime('%Y%m%dT%H%M%S')}.json"), meta)
    jobs = []
    for pid in ids:
        if pid not in scans:
            raise SystemExit(f"{pid}: no scan")
        alto = os.path.join(a.alto_dir, pid + ".xml")
        for arm in arms:
            if arm == "L" and not os.path.exists(alto):
                raise SystemExit(f"{pid}: no ALTO")
            jobs.append((pid, arm, pages[pid], scans[pid], alto, a.out_dir))
    t0 = time.time()
    with mp.Pool(a.workers) as pool:
        for n, (pid, arm, s) in enumerate(pool.imap_unordered(_job, jobs), 1):
            print(f"arms {n}/{len(jobs)} {arm} {pid} {s}s {time.time() - t0:.0f}s", flush=True)
    return 0


# ---------------------------------------------------------------- scoring


def caption_rows(pages, ids, run_dir):
    """One row per caption: page, title, decade, truth, the two arms' texts and counts, and its type size."""
    rows, skipped = [], []
    for pid in ids:
        L = json.load(open(os.path.join(run_dir, "L", pid + ".json")))
        W = json.load(open(os.path.join(run_dir, "W", pid + ".json")))
        if L.get("skipped") or W.get("skipped"):
            skipped.append(pid)
            continue
        unit = ALTO_UNITS.get(L["alto"]["unit"])     # ALTO units -> inch1200; None: size unknown
        to_alto_y = L["alto"]["height"] / W["size"][1]
        for c, cl, cw in zip(pages[pid]["captions"], L["captions"], W["captions"]):
            assert c["id"] == cl["id"] == cw["id"]
            art = [{"id": c["id"], "text": c["caption"]}]
            r = {"decade": pages[pid]["date"][:3] + "0s", "id": c["id"], "page": pid, "title": pages[pid]["lccn"]}
            for arm, res in (("L", cl), ("W", cw)):
                cnt, cer = score2122.page_counts(art, res["text"])
                r[arm] = cnt + [cer[0]["edits"], cer[0]["chars"]]
            hs = cl["heights_alto"] or [h * to_alto_y for h in cw["heights_px"]]
            r["type_size"] = statistics.median(hs) * unit if hs and unit else None
            r["type_from"] = "L" if cl["heights_alto"] else ("W" if cw["heights_px"] else None)
            rows.append(r)
    return rows, skipped


def large_type(rows):
    """Ids of the floor(10%) of captions with the largest type (ties by id); captions with no size are kept."""
    k = int(LARGE_TYPE_SHARE * len(rows))
    sized = sorted((r for r in rows if r["type_size"] is not None), key=lambda r: (-r["type_size"], r["id"]))
    return {r["id"] for r in sized[:k]}


def ratios(s):
    return s[0] / s[1], s[2] / s[3], s[4] / s[5]


def cluster_bootstrap(rows):
    """Titles with replacement, then pages within each drawn title with replacement; paired arms."""
    rng = np.random.default_rng(SEED)
    by_page = collections.defaultdict(lambda: {"L": np.zeros(6), "W": np.zeros(6)})
    for r in rows:
        for arm in ("L", "W"):
            by_page[(r["title"], r["page"])][arm] += np.array(r[arm], dtype=np.float64)
    titles = sorted({t for t, _ in by_page})
    pages_of = {t: sorted(p for tt, p in by_page if tt == t) for t in titles}
    arr = {t: {arm: np.array([by_page[(t, p)][arm] for p in pages_of[t]]) for arm in ("L", "W")} for t in titles}
    draws = {"L": [], "W": []}
    for _ in range(DRAWS):
        tot = {"L": np.zeros(6), "W": np.zeros(6)}
        for ti in rng.integers(0, len(titles), len(titles)):
            t = titles[ti]
            pi = rng.integers(0, len(pages_of[t]), len(pages_of[t]))
            for arm in ("L", "W"):
                tot[arm] += arr[t][arm][pi].sum(axis=0)
        for arm in ("L", "W"):
            draws[arm].append(ratios(tot[arm]))
    L, W = np.array(draws["L"]), np.array(draws["W"])
    names = ("trigram_recall", "unigram_recall", "cer_pooled")
    out = {arm: {n: score2122.interval(v[:, i]) for i, n in enumerate(names)} for arm, v in (("L", L), ("W", W))}
    out["W-L"] = {n: score2122.interval(W[:, i] - L[:, i]) for i, n in enumerate(names)}
    return out


def summarise(rows):
    out = {"captions": len(rows), "pages": len({r["page"] for r in rows}), "titles": len({r["title"] for r in rows})}
    for arm in ("L", "W"):
        s = np.sum([r[arm] for r in rows], axis=0) if rows else np.zeros(6)
        cers = [r[arm][4] / r[arm][5] for r in rows if r[arm][5]]
        out[arm] = {"article_cer_median": float(np.median(cers)) if cers else None, "article_cer_pooled": float(s[4] / s[5]) if s[5] else None,
                    "trigram_recall": float(s[0] / s[1]) if s[1] else None, "trigrams": int(s[1]),
                    "unigram_recall": float(s[2] / s[3]) if s[3] else None, "unigrams": int(s[3])}
    if rows and out["L"]["trigram_recall"] is not None:
        out["W-L"] = {k: out["W"][k] - out["L"][k] for k in ("trigram_recall", "unigram_recall", "article_cer_pooled")}
    return out


def score(pages, ids, run_dir):
    rows, skipped = caption_rows(pages, ids, run_dir)
    res = {"all": summarise(rows), "pages_skipped_aspect": skipped,
           "settings": {"bootstrap": "titles, then pages within title", "draws": DRAWS, "seed": SEED, "large_type_share": LARGE_TYPE_SHARE}}
    res["intervals"] = cluster_bootstrap(rows) if rows else {}
    if rows:
        d3, d1 = res["all"]["W-L"]["trigram_recall"], res["all"]["W-L"]["unigram_recall"]
        iv = res["intervals"]["W-L"]
        rule = {"trigram_W_minus_L": d3, "trigram_ge_0.02": bool(d3 >= 0.02), "trigram_interval_above_0": bool(iv["trigram_recall"][0] > 0),
                "unigram_W_minus_L": d1, "unigram_interval_lower_above_-0.005": bool(iv["unigram_recall"][0] > -0.005)}
        rule["met"] = rule["trigram_ge_0.02"] and rule["trigram_interval_above_0"] and rule["unigram_interval_lower_above_-0.005"]
        res["rule"] = rule
    big = large_type(rows)
    res["descriptive"] = {"by_decade": {d: summarise([r for r in rows if r["decade"] == d]) for d in sorted({r["decade"] for r in rows})},
                          "without_largest_type": dict(summarise([r for r in rows if r["id"] not in big]), left_out=len(big)),
                          "type_size_from": dict(collections.Counter(str(r["type_from"]) for r in rows))}
    res["captions"] = rows
    return res


def cmd_score(a):
    pages = json.load(open(a.pages))
    pages = pages.get("pages", pages)
    ids = read_ids(a.ids_file)
    refuse_measure(ids, a)
    res = score(pages, ids, a.run_dir)
    res["inputs"] = {"bwcap_sha256": sha256_file(__file__), "ids_sha256": sha256_file(a.ids_file), "pages_sha256": sha256_file(a.pages),
                     "score2122_sha256": sha256_file(os.path.join(HERE, "score2122.py"))}
    with open(a.out, "x" if a.measure_run else "w") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    print(json.dumps({k: v for k, v in res.items() if k != "captions"}, indent=1, sort_keys=True))
    return 0


# ---------------------------------------------------------------- overlay


def cmd_overlay(a):
    """The page with each caption box (blue) and the words each arm took (L red, W green), the box's area enlarged."""
    from PIL import Image, ImageDraw
    Image.MAX_IMAGE_PIXELS = None
    L = json.load(open(os.path.join(a.run_dir, "L", a.page + ".json")))
    W = json.load(open(os.path.join(a.run_dir, "W", a.page + ".json")))
    scans = dict(ln.rstrip("\n").split("\t") for ln in open(a.scans) if ln.strip())
    im = Image.open(scans[a.page]).convert("RGB")
    x0 = min(b[0] for b in L["boxes"]); y0 = min(b[1] for b in L["boxes"])
    x1 = max(b[2] for b in L["boxes"]); y1 = max(b[3] for b in L["boxes"])
    pad = 0.05 * im.size[0]
    crop = [max(0, x0 - pad), max(0, y0 - pad), min(im.size[0], x1 + pad), min(im.size[1], y1 + pad)]
    im = im.crop([round(v) for v in crop])
    s = min(1.0, a.width / im.size[0])
    im = im.resize((round(im.size[0] * s), round(im.size[1] * s)), Image.LANCZOS)
    dr = ImageDraw.Draw(im, "RGBA")
    t = lambda b: [(b[0] - crop[0]) * s, (b[1] - crop[1]) * s, (b[2] - crop[0]) * s, (b[3] - crop[1]) * s]  # noqa: E731
    for b in L["boxes"]:
        dr.rectangle(t(b), outline=(0, 80, 255, 255), width=4)
    for c in L["captions"]:
        for b in c["words"]:
            dr.rectangle(t(b), outline=(230, 0, 0, 220), width=2)
    for c in W["captions"]:
        for b in c["words"]:
            dr.rectangle(t(b), outline=(0, 170, 0, 200), width=1)
    dr.text((8, 8), "blue: Beyond Words box   red: L (ALTO) words taken   green: W (Tesseract) words taken", fill=(0, 0, 0, 255))
    im.save(a.out)
    print(a.out, file=sys.stderr)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("select")
    s.add_argument("--bw", required=True)
    s.add_argument("--out-dir", required=True)
    c = sp.add_parser("check-pages")
    c.add_argument("--ranking", required=True)
    c.add_argument("--held", required=True, help="doc ids whose scan is held, one per line")
    c.add_argument("--n", type=int, required=True)
    c.add_argument("--out", required=True)
    p = sp.add_parser("plan")
    p.add_argument("--pages", required=True)
    p.add_argument("--index", required=True)
    p.add_argument("--held", action="append", required=True, help="ls -lL listing of held corpus scans")
    p.add_argument("--store-listing", action="append", help="ls -lL listing of a scan store (bw-ca-scans)")
    p.add_argument("--locstats-dir", required=True)
    p.add_argument("--out", required=True)
    al = sp.add_parser("alto")
    al.add_argument("--pages", required=True)
    al.add_argument("--ids-file", required=True)
    al.add_argument("--locstats-dir", required=True)
    al.add_argument("--locbulk-dir", required=True)
    al.add_argument("--alto-dir", required=True)
    al.add_argument("--workers", type=int, default=2)
    al.add_argument("--measure-run", action="store_true")
    r = sp.add_parser("run")
    r.add_argument("--pages", required=True)
    r.add_argument("--ids-file", required=True)
    r.add_argument("--scans", required=True, help="TSV: doc id, scan path")
    r.add_argument("--alto-dir", required=True)
    r.add_argument("--out-dir", required=True)
    r.add_argument("--arms", default="L,W")
    r.add_argument("--workers", type=int, default=3)
    r.add_argument("--measure-run", action="store_true")
    sc = sp.add_parser("score")
    sc.add_argument("--pages", required=True)
    sc.add_argument("--ids-file", required=True)
    sc.add_argument("--run-dir", required=True)
    sc.add_argument("--out", required=True)
    sc.add_argument("--measure-run", action="store_true")
    o = sp.add_parser("overlay")
    o.add_argument("--run-dir", required=True)
    o.add_argument("--scans", required=True)
    o.add_argument("--page", required=True)
    o.add_argument("--width", type=int, default=1800)
    o.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    return {"select": cmd_select, "check-pages": cmd_check_pages, "plan": cmd_plan, "alto": cmd_alto, "run": cmd_run,
            "score": cmd_score, "overlay": cmd_overlay}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
