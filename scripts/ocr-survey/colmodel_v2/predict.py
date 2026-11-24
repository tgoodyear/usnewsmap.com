#!/usr/bin/env python3
"""Run the exported v2 column detector (ONNX, CPU) on page images and time it (#2107).

    predict.py --model colv2.onnx --pages pages.tsv --out preds.json [--threads 1] [--floor 0.01]
               [--shard-index 0 --shard-count 1]

--pages is a TSV with columns page_id and image_path (image_path in any imgread.py form); a labels
JSONL works too. Each image is turned grey, resized to model.MAX_SIDE on its longer side, run once,
and the boxes are scaled back to the original image's pixels. Boxes scoring under --floor are not
kept (thresholds are applied later by to_gutters.py). Output, keys sorted: page_id -> {boxes, scores,
width, height, seconds}, plus "_meta" {model sha256, threads, seconds per page: mean, median, p90,
split into decode+resize and model}.
"""
import argparse
import csv
import hashlib
import io
import json
import os
import statistics
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import imgread  # noqa: E402
import model as M  # noqa: E402


def read_pages(path):
    if path.endswith(".jsonl"):
        with open(path) as f:
            return [{"page_id": str(d["page_id"]), "image_path": d["image_path"]}
                    for d in (json.loads(ln) for ln in f if ln.strip())]
    with open(path, newline="") as f:
        return [{"page_id": r.get("page_id") or r.get("doc_id"), "image_path": r["image_path"]}
                for r in csv.DictReader(f, delimiter="\t")]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--model", required=True)
    ap.add_argument("--pages", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--threads", type=int, default=1)
    ap.add_argument("--floor", type=float, default=0.01)
    ap.add_argument("--shard-index", type=int, default=0)
    ap.add_argument("--shard-count", type=int, default=1)
    a = ap.parse_args(argv)
    import numpy as np
    import onnxruntime as ort
    from PIL import Image
    so = ort.SessionOptions()
    so.intra_op_num_threads = a.threads
    so.inter_op_num_threads = 1
    sess = ort.InferenceSession(a.model, sess_options=so, providers=["CPUExecutionProvider"])
    with open(a.model, "rb") as f:
        sha = hashlib.sha256(f.read()).hexdigest()
    pages = [p for i, p in enumerate(read_pages(a.pages)) if i % a.shard_count == a.shard_index]
    rd = imgread.Reader()
    out, t_pre, t_run = {}, [], []
    for i, p in enumerate(pages, 1):
        t0 = time.perf_counter()
        img = Image.open(io.BytesIO(rd.read(p["image_path"]))).convert("L")
        w, h = img.size
        small, s = M.resize_for_model(img)
        x = M.to_tensor(small)[None].numpy()
        t1 = time.perf_counter()
        boxes, scores = sess.run(None, {"image": x})
        t2 = time.perf_counter()
        keep = scores >= a.floor
        out[p["page_id"]] = {"boxes": [[round(float(v) / s, 1) for v in b] for b in boxes[keep]],
                             "height": h, "scores": [round(float(v), 4) for v in scores[keep]],
                             "seconds": round(t2 - t0, 3), "width": w}
        t_pre.append(t1 - t0)
        t_run.append(t2 - t1)
        if i % 10 == 0 or i == len(pages):
            print("pages %d/%d model %.2fs/page" % (i, len(pages), statistics.mean(t_run)), flush=True)
    tot = [x + y for x, y in zip(t_pre, t_run)]

    def stats(v):
        if not v:
            return {}
        v = sorted(v)
        return {"mean": round(statistics.mean(v), 3), "median": round(statistics.median(v), 3),
                "p90": round(float(np.percentile(v, 90)), 3)}
    out["_meta"] = {"model": os.path.basename(a.model), "model_sha256": sha, "pages": len(pages),
                    "seconds_per_page": stats(tot), "seconds_decode_resize": stats(t_pre),
                    "seconds_model": stats(t_run), "threads": a.threads, "floor": a.floor,
                    "cpu": _cpu_name()}
    with open(a.out, "w") as f:
        json.dump(out, f, sort_keys=True)
    print(json.dumps(out["_meta"], sort_keys=True))
    return 0


def _cpu_name():
    try:
        with open("/proc/cpuinfo") as f:
            for ln in f:
                if ln.startswith("model name"):
                    return ln.split(":", 1)[1].strip()
    except OSError:
        pass
    return None


if __name__ == "__main__":
    sys.exit(main())
