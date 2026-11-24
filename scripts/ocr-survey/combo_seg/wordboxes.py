#!/usr/bin/env python3
"""Word and text-line boxes for the combined segmenter's "how much text does this line cut" features (#2107).

Two sources, both written as --out-dir/<page id with "/" as "__">.json = {"source", "size": [W, H],
"words": [[x0, y0, x1, y1, conf]], "lines": [[x0, y0, x1, y1]]} in the page's original pixels:
  - Tesseract TSV (datasets without word boxes): the page cache's work-resolution array is written as a PNG and
    read once with `tesseract - - --psm 3 -l LANG tsv`; level-5 rows with conf >= 0 are words, level-4 rows lines.
  - LoC ALTO (Chronicling America pages): String elements are words, TextLine elements lines; coordinates are
    scaled from the ALTO Page's WIDTH/HEIGHT to the image size given in the map file.

    wordboxes.py --page-cache DIR --pages-from refs.json --out-dir DIR [--lang fra] [--workers 2]
    wordboxes.py --alto-map map.json --out-dir DIR     (map: page id -> {"alto": path, "width": W, "height": H})
"""
import argparse
import json
import multiprocessing as mp
import os
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))


def out_name(pid):
    return pid.replace("/", "__") + ".json"


def parse_tsv(text, sx, sy):
    words, lines_ = [], []
    for i, ln in enumerate(text.splitlines()):
        p = ln.split("\t")
        if i == 0 or len(p) < 12:
            continue
        try:
            level, left, top, w, h, conf = int(p[0]), int(p[6]), int(p[7]), int(p[8]), int(p[9]), float(p[10])
        except ValueError:
            continue
        box = [round(left * sx, 1), round(top * sy, 1), round((left + w) * sx, 1), round((top + h) * sy, 1)]
        if level == 5 and conf >= 0 and p[11].strip():
            words.append(box + [round(conf, 1)])
        elif level == 4:
            lines_.append(box)
    return words, lines_


def _tess(args):
    pid, cache, out_dir, lang, psm = args
    import columns
    from PIL import Image
    gray, size = columns.load_cached(cache, pid)
    with tempfile.NamedTemporaryFile(suffix=".png") as tf:
        Image.fromarray(gray).save(tf.name)
        r = subprocess.run(["tesseract", tf.name, "-", "--psm", str(psm), "-l", lang, "tsv"],
                           capture_output=True, text=True, env=dict(os.environ, OMP_THREAD_LIMIT="1"))
    if r.returncode != 0:
        return pid, "tesseract failed: " + r.stderr[-300:]
    sx, sy = size[0] / gray.shape[1], size[1] / gray.shape[0]
    words, lines_ = parse_tsv(r.stdout, sx, sy)
    json.dump({"source": "tesseract psm %d lang %s on the %d px page cache" % (psm, lang, gray.shape[1]),
               "size": [size[0], size[1]], "words": words, "lines": lines_},
              open(os.path.join(out_dir, out_name(pid)), "w"))
    return pid, None


def alto_boxes(path, width, height):
    """Words and lines from an ALTO file, scaled to an image of (width, height)."""
    root = ET.parse(path).getroot()
    tag = lambda e: e.tag.split("}")[-1]  # noqa: E731
    page = next((e for e in root.iter() if tag(e) == "Page"), None)
    if page is None:
        raise ValueError("no Page element in %s" % path)
    pw, ph = float(page.get("WIDTH")), float(page.get("HEIGHT"))
    sx, sy = width / pw, height / ph

    def box(e):
        x, y, w, h = (float(e.get(k, 0)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
        return [round(x * sx, 1), round(y * sy, 1), round((x + w) * sx, 1), round((y + h) * sy, 1)]
    words, lines_ = [], []
    for e in root.iter():
        t = tag(e)
        if t == "String":
            words.append(box(e) + [float(e.get("WC", 1.0)) * 100])
        elif t == "TextLine":
            lines_.append(box(e))
    return words, lines_


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--page-cache")
    ap.add_argument("--pages-from")
    ap.add_argument("--alto-map")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--lang", default="eng")
    ap.add_argument("--psm", type=int, default=3)
    ap.add_argument("--workers", type=int, default=2)
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    if a.alto_map:
        m = json.load(open(a.alto_map))
        for pid, v in sorted(m.items()):
            words, lines_ = alto_boxes(v["alto"], v["width"], v["height"])
            json.dump({"source": "LoC ALTO " + os.path.basename(v["alto"]), "size": [v["width"], v["height"]],
                       "words": words, "lines": lines_}, open(os.path.join(a.out_dir, out_name(pid)), "w"))
        print("pages %d/%d" % (len(m), len(m)))
        return 0
    if not (a.page_cache and a.pages_from):
        raise SystemExit("give --page-cache with --pages-from, or --alto-map")
    ids = sorted(json.load(open(a.pages_from))["pages"]) if a.pages_from.endswith(".json") else \
        sorted(ln.strip() for ln in open(a.pages_from) if ln.strip())
    jobs = [(p, a.page_cache, a.out_dir, a.lang, a.psm) for p in ids if not os.path.exists(os.path.join(a.out_dir, out_name(p)))]
    t0, bad = time.time(), []
    with mp.Pool(a.workers) as pool:
        for n, (pid, err) in enumerate(pool.imap_unordered(_tess, jobs), 1):
            if err:
                bad.append((pid, err))
            if n % 10 == 0 or n == len(jobs):
                print("pages %d/%d %.0fs" % (n, len(jobs), time.time() - t0), flush=True)
    for pid, err in bad:
        print("FAILED", pid, err, flush=True)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
