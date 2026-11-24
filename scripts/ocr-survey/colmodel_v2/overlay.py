#!/usr/bin/env python3
"""Render weak-label overlays for eyeballing (#2107, model v2).

    overlay.py --pages pages.tsv --alto-dir OUT/alto --ids-file ids.txt --out-dir samples [--width 1400]
    overlay.py --labels labels.jsonl --ids-file ids.txt --out-dir samples      (reference-set labels)

From ALTO, the page is relabelled with alto_weak.label_page and drawn with: ALTO TextLines (thin grey),
column regions (translucent, alternating colours), gutters (red) and a caption with the filter
verdict and metrics, so rejected pages can be inspected too. From a labels file, only the regions and
gutters are drawn. Output: <out-dir>/<page_id>.jpg (derived images, deletable).
"""
import argparse
import csv
import io
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import alto_weak  # noqa: E402
import imgread  # noqa: E402

COLOURS = [(30, 120, 255), (0, 170, 90), (230, 140, 0), (160, 60, 200)]


def draw(img, regions, gutters, lines, caption, width):
    from PIL import Image, ImageDraw
    s = width / float(img.size[0])
    base = img.convert("RGB").resize((width, round(img.size[1] * s)), Image.BILINEAR)
    layer = Image.new("RGBA", base.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    for b in lines:
        d.rectangle([b[0] * s, b[1] * s, b[2] * s, b[3] * s], outline=(90, 90, 90, 160), width=1)
    for i, r in enumerate(regions):
        c = COLOURS[i % len(COLOURS)]
        d.rectangle([r[0] * s, r[1] * s, r[2] * s, r[3] * s], fill=c + (50,), outline=c + (255,), width=3)
    for g in gutters:
        for x0, x1, y0, y1 in g:
            cx = (x0 + x1) / 2 * s
            d.line([cx, y0 * s, cx, y1 * s], fill=(230, 0, 0, 255), width=3)
    out = Image.alpha_composite(base.convert("RGBA"), layer).convert("RGB")
    band = Image.new("RGB", (out.size[0], 60), (255, 255, 255))
    ImageDraw.Draw(band).multiline_text((8, 6), caption, fill=(0, 0, 0))
    full = Image.new("RGB", (out.size[0], out.size[1] + 60), (255, 255, 255))
    full.paste(band, (0, 0))
    full.paste(out, (0, 60))
    return full


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--pages")
    ap.add_argument("--alto-dir")
    ap.add_argument("--labels")
    ap.add_argument("--ids-file", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--width", type=int, default=1400)
    a = ap.parse_args(argv)
    from PIL import Image
    ids = alto_weak.read_id_list(a.ids_file)
    os.makedirs(a.out_dir, exist_ok=True)
    rd = imgread.Reader()
    if a.labels:
        with open(a.labels) as f:
            rows = {str(d["page_id"]): d for d in (json.loads(ln) for ln in f if ln.strip())}
        for pid in ids:
            r = rows[pid]
            img = Image.open(io.BytesIO(rd.read(r["image_path"])))
            cap = "%s  %s  columns %d  gutters %d" % (r["source"], pid, len(r["columns"]), len(r["gutters"]))
            draw(img, r["columns"], r["gutters"], [], cap, a.width).save(os.path.join(a.out_dir, pid + ".jpg"), quality=85)
        return 0
    with open(a.pages, newline="") as f:
        pages = {r["doc_id"]: r for r in csv.DictReader(f, delimiter="\t")}
    for pid in ids:
        img = Image.open(pages[pid]["image_path"])
        with open(os.path.join(a.alto_dir, pid + ".xml"), "rb") as f:
            parsed = alto_weak.parse_alto(f.read(), img.size)
        lab = alto_weak.label_page(parsed, img.size[0], img.size[1])
        m = lab["metrics"]
        verdict = "ACCEPTED" if lab["passed"] else "REJECTED (" + ",".join(lab["failed"]) + ")"
        fmt = lambda v: "-" if v is None else ("%.3f" % v if isinstance(v, float) else str(v))  # noqa: E731
        cap = "%s  %s\n%s" % (pid, verdict, "  ".join("%s=%s" % (k, fmt(m[k])) for k in sorted(m)))
        draw(img, lab["columns"], lab["gutters"], parsed["lines"], cap, a.width).save(
            os.path.join(a.out_dir, pid + ".jpg"), quality=85)
        print(pid, verdict, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
