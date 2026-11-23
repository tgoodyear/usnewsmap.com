#!/usr/bin/env python3
"""Teacher labels for the Beyond Words train pages (#2107): American Stories layout model, zero-shot.

Runs layout_columns/asmodel.py's Detector (the approved settings: letterbox 1280, conf 0.01, NMS IoU
0.1 agnostic, max_det 2000) on each page and writes every detection to a JSONL, one line per page,
so the 0.25 cut is applied later and the run can resume. American Stories: Dell et al.,
arXiv:2308.12477. Beyond Words (Library of Congress, CC0). Research use only; weights stay in place.

    teacher.py --model ONNX --rows refs.jsonl --out shard.jsonl [--shard-index I --shard-count N]
               [--threads 2] [--limit N]
"""
import argparse
import json
import os
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
sys.path.insert(0, HERE)
import rowimages  # noqa: E402


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--model", required=True)
    ap.add_argument("--rows", required=True, help="refs JSONL (Beyond Words train)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--zip-override", help="use this images.zip instead of the one in image_path")
    a = ap.parse_args(argv)
    import cv2
    import asmodel
    rows = [json.loads(ln) for ln in open(a.rows)]
    rows.sort(key=lambda r: r["page_id"])
    rows = rows[a.shard_index:: a.shard_count]
    if a.limit:
        rows = rows[: a.limit]
    done = set()
    if os.path.exists(a.out):
        done = {json.loads(ln)["page_id"] for ln in open(a.out) if ln.strip()}
    det = asmodel.Detector(a.model, a.threads)
    rd = rowimages.Reader()
    t0 = time.time()
    with open(a.out, "a") as out:
        for n, r in enumerate(rows, 1):
            if r["page_id"] in done:
                continue
            p = r["image_path"]
            if a.zip_override:
                p = a.zip_override + "!" + p.split("!", 1)[1]
            img = cv2.imdecode(np.frombuffer(rd.read(p), np.uint8), cv2.IMREAD_GRAYSCALE)
            h, w = img.shape
            if (w, h) != (r["width"], r["height"]):
                raise SystemExit("size mismatch for page %s: %s vs %s" % (r["page_id"], (w, h), (r["width"], r["height"])))
            dets = det.boxes(img)
            boxes = [[round(d[0], 1), round(d[1], 1), round(d[2], 1), round(d[3], 1), round(d[4], 4), asmodel.NAMES[d[5]]]
                     for d in dets]
            out.write(json.dumps({"page_id": r["page_id"], "boxes": boxes}) + "\n")
            out.flush()
            print("pages %d/%d %.1fs" % (n, len(rows), time.time() - t0), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
