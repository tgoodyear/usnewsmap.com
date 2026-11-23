#!/usr/bin/env python3
"""S0, the #1622 line finder (PR #1830), on any page list: Tesseract PSM 3 on the Sauvola-binarised scan,
then the clean-up. It calls the same code as `chr_layout_find.py tess --prep sauvola` and
`chr_layout_post.py clean` (Tesseract 5.5.0, `eng`, --psm 3 --dpi 300, one thread, window 51, k 0.3,
min height 14), and in addition keeps Tesseract's block.paragraph.line numbers of each box, so that reading
order survives for the silver-set blocks. The boxes are the same as the earlier scripts give.

    s0_find.py --pages pages.txt --scans DIR --layout ghn|loc --tessdata DIR --out found.jsonl [--jobs N] [--prep-slots 2]

One JSON line per page: {"page", "width", "height", "raw_lines", "lines": [{"box", "tid"}]}, where `tid`
is "<block>.<par>.<line>" and `lines` are the boxes kept by the clean-up. Pages already in --out are skipped.
"""
import argparse
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import tempfile

import common  # noqa: F401  (puts the #1622 scripts on the path)
import chr_layout_find as lf  # noqa: E402
import chr_layout_post as lp  # noqa: E402

PSM, LANG, MIN_H, WINDOW, K = 3, "eng", 14, 51, 0.3


def tess_rows(image, tessdata, psm=PSM, lang=LANG, offset=(0, 0)):
    """Tesseract's line rows of one image file: boxes (shifted by `offset`) and block.par.line numbers."""
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, OMP_THREAD_LIMIT="1")
        subprocess.run(["tesseract", str(image), f"{tmp}/o", "--psm", str(psm), "-l", lang, "--dpi", "300",
                        "--tessdata-dir", tessdata, "-c", "tessedit_create_tsv=1"],
                       check=True, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        rows = [l.rstrip("\n").split("\t") for l in open(f"{tmp}/o.tsv", encoding="utf-8")][1:]
    out = []
    for r in rows:
        if r[0] == "4":
            x, y, w, h = (int(v) for v in r[6:10])
            out.append({"box": [x + offset[0], y + offset[1], x + w + offset[0], y + h + offset[1]],
                        "tid": f"{r[2]}.{r[3]}.{r[4]}"})
    return out


_GATE = None  # bounds how many workers binarise at once (about 1.2 GB each on a GHN page)


def _init(gate):
    global _GATE
    _GATE = gate


def sauvola(scan, tmp):
    if _GATE is None:
        return lf.prepare(scan, "sauvola", tmp, 1, WINDOW, K)
    with _GATE:
        return lf.prepare(scan, "sauvola", tmp, 1, WINDOW, K)


def s0_page(scan, tessdata):
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    with Image.open(scan) as im:
        w, h = im.size
    with tempfile.TemporaryDirectory() as tmp:
        prepped = sauvola(scan, tmp)
        raw = tess_rows(prepped, tessdata)
    return {"width": w, "height": h, "raw_lines": len(raw), "lines": lp.clean(raw, w, MIN_H)}


def _run(args):
    page, scan, tessdata = args
    return {"page": page, **s0_page(scan, tessdata)}


def pool_of(jobs, slots):
    import multiprocessing as mp
    # one page a worker: a worker that has binarised a page keeps over 1 GB, so it is replaced after each page
    ctx = mp.get_context("spawn")
    return cf.ProcessPoolExecutor(jobs, mp_context=ctx, initializer=_init, initargs=(ctx.Semaphore(slots),),
                                  max_tasks_per_child=1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pages", required=True, help="page ids, one per line")
    ap.add_argument("--scans", required=True)
    ap.add_argument("--layout", choices=["ghn", "loc"], required=True)
    ap.add_argument("--tessdata", required=True, help="tessdata directory holding eng.traineddata")
    ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=1)
    ap.add_argument("--prep-slots", type=int, default=2, help="workers allowed to binarise at the same time")
    a = ap.parse_args()
    pages = common.read_pages(a.pages)
    done = {json.loads(l)["page"] for l in open(a.out)} if os.path.exists(a.out) else set()
    todo = [(p, str(common.scan_path(a.scans, p, a.layout)), a.tessdata) for p in pages if p not in done]
    print(f"{len(todo)} pages to do, {len(done)} done", file=sys.stderr, flush=True)
    with open(a.out, "a") as out, pool_of(a.jobs, a.prep_slots) as pool:
        for i, rec in enumerate(pool.map(_run, todo), 1):
            out.write(json.dumps(rec, sort_keys=True) + "\n")
            out.flush()
            print(f"{i}/{len(todo)} {rec['page']} {rec['raw_lines']} -> {len(rec['lines'])}", file=sys.stderr, flush=True)


if __name__ == "__main__":
    main()
