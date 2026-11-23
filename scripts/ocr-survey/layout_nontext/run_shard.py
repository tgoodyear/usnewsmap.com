#!/usr/bin/env python3
"""Run the American Stories layout model over a shard of the Beyond Words validation pages (#2107).

Zero-shot, upstream settings, through layout_columns/asmodel.py unchanged (grayscale page replicated to
three channels, as in step 1). Dell et al., arXiv:2308.12477, mirror NealCaren/american-stories-onnx at
2558a023; pages: biglam/loc_beyond_words at 6c7f5fb3 (LoC, CC0), validation split only. The weights are
research use only and are never copied into the repository.

    run_shard.py --model layout_model_new.onnx --rows VAL.jsonl --images-zip images.zip --out shard.json
                 [--shard-index I --shard-count N] [--threads 2]

Output: {"_meta": ..., "pages": {page_id: [[x0, y0, x1, y1, conf, class], ...]}, "skipped": {page_id: reason}}
in the stored image's pixel frame. Pages are taken in sorted page_id order, shard I of N takes every Nth.
"""
import argparse
import json
import os
import sys
import time
import zipfile

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "layout_columns"))


def main(argv=None):
    import cv2
    import asmodel
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--model", required=True)
    ap.add_argument("--rows", required=True)
    ap.add_argument("--images-zip", required=True, help="images.zip; rows name it as <zip>!images/<file>")
    ap.add_argument("--out", required=True)
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    ap.add_argument("--threads", type=int, default=2)
    a = ap.parse_args(argv)
    rows = {}
    for ln in open(a.rows):
        r = json.loads(ln)
        if r.get("split") != "val":
            raise SystemExit("not a validation row: %s" % r.get("page_id"))
        rows[r["page_id"]] = r
    ids = sorted(rows)[a.shard_index:: a.shard_count]
    det = asmodel.Detector(a.model, a.threads)
    zf = zipfile.ZipFile(a.images_zip)
    pages, skipped = {}, {}
    t0 = time.time()
    for n, pid in enumerate(ids, 1):
        r = rows[pid]
        name = r["image_path"].split("!", 1)[1]
        try:
            gray = cv2.imdecode(np.frombuffer(zf.read(name), np.uint8), cv2.IMREAD_GRAYSCALE)
        except Exception as e:  # unreadable member
            skipped[pid] = "read error: %s" % e
            continue
        if gray is None:
            skipped[pid] = "image did not decode"
            continue
        if gray.shape != (r["height"], r["width"]):
            skipped[pid] = "decoded size %dx%d differs from the row %dx%d" % (
                gray.shape[1], gray.shape[0], r["width"], r["height"])
            continue
        pages[pid] = [[round(d[0], 1), round(d[1], 1), round(d[2], 1), round(d[3], 1), round(d[4], 4),
                       asmodel.NAMES[d[5]]] for d in det.boxes(gray)]
        print("pages %d/%d" % (n, len(ids)), flush=True)
    meta = {"conf": asmodel.CONF, "iou": asmodel.IOU, "max_det": asmodel.MAX_DET, "imgsz": asmodel.IMGSZ,
            "pages_assigned": len(ids), "shard_count": a.shard_count, "shard_index": a.shard_index,
            "threads": a.threads, "wall_seconds": round(time.time() - t0, 2)}
    with open(a.out, "w") as f:
        json.dump({"_meta": meta, "pages": pages, "skipped": skipped}, f, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
