#!/usr/bin/env python3
"""Read a list with Surya in the usnm_surya image, one model load for the whole list. Output is JSONL."""
import argparse, json, sys, time
sys.path.insert(0, "/opt/surya")
import run as surya_run  # the image's run.py: tile rule and block text
from PIL import Image
from surya.inference import SuryaInferenceManager
from surya.recognition import RecognitionPredictor
from surya.layout.schema import LayoutBox, LayoutResult

ap = argparse.ArgumentParser()
ap.add_argument("--list", required=True)    # TSV id, path relative to /work/in
ap.add_argument("--mode", choices=["whole", "tiled", "capped", "line"], required=True)
ap.add_argument("--out", required=True)
a = ap.parse_args()
Image.MAX_IMAGE_PIXELS = None
t0 = time.time()
manager = SuryaInferenceManager(lazy=False)
rec = RecognitionPredictor(manager)
load = time.time() - t0
rows = [l.rstrip("\n").split("\t") for l in open(a.list, encoding="utf-8") if l.strip()]
with open(a.out, "w", encoding="utf-8", buffering=1) as out, open("/work/out/progress.txt", "a", buffering=1) as prog:
    prog.write(f"surya model load {load:.0f}s\n")
    for i, (cid, rel) in enumerate(rows):
        s = time.time(); status = "ok"; lines = []
        try:
            im = Image.open("/work/in/" + rel); im.load(); im = im.convert("RGB")
            if a.mode == "tiled":
                for x0, y0, x1, y1, core in surya_run.tiles(*im.size):
                    for b in rec([im.crop((x0, y0, x1, y1))], full_page=True)[0].blocks:
                        if b.error or b.skipped:
                            continue
                        bx0, by0, bx1, by1 = b.bbox
                        t = surya_run.block_text(b.html)
                        if t and surya_run.inside(core, x0 + (bx0 + bx1) / 2, y0 + (by0 + by1) / 2):
                            lines.append(t)
            elif a.mode == "line":
                # Block mode with the crop as its one block: Surya's recognition without its page layout.
                w, h = im.size
                box = LayoutBox(polygon=[[0, 0], [w, 0], [w, h], [0, h]], label="Text", raw_label="Text", position=0, count=400)
                res = rec([im], [LayoutResult(bboxes=[box], image_bbox=[0, 0, w, h])], full_page=False)[0]
                for b in res.blocks:
                    if b.error:
                        status = "error: block"
                    t = surya_run.block_text(b.html)
                    if t:
                        lines.append(t)
            else:
                if a.mode == "capped" and max(im.size) > surya_run.CAPPED_LONG_SIDE:
                    f = surya_run.CAPPED_LONG_SIDE / max(im.size)
                    im = im.resize((max(1, round(im.width * f)), max(1, round(im.height * f))), Image.LANCZOS)
                for b in rec([im], full_page=True)[0].blocks:
                    if not (b.error or b.skipped):
                        t = surya_run.block_text(b.html)
                        if t:
                            lines.append(t)
        except Exception as e:  # counted as missing on our side
            status = f"error: {str(e)[:200]}"
        rec_out = {"id": cid, "status": status, "ms": int((time.time() - s) * 1000)}
        if status == "ok":
            rec_out["text"] = "\n".join(lines)
        out.write(json.dumps(rec_out, ensure_ascii=False) + "\n")
        if i % 10 == 0:
            prog.write(f"surya {i}/{len(rows)} {time.time() - t0:.0f}s\n")
manager.stop()
