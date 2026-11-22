"""Line finding for pages without ALTO (#1622): LoC's ALTO lines as truth, and the scores of a finder's boxes.

Subcommands (named arguments only; JSON Lines in and out):

  truth   --pages phoenix-pages.jsonl --alto-dir <unpacked LoC archives> --scans <dir of <doc_id>.jpg>
          [--select keys.txt]      one record per page: its text lines in scan pixels, with each line's language
  score   --truth truth.jsonl --found found.jsonl [--label NAME]
          recall and precision of a finder's boxes against the truth, pooled and per page

A truth record is {"page", "width", "height", "lines": [{"id", "box": [x0, y0, x1, y1], "chr": bool,
"words": n, "text"}]}. A found record is {"page", "lines": [{"box": [...], "score": optional}]}; boxes are
in the scan's pixels. `chr` is what `usnm-boinc export --line-read chr` decides (`lines::select_lines`): the
line's block is marked chr, or most of its words are syllabary.

LoC's line boxes are loose: they run taller than the print and overlap their neighbours' by a few pixels, and
a finder that hugs the ink gets IoU of 0.5 to 0.7 against them on a perfect line. So the main score looks at
what a crop would hold. A truth line and a found box match ("usable") when:
  - the box's centre is in the middle half of the truth line's height and inside its width;
  - the box covers at least 80% of the truth line's width, and is at most 1.6 times as wide (no merge across
    a gutter);
  - the box is at most 1.6 times as tall as the truth line (no merge of two lines).
Recall is truth lines with a matching box over truth lines; precision is boxes matching a truth line over boxes
(a box smaller than 8 pixels either way is dropped first). Truth is LoC's own segmentation, so a box on real
print that LoC's OCR missed counts against precision. Also reported: truth lines matched by two or more boxes
(split), boxes that match two or more truth lines (merged), and strict IoU for comparison.
"""
import argparse
import json
import re
import statistics
import struct
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SYLL = re.compile("[Ꭰ-᏿ꭰ-ꮿ]")


def local(tag):
    return tag.rsplit("}", 1)[-1]


def jpeg_size(path):
    with open(path, "rb") as f:
        f.read(2)
        while True:
            b = f.read(1)
            while b != b"\xff":
                b = f.read(1)
            while b == b"\xff":
                b = f.read(1)
            m = b[0]
            if m in (0xD8, 0x01) or 0xD0 <= m <= 0xD7:
                continue
            (n,) = struct.unpack(">H", f.read(2))
            if 0xC0 <= m <= 0xCF and m not in (0xC4, 0xC8, 0xCC):
                _, h, w, _ = struct.unpack(">BHHB", f.read(6))
                return w, h
            f.seek(n - 2, 1)


def alto_path(root, page):
    lccn, date, ed, seq = page.split("/")
    y, m, d = date.split("-")
    return Path(root) / lccn / y / m / d / ed / seq / "ocr.xml"


def parse_truth(path, scan_w, scan_h):
    """The text lines of one LoC ALTO, boxes scaled to the scan (as `lines::select_lines` scales them)."""
    root = ET.parse(path).getroot()
    page = next(e for e in root.iter() if local(e.tag) == "Page")
    pw, ph = float(page.get("WIDTH")), float(page.get("HEIGHT"))
    sx, sy = scan_w / pw, scan_h / ph
    lines = []
    for bi, block in enumerate(e for e in root.iter() if local(e.tag) == "TextBlock"):
        block_chr = block.get("language") == "chr"
        for li, line in enumerate(e for e in block.iter() if local(e.tag) == "TextLine"):
            words = [s.get("CONTENT") or "" for s in line if local(s.tag) == "String"]
            x0 = float(line.get("HPOS")) * sx
            y0 = float(line.get("VPOS")) * sy
            x1 = x0 + float(line.get("WIDTH")) * sx
            y1 = y0 + float(line.get("HEIGHT")) * sy
            box = [round(x0), round(y0), round(x1), round(y1)]
            if not (box[0] < box[2] and box[1] < box[3]):
                continue
            counted = [w for w in words if any(c.isalpha() for c in w)]
            syll = sum(1 for w in counted if sum(bool(SYLL.match(c)) for c in w) * 2 > sum(c.isalpha() for c in w))
            mostly = bool(counted) and syll * 2 > len(counted)
            lines.append({"id": f"b{bi}l{li}", "box": box, "chr": block_chr or mostly,
                          "words": len(words), "text": " ".join(words)})
    return lines


def cmd_truth(a):
    keys = None
    if a.select:
        keys = {k.strip() for k in open(a.select) if k.strip()}
    for line in open(a.pages):
        r = json.loads(line)
        if keys is not None and r["page"] not in keys:
            continue
        scan = Path(a.scans) / f"{r['doc_id']}.jpg"
        if not scan.exists() or not alto_path(a.alto_dir, r["page"]).exists():
            continue
        w, h = jpeg_size(scan)
        lines = parse_truth(alto_path(a.alto_dir, r["page"]), w, h)
        print(json.dumps({"page": r["page"], "width": w, "height": h, "lines": lines},
                         ensure_ascii=False, sort_keys=True))


def inter(a, b):
    w = min(a[2], b[2]) - max(a[0], b[0])
    h = min(a[3], b[3]) - max(a[1], b[1])
    return max(w, 0) * max(h, 0)


def area(a):
    return max(a[2] - a[0], 0) * max(a[3] - a[1], 0)


def greedy(pairs, n, m):
    """One-to-one matches from (score, i, j) pairs, best first."""
    used_i, used_j, out = set(), set(), []
    for _, i, j in sorted(pairs, reverse=True):
        if i in used_i or j in used_j:
            continue
        used_i.add(i)
        used_j.add(j)
        out.append((i, j))
    return out


def usable(t, f):
    """Whether found box `f` holds truth line `t` the way a crop must (see the module's notes)."""
    tw, th = t[2] - t[0], t[3] - t[1]
    fw, fh = f[2] - f[0], f[3] - f[1]
    cx, cy = (f[0] + f[2]) / 2, (f[1] + f[3]) / 2
    if not (t[0] <= cx <= t[2] and t[1] + th / 4 <= cy <= t[3] - th / 4):
        return False
    ox = min(t[2], f[2]) - max(t[0], f[0])
    return ox >= 0.8 * tw and fw <= 1.6 * tw and fh <= 1.6 * th


def score_page(truth, found):
    t = [l["box"] for l in truth]
    f = [l["box"] for l in found if l["box"][2] - l["box"][0] >= 8 and l["box"][3] - l["box"][1] >= 8]
    n, m = len(t), len(f)
    iou = {}
    for i in range(n):
        for j in range(m):
            x = inter(t[i], f[j])
            if x:
                iou[i, j] = x / (area(t[i]) + area(f[j]) - x)
    res = {"truth": n, "found": m}
    for thr in (0.5,):
        pairs = [(v, i, j) for (i, j), v in iou.items() if v >= thr]
        matched = greedy(pairs, n, m)
        res[f"iou{thr}"] = {"truth_hit": {i for i, _ in matched}, "found_hit": {j for _, j in matched}}
    ok = [(i, j) for (i, j) in iou if usable(t[i], f[j])]
    res["usable"] = {"truth_hit": {i for i, _ in ok}, "found_hit": {j for _, j in ok}}
    per_t, per_f = {}, {}
    for i, j in ok:
        per_t[i] = per_t.get(i, 0) + 1
        per_f[j] = per_f.get(j, 0) + 1
    res["split"] = sum(1 for v in per_t.values() if v >= 2)
    # a merged box: its height holds two truth lines' centres
    merged = 0
    for j in range(m):
        centres = sum(1 for i in range(n) if f[j][1] <= (t[i][1] + t[i][3]) / 2 <= f[j][3]
                      and min(t[i][2], f[j][2]) - max(t[i][0], f[j][0]) >= 0.5 * (t[i][2] - t[i][0]))
        merged += centres >= 2
    res["merged"] = merged
    res["chr_idx"] = {i for i, l in enumerate(truth) if l["chr"]}
    return res


def cmd_score(a):
    truth = {r["page"]: r for r in map(json.loads, open(a.truth))}
    found = {r["page"]: r for r in map(json.loads, open(a.found))}
    pages = sorted(set(truth) & set(found))
    missing = sorted(set(truth) - set(found))
    tot = {"truth": 0, "found": 0, "split": 0, "merged": 0, "chr_truth": 0}
    hits = {k: {"truth": 0, "found": 0, "chr_truth": 0} for k in ("iou0.5", "usable")}
    per_page = []
    for p in pages:
        s = score_page(truth[p]["lines"], found[p]["lines"])
        tot["truth"] += s["truth"]
        tot["found"] += s["found"]
        tot["split"] += s["split"]
        tot["merged"] += s["merged"]
        tot["chr_truth"] += len(s["chr_idx"])
        row = {"page": p, "truth": s["truth"], "found": s["found"]}
        for k in hits:
            hits[k]["truth"] += len(s[k]["truth_hit"])
            hits[k]["found"] += len(s[k]["found_hit"])
            hits[k]["chr_truth"] += len(s[k]["truth_hit"] & s["chr_idx"])
        row["usable_recall"] = round(len(s["usable"]["truth_hit"]) / s["truth"], 4) if s["truth"] else None
        row["usable_precision"] = round(len(s["usable"]["found_hit"]) / s["found"], 4) if s["found"] else None
        per_page.append(row)
    out = {"label": a.label, "pages": len(pages), "missing_pages": len(missing), **tot}
    for k, v in hits.items():
        out[k] = {"recall": round(v["truth"] / tot["truth"], 4),
                  "precision": round(v["found"] / tot["found"], 4) if tot["found"] else None,
                  "chr_recall": round(v["chr_truth"] / tot["chr_truth"], 4) if tot["chr_truth"] else None}
    rec = [r["usable_recall"] for r in per_page if r["usable_recall"] is not None]
    prec = [r["usable_precision"] for r in per_page if r["usable_precision"] is not None]
    out["usable_recall_by_page"] = {"min": min(rec), "median": statistics.median(rec), "max": max(rec)} if rec else None
    out["usable_precision_by_page"] = {"min": min(prec), "median": statistics.median(prec), "max": max(prec)} if prec else None
    out["per_page"] = per_page
    print(json.dumps(out, indent=1, sort_keys=True))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("truth")
    t.add_argument("--pages", required=True)
    t.add_argument("--alto-dir", required=True)
    t.add_argument("--scans", required=True)
    t.add_argument("--select")
    t.set_defaults(fn=cmd_truth)
    s = sub.add_parser("score")
    s.add_argument("--truth", required=True)
    s.add_argument("--found", required=True)
    s.add_argument("--label", default="")
    s.set_defaults(fn=cmd_score)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
