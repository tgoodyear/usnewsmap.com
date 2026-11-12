"""Line finders for pages without ALTO (#1622). Each writes one JSON line per page: {"page", "lines": [{"box"}]}
with boxes in the scan's pixels, for `chr_layout_eval.py score`.

  tess    --psm N [--lang eng]   Tesseract's own layout: the TSV's line rows (the way the live page jobs
                                 find lines). Lines it read as nothing are not in the TSV.
  kraken  [--model PATH]         Kraken's baseline segmenter (`blla`, the model in its wheel), the line's
                                 boundary polygon as a box. It scales its input to 1,800 pixels high.
  kraken-tiles [--tile-h 1800]   The same on tiles of the page at full size (overlapping by 150 pixels, each
                                 line kept from the tile its centre is deepest in), so the print stays at the
                                 size the model expects.

Every Tesseract run can take a flattened scan (`--prep flat`: the paper's brightness divided out with a large
local estimate, then the grey levels stretched) or a binarised one (`--prep sauvola`), since GHN's scans are
darker and lower in contrast than LoC's.

`--shrink N` reduces the scan N times first (boxes come back in full-size pixels).

Common: --scans <dir of <doc_id>.jpg> --truth <truth.jsonl, for the page list> [--limit N] [--jobs N] --out FILE.
Pages already in --out are skipped, so a run can be resumed.
"""
import argparse
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def doc_id(page):
    return page.replace("/", "_")


def prepare(scan, prep, tmp, shrink=1, window=51, k=0.3):
    """The scan, flattened or binarised, written under `tmp`; the original path for `none`."""
    if prep == "none" and shrink == 1:
        return str(scan)
    import numpy as np
    from PIL import Image
    from scipy import ndimage
    Image.MAX_IMAGE_PIXELS = None
    img = Image.open(scan).convert("L")
    if shrink > 1:
        img = img.reduce(shrink)
    if prep == "none":
        out = f"{tmp}/prep.png"
        img.save(out)
        return out
    g = np.asarray(img, dtype=np.float32)
    small = np.asarray(img.reduce(8), dtype=np.float32)
    # the paper level: the brightest nearby pixel (ink is darker), smoothed, and brought back to full size
    bg = ndimage.gaussian_filter(ndimage.maximum_filter(small, size=9), 4)
    bg = np.asarray(Image.fromarray(bg).resize(img.size, Image.BILINEAR), dtype=np.float32)
    flat = np.clip(g / np.maximum(bg, 1.0), 0, 1.2)
    lo, hi = np.percentile(flat[::4, ::4], [0.5, 99.5])
    flat = np.clip((flat - lo) / max(hi - lo, 1e-3), 0, 1)
    if prep == "sauvola":
        w = window
        mean = ndimage.uniform_filter(flat, w)
        sq = ndimage.uniform_filter(flat * flat, w)
        std = np.sqrt(np.maximum(sq - mean * mean, 0))
        flat = (flat > mean * (1 + k * (std / 0.5 - 1))).astype(np.float32)
    out = f"{tmp}/prep.png"
    Image.fromarray((flat * 255).astype(np.uint8)).save(out)
    return out


def tess_lines(scan, psm, lang, tessdata=None, prep="none", shrink=1, window=51, k=0.3):
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, OMP_THREAD_LIMIT="1")
        extra = ["--tessdata-dir", tessdata] if tessdata else []
        scan = prepare(scan, prep, tmp, shrink, window, k)
        subprocess.run(["tesseract", str(scan), f"{tmp}/o", "--psm", str(psm), "-l", lang, "--dpi", "300", *extra, "-c", "tessedit_create_tsv=1"],
                       check=True, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        rows = [l.rstrip("\n").split("\t") for l in open(f"{tmp}/o.tsv", encoding="utf-8")][1:]
    out = []
    for r in rows:
        if r[0] == "4":
            x, y, w, h = (int(v) * shrink for v in r[6:10])
            out.append({"box": [x, y, x + w, y + h]})
    return out


def kraken_lines(scan, model):
    from PIL import Image
    from kraken.configs import SegmentationInferenceConfig
    from kraken.tasks import SegmentationTaskModel
    Image.MAX_IMAGE_PIXELS = None
    seg = model.predict(Image.open(scan).convert("RGB"), SegmentationInferenceConfig())
    out = []
    for line in seg.lines:
        pts = line.boundary or line.baseline
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        out.append({"box": [round(min(xs)), round(min(ys)), round(max(xs)), round(max(ys))]})
    return out


def kraken_tile_lines(scan, model, tile_h, overlap=150):
    from PIL import Image
    from kraken.configs import SegmentationInferenceConfig
    Image.MAX_IMAGE_PIXELS = None
    im = Image.open(scan).convert("RGB")
    W, H = im.size
    step = tile_h - overlap
    out = []
    top = 0
    while True:
        bottom = min(top + tile_h, H)
        seg = model.predict(im.crop((0, top, W, bottom)), SegmentationInferenceConfig())
        # keep a line from this tile when its centre is in the half of the overlap that belongs to it
        lo = 0 if top == 0 else top + overlap // 2
        hi = H if bottom == H else bottom - overlap // 2
        for line in seg.lines:
            pts = line.boundary or line.baseline
            xs = [p[0] for p in pts]
            ys = [p[1] + top for p in pts]
            cy = (min(ys) + max(ys)) / 2
            if lo <= cy < hi:
                out.append({"box": [round(min(xs)), round(min(ys)), round(max(xs)), round(max(ys))]})
        if bottom == H:
            return out
        top += step


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("method", choices=["tess", "kraken", "kraken-tiles"])
    ap.add_argument("--scans", required=True)
    ap.add_argument("--truth", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit", type=int)
    ap.add_argument("--jobs", type=int, default=1)
    ap.add_argument("--psm", type=int, default=3)
    ap.add_argument("--lang", default="eng")
    ap.add_argument("--shrink", type=int, default=1, help="reduce the scan this many times before Tesseract")
    ap.add_argument("--window", type=int, default=51, help="Sauvola window, pixels")
    ap.add_argument("--k", type=float, default=0.3, help="Sauvola k")
    ap.add_argument("--prep", choices=["none", "flat", "sauvola"], default="none")
    ap.add_argument("--tessdata", help="a tessdata directory holding every model --lang names")
    ap.add_argument("--model")
    ap.add_argument("--tile-h", type=int, default=1800)
    ap.add_argument("--select", help="file of page keys to run; the truth file's pages otherwise")
    a = ap.parse_args()
    pages = [json.loads(l)["page"] for l in open(a.truth)]
    if a.select:
        keep = {k.strip() for k in open(a.select) if k.strip()}
        pages = [p for p in pages if p in keep]
    if a.limit:
        pages = pages[: a.limit]
    done = set()
    if os.path.exists(a.out):
        done = {json.loads(l)["page"] for l in open(a.out)}
    todo = [p for p in pages if p not in done]
    model = None
    if a.method.startswith("kraken"):
        import torch
        torch.set_num_threads(1)
        from kraken.tasks import SegmentationTaskModel
        model = SegmentationTaskModel.load_model(a.model)

    def run(p):
        scan = Path(a.scans) / f"{doc_id(p)}.jpg"
        if a.method == "tess":
            lines = tess_lines(scan, a.psm, a.lang, a.tessdata, a.prep, a.shrink, a.window, a.k)
        elif a.method == "kraken":
            lines = kraken_lines(scan, model)
        else:
            lines = kraken_tile_lines(scan, model, a.tile_h)
        return {"page": p, "lines": lines}

    with open(a.out, "a") as out, cf.ThreadPoolExecutor(a.jobs) as pool:
        for rec in pool.map(run, todo):
            out.write(json.dumps(rec, sort_keys=True) + "\n")
            out.flush()
            print(rec["page"], len(rec["lines"]), file=sys.stderr)


if __name__ == "__main__":
    main()
