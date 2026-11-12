"""Does reading from a finder's boxes cost accuracy? (#1622) The step-3 model reads Cherokee crops cut at LoC's
line boxes. For each Cherokee line a finder's box matches (`chr_layout_eval.usable`), this reads the crop at
LoC's box and the crop at the finder's box, the way a line job does (padding, PSM 13, 300 DPI, one Tesseract
run over the list of crops), and scores both.

  --truth truth.jsonl --found found.jsonl --scans DIR --tessdata DIR [--lang chr_phoenix] [--max-lines N]
  [--seed S] [--pad-y-tall N] [--reads-out FILE]  -> one JSON object on standard output

Reported over syllabary letters only (other characters dropped from both sides): the share of letters
different between the two readings (edit distance over the LoC-box reading's length), and each reading's
edit distance to LoC's own text. LoC's text is unchecked OCR, so the second pair shows direction only.
"""
import argparse
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chr_layout_eval as ev  # noqa: E402

Image.MAX_IMAGE_PIXELS = None
PAD_X, PAD_Y = 12, 8  # crates/usnm-boinc/src/lines.rs


def lev(a, b):
    if not a:
        return len(b)
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def syll(s):
    return "".join(c for c in s if ev.SYLL.match(c))


def crop(img, box, pad_x, pad_y):
    h, w = img.shape
    x0, y0 = max(box[0] - pad_x, 0), max(box[1] - pad_y, 0)
    x1, y1 = min(box[2] + pad_x, w), min(box[3] + pad_y, h)
    return img[y0:y1, x0:x1]


def read_crops(crops, lang, tessdata):
    """One Tesseract run over the crops (PSM 13, as the worker's `read_lines`); one text per crop."""
    with tempfile.TemporaryDirectory() as tmp:
        names = []
        for i, c in enumerate(crops):
            Image.fromarray(c).save(f"{tmp}/line-{i:05d}.png")
            names.append(f"line-{i:05d}.png")
        Path(tmp, "lines.txt").write_text("\n".join(names) + "\n")
        subprocess.run(["tesseract", "lines.txt", "lines", "-l", lang, "--tessdata-dir", tessdata, "--psm", "13",
                        "--dpi", "300", "-c", "tessedit_create_tsv=1"], cwd=tmp, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env={"OMP_THREAD_LIMIT": "1", "PATH": "/usr/bin"})
        words = {}
        for row in Path(tmp, "lines.tsv").read_text(encoding="utf-8").splitlines()[1:]:
            f = row.split("\t")
            if len(f) >= 12 and f[0] == "5" and f[11].strip():
                words.setdefault(int(f[1]), []).append(f[11])
    return [" ".join(words.get(i + 1, [])) for i in range(len(crops))]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--truth", required=True)
    ap.add_argument("--found", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--tessdata", required=True)
    ap.add_argument("--lang", default="chr_phoenix")
    ap.add_argument("--max-lines", type=int, default=600)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--pad-y-tall", type=int, default=18)
    ap.add_argument("--reads-out", help="write each line's readings here (JSON Lines)")
    a = ap.parse_args()
    truth = {r["page"]: r for r in map(json.loads, open(a.truth))}
    found = {r["page"]: r for r in map(json.loads, open(a.found))}
    pairs = []
    for page in sorted(set(truth) & set(found)):
        fb = [l["box"] for l in found[page]["lines"] if l["box"][2] - l["box"][0] >= 8 and l["box"][3] - l["box"][1] >= 8]
        for l in truth[page]["lines"]:
            if not l["chr"] or not syll(l["text"]):
                continue
            ok = [b for b in fb if ev.usable(l["box"], b)]
            if len(ok) == 1:
                pairs.append((page, l, ok[0]))
    total_pairs = len(pairs)
    random.Random(a.seed).shuffle(pairs)
    pairs = sorted(pairs[: a.max_lines], key=lambda p: p[0])
    rows = []
    by_page = {}
    for p in pairs:
        by_page.setdefault(p[0], []).append(p)
    crops = {"loc": [], "found": [], "found_tall": []}
    order = []
    for page, items in by_page.items():
        img = np.asarray(Image.open(Path(a.scans) / f"{page.replace('/', '_')}.jpg").convert("L"))
        for _, l, b in items:
            crops["loc"].append(crop(img, l["box"], PAD_X, PAD_Y))
            crops["found"].append(crop(img, b, PAD_X, PAD_Y))
            crops["found_tall"].append(crop(img, b, PAD_X, a.pad_y_tall))
            order.append((page, l))
    reads = {k: read_crops(v, a.lang, a.tessdata) for k, v in crops.items()}
    acc = {k: {"letters_ref": 0, "dist_vs_loc_box": 0, "dist_vs_loc_text": 0, "empty": 0} for k in reads}
    loc_text_letters = 0
    for i, (page, l) in enumerate(order):
        ref = syll(reads["loc"][i])
        loc_text = syll(l["text"])
        loc_text_letters += len(loc_text)
        for k in reads:
            s = syll(reads[k][i])
            acc[k]["letters_ref"] += len(ref)
            acc[k]["dist_vs_loc_box"] += lev(s, ref)
            acc[k]["dist_vs_loc_text"] += lev(s, loc_text)
            acc[k]["empty"] += not s
        rows.append({"page": page, "id": l["id"], "loc_text": l["text"], **{k: reads[k][i] for k in reads}})
    out = {"matched_chr_lines": total_pairs, "lines_read": len(order), "loc_text_letters": loc_text_letters}
    for k, v in acc.items():
        out[k] = {"empty_readings": v["empty"],
                  "differs_from_loc_box_reading": round(v["dist_vs_loc_box"] / max(v["letters_ref"], 1), 4),
                  "distance_to_loc_text": round(v["dist_vs_loc_text"] / max(loc_text_letters, 1), 4)}
    print(json.dumps(out, indent=1))
    if a.reads_out:
        Path(a.reads_out).write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n")


if __name__ == "__main__":
    main()
