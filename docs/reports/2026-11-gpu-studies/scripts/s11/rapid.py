"""Study 11: RapidOCR 3.10 PP-OCRv6 small (the package default models) on regions or pages.

--mode read: det + cls + rec on each image, lines joined with newlines in RapidOCR order. Writes {id,text,lines,ms}.
--mode detect: line detection only. Writes {id,width,height,boxes,ms}, boxes as quads in image pixels, in RapidOCR order.
Rows are appended and flushed; ids already in --out are skipped, so a rerun resumes.
"""
import argparse, json, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--in-dir", type=Path, required=True)
ap.add_argument("--list", type=Path, required=True)  # JSONL {id,image}, image relative to --in-dir
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--mode", choices=["read", "detect"], required=True)
ap.add_argument("--threads", type=int, required=True)
ap.add_argument("--max-side-len", type=int, default=0)  # 0 keeps the package default (2000)
a = ap.parse_args()

from rapidocr import RapidOCR

params = {"EngineConfig.onnxruntime.intra_op_num_threads": a.threads, "EngineConfig.onnxruntime.inter_op_num_threads": 1, "Global.log_level": "error"}
if a.max_side_len:
    params["Global.max_side_len"] = a.max_side_len
eng = RapidOCR(params=params)
root = a.in_dir.resolve()
done = set()
if a.out.exists():
    for l in a.out.read_text(encoding="utf-8").splitlines():
        if l.strip():
            done.add(json.loads(l)["id"])
with a.out.open("a", encoding="utf-8") as out:
    for l in a.list.read_text(encoding="utf-8").splitlines():
        if not l.strip():
            continue
        r = json.loads(l)
        if r["id"] in done:
            continue
        p = (root / r["image"]).resolve()
        p.relative_to(root)
        s = time.time()
        if a.mode == "read":
            res = eng(str(p))
            txts = list(res.txts or [])
            rec = {"id": r["id"], "text": "\n".join(txts), "lines": len(txts)}
        else:
            res = eng(str(p), use_det=True, use_cls=False, use_rec=False)
            h, w = res.img.shape[:2] if getattr(res, "img", None) is not None else (0, 0)
            boxes = [] if res.boxes is None else [[[float(x), float(y)] for x, y in b] for b in res.boxes]
            rec = {"id": r["id"], "width": int(w), "height": int(h), "boxes": boxes}
        rec["ms"] = int((time.time() - s) * 1000)
        out.write(json.dumps(rec, ensure_ascii=False) + "\n")
        out.flush()
