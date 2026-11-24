"""Sample and blinded contact sheets for the line-classifier check (#2128, Amendments 2 and 2.1).

  clf_sheets.py contrast --summary S --pilot P --inventory I --ghn-root R --out contrast.jsonl --workers N
  clf_sheets.py build --summary S --summary-sha256 H --pilot P --contrast C --boxes-dir D --found F
                      --inventory I --ghn-root R --aligned A --loc-images L --alto-root X
                      --wordlist W --out-dir O

`contrast`: the p95 - p5 of each pool page's greyscale scan, scaled to 1,000 px wide (box filter).
`build`: draws the pages and boxes, picks the calibration lines, cuts and shuffles every crop, and
writes the contact sheets. key.jsonl (private) ties each opaque crop id to its source; the sheets
and their crop-id lists carry nothing but the ids and the crops.

Formats: page_id is summary.jsonl's doc_id (`ghn:sn83020866_1828-02-21_ed-1_seq-1`); box_id is
boxes.jsonl's `id` (`r0b2p1l1`). A calibration line's page_id is `loc:` + LoC page with `/` -> `_`,
its box_id the ALTO TextLine ID, and its line_id page_id + ":" + box_id.
"""
import argparse
import hashlib
import json
import math
import re
import shutil
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
import xml.etree.ElementTree as ET

import numpy as np
from PIL import Image, ImageDraw, ImageFont

Image.MAX_IMAGE_PIXELS = None

PAD = 6               # px added on each side of a box, in scan pixels
TARGET_H = 56         # crop height after scaling
MAX_W = 1400          # crop width cap; the scale is min(TARGET_H / h, MAX_W / w)
PER_SHEET = 20
MARGIN = 190          # left margin holding the id
GAP = 10
N_HELD = N_NOT_HELD = 10
MIN_FADED = 6
N_OTHER = 150
N_CAL_CHR = N_CAL_LAT = 200
MIN_LOOKALIKE = 60
CAL_TIERS = (None, "wording", "gap_run")
LOOKALIKE = {chr(c) for c in (0x13a0, 0x13a1, 0x13a2, 0x13a9, 0x13aa, 0x13ab, 0x13b3, 0x13b7, 0x13bb, 0x13c0, 0x13c1, 0x13c3, 0x13c6, 0x13cf, 0x13d2, 0x13da, 0x13df, 0x13e2, 0x13e3, 0x13e4, 0x13e6, 0x13e7, 0x13f4)}  # the 23 letters listed in Amendment 2.1
SYLLABARY = re.compile(r"[Ꭰ-᏿ꭰ-ꮿ]")
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"
FONT_FALLBACK = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"


def sha(s):
    return hashlib.sha256(s.encode("utf-8")).hexdigest()


def file_sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def jsonl(path):
    return [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]


def inv_id(page_id):
    """`ghn:sn83020866_1828-02-21_ed-1_seq-1` -> `sn83020866/1828-02-21/ed-1/seq-1`."""
    lccn, date, ed, seq = page_id.split(":", 1)[1].split("_")
    return f"{lccn}/{date}/{ed}/{seq}"


def pool(summary, pilot):
    excl = set(pilot)
    return [r for r in summary if r["doc_id"] not in excl and r["boxes"] >= 1]


def contrast_of(path):
    im = Image.open(path).convert("L")
    w, h = im.size
    im = im.resize((1000, max(1, round(h * 1000 / w))), Image.BOX)
    p5, p95 = np.percentile(np.asarray(im, dtype=np.float64), [5, 95])
    return float(p95 - p5)


def _contrast_job(args):
    page_id, path, want_sha = args
    got = file_sha(path)
    if got != want_sha:
        raise SystemExit(f"{page_id}: scan sha256 {got} != inventory {want_sha}")
    return page_id, contrast_of(path)


def cmd_contrast(a):
    summary = jsonl(a.summary)
    pilot = [l.strip() for l in open(a.pilot) if l.strip()]
    inv = {r["id"]: r for r in jsonl(a.inventory)}
    jobs = []
    for r in pool(summary, pilot):
        e = inv[inv_id(r["doc_id"])]
        jobs.append((r["doc_id"], str(Path(a.ghn_root) / e["file"]), e["sha256"]))
    out = {}
    with ProcessPoolExecutor(a.workers) as ex:
        for i, (pid, c) in enumerate(ex.map(_contrast_job, jobs, chunksize=4), 1):
            out[pid] = c
            if i % 50 == 0:
                print(f"contrast {i}/{len(jobs)}", file=sys.stderr)
    with open(a.out, "w") as f:
        for pid in sorted(out):
            f.write(json.dumps({"page_id": pid, "contrast": out[pid]}) + "\n")
    print(f"{len(out)} pages", file=sys.stderr)


def crop_scaled(im, x0, y0, x1, y1):
    W, H = im.size
    x0, y0 = max(0, x0 - PAD), max(0, y0 - PAD)
    x1, y1 = min(W, x1 + PAD), min(H, y1 + PAD)
    if x1 <= x0 or y1 <= y0:
        raise ValueError("empty crop")
    c = im.crop((x0, y0, x1, y1))
    w, h = c.size
    s = min(TARGET_H / h, MAX_W / w)
    return c.resize((max(1, round(w * s)), max(1, round(h * s))), Image.LANCZOS), [x0, y0, x1, y1]


def alto_lines(path):
    root = ET.parse(path).getroot()
    ns = root.tag.split("}")[0] + "}" if root.tag.startswith("{") else ""
    page = root.find(f".//{ns}Page")
    size = (float(page.get("WIDTH")), float(page.get("HEIGHT")))
    out = []
    for block in root.iter(f"{ns}TextBlock"):
        for line in block.iter(f"{ns}TextLine"):
            words = [s.get("CONTENT") or "" for s in line.iter(f"{ns}String")]
            box = {k: float(line.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")}
            out.append({"block_lang": block.get("language"), "line_id": line.get("ID"), "box": box,
                        "words": words})
    return out, size


def alto_to_px(box, page_size, image_size):
    sx, sy = image_size[0] / page_size[0], image_size[1] / page_size[1]
    return (round(box["HPOS"] * sx), round(box["VPOS"] * sy),
            round((box["HPOS"] + box["WIDTH"]) * sx), round((box["VPOS"] + box["HEIGHT"]) * sy))


def english_line(words, vocab):
    """True if the line has no syllabary, at least one word of 3+ letters, and every such word
    (stripped of leading and trailing non-letters) is in the word list, case-insensitively."""
    text = " ".join(words)
    if SYLLABARY.search(text) or not text.strip():
        return False
    n = 0
    for tok in text.split():
        core = re.sub(r"^[^A-Za-z]+|[^A-Za-z]+$", "", tok)
        if sum(ch.isascii() and ch.isalpha() for ch in core) < 3:
            continue
        if core.lower() not in vocab:
            return False
        n += 1
    return n >= 1


def cmd_build(a):
    out = Path(a.out_dir)
    got = file_sha(a.summary)
    if got != a.summary_sha256:
        raise SystemExit(f"summary.jsonl sha256 {got} != {a.summary_sha256}")
    summary = jsonl(a.summary)
    pilot = [l.strip() for l in open(a.pilot) if l.strip()]
    pl = pool(summary, pilot)
    contrast = {r["page_id"]: r["contrast"] for r in jsonl(a.contrast)}
    assert set(contrast) == {r["doc_id"] for r in pl}, "contrast file doesn't cover the pool"

    # Faded: the lowest quarter of the pool by contrast (floor(N/4) pages, ties by page_id).
    ranked_c = sorted(contrast, key=lambda p: (contrast[p], p))
    n_faded = len(ranked_c) // 4
    faded = set(ranked_c[:n_faded])
    faded_cut = contrast[ranked_c[n_faded - 1]]

    held = {r["doc_id"]: bool(r["held"]) for r in pl}
    key_order = sorted(held, key=lambda p: sha("2128:clf:" + p))
    base_held = [p for p in key_order if held[p]][:N_HELD]
    base_not = [p for p in key_order if not held[p]][:N_NOT_HELD]
    base = base_held + base_not
    added = []
    n_f = sum(p in faded for p in base)
    for p in key_order:
        if n_f + len(added) >= MIN_FADED:
            break
        if p in faded and p not in base:
            added.append(p)
    pages = sorted(base + added)
    out.mkdir(parents=True, exist_ok=True)
    (out / "sample-pages.txt").write_text("".join(p + "\n" for p in pages))
    pages_sha = file_sha(out / "sample-pages.txt")
    (out / "sample-pages.sha256").write_text(f"{pages_sha}  sample-pages.txt\n")

    strata = {}
    for p in pages:
        s = []
        if p in base_held:
            s.append("held")
        if p in base_not:
            s.append("not_held")
        if p in faded:
            s.append("faded")
        strata[p] = s

    inv = {r["id"]: r for r in jsonl(a.inventory)}
    found = {}
    want = set(pages)
    for l in open(a.found, encoding="utf-8"):
        r = json.loads(l)
        if r["doc_id"] in want:
            # ghn_boxes.py write drops boxes with no area; boxes.jsonl holds the rest, in S1's order.
            found[r["doc_id"]] = [[int(v) for v in x["box"]] for x in r["lines"]
                                  if x["box"][2] > x["box"][0] and x["box"][3] > x["box"][1]]

    items = []   # (shuffle key, key row, image path, pixel box)
    n_called = n_sampled = 0
    for p in pages:
        rows = jsonl(Path(a.boxes_dir) / p.split(":", 1)[1] / "boxes.jsonl")
        head, boxes = rows[0], rows[1:]
        assert head["doc_id"] == p and [b["box"] for b in boxes] == found[p], f"{p}: boxes != found-s1"
        assert len({b["id"] for b in boxes}) == len(boxes)
        e = inv[inv_id(p)]
        img = Path(a.ghn_root) / e["file"]
        if file_sha(img) != head["scan_sha256"]:
            raise SystemExit(f"{p}: scan sha256 differs from boxes.jsonl")
        called = [b for b in boxes if b["chr"] is True]
        other = sorted((b for b in boxes if b["chr"] is not True),
                       key=lambda b: sha("2128:clf-box:" + p + ":" + b["id"]))
        chosen = other[:N_OTHER]
        N_o, n_o = len(other), len(chosen)
        page_info = {"page_id": p, "held_flag": held[p], "strata": strata[p],
                     "added_faded": p in added, "contrast": contrast[p],
                     "n_boxes": len(boxes), "n_called": len(called), "N_notcalled": N_o, "n_sampled": n_o}
        for b, sel in [(b, "called") for b in called] + [(b, "sampled_not_called") for b in chosen]:
            w = 1.0 if sel == "called" else N_o / n_o
            row = {"source": "ghn", **page_info, "box_id": b["id"], "box": b["box"], "called": sel == "called",
                   "p_chr": b.get("p_chr"), "selection": sel, "weight": w}
            items.append((sha("2128:clf-shuffle:" + p + ":" + b["id"]), row, img, tuple(b["box"])))
        n_called += len(called)
        n_sampled += n_o

    # Calibration lines (Amendment 2.1), from LoC pages that aren't GHN sample pages.
    sample_loc = {inv_id(p) for p in pages}
    vocab = {w.strip().lower() for w in open(a.wordlist, encoding="utf-8") if w.strip()}
    # Silver alone has 29 lookalike lines, short of the 60 Amendment 2.1 asks for, so the pool is every
    # aligned line whose reference follows the printed line: silver plus the `wording` and `gap_run`
    # drops (60 lookalikes). `too_different` lines are left out: their reference may not be the line's text.
    aligned = [r for r in jsonl(a.aligned) if r["drop"] in CAL_TIERS and r["page"] not in sample_loc]
    chr_cands = []
    for r in aligned:
        ref = SYLLABARY.findall(r["reference"])
        share = sum(c in LOOKALIKE for c in ref) / len(ref) if ref else 0.0
        pid = "loc:" + r["page"].replace("/", "_")
        chr_cands.append({"page": r["page"], "page_id": pid, "box_id": r["line_id"],
                          "line_id": pid + ":" + r["line_id"], "alto_box": r["box"],
                          "alto_page_size": r["alto_page_size"], "lookalike_share": share,
                          "lookalike": share >= 0.5})
    chr_cands.sort(key=lambda c: sha("2128:clf-cal:" + c["line_id"]))
    look = [c for c in chr_cands if c["lookalike"]][:MIN_LOOKALIKE]
    taken = {c["line_id"] for c in look}
    cal_chr = look + [c for c in chr_cands if c["line_id"] not in taken][:N_CAL_CHR - len(look)]

    lat_cands = []
    cal_pages = sorted({r["page"] for r in jsonl(a.aligned)} - sample_loc)
    for page in cal_pages:
        lccn, date, ed, seq = page.split("/")
        y, m, d = date.split("-")
        lines, size = alto_lines(Path(a.alto_root) / lccn / y / m / d / ed / seq / "ocr.xml")
        pid = "loc:" + page.replace("/", "_")
        for ln in lines:
            if ln["block_lang"] == "chr" or not english_line(ln["words"], vocab):
                continue
            lat_cands.append({"page": page, "page_id": pid, "box_id": ln["line_id"],
                              "line_id": pid + ":" + ln["line_id"], "alto_box": ln["box"],
                              "alto_page_size": list(size)})
    lat_cands.sort(key=lambda c: sha("2128:clf-cal:" + c["line_id"]))
    cal_lat = lat_cands[:N_CAL_LAT]

    img_sizes = {}
    for script, group in (("cherokee", cal_chr), ("latin", cal_lat)):
        for c in group:
            img = Path(a.loc_images) / (c["page"].replace("/", "_") + ".jpg")
            if img not in img_sizes:
                with Image.open(img) as im:
                    img_sizes[img] = im.size
            px = alto_to_px(c["alto_box"], c["alto_page_size"], img_sizes[img])
            row = {"source": "calibration", "page_id": c["page_id"], "box_id": c["box_id"],
                   "line_id": c["line_id"], "known_script": script,
                   "lookalike": c.get("lookalike", False), "lookalike_share": c.get("lookalike_share")}
            items.append((sha("2128:clf-shuffle:" + c["page_id"] + ":" + c["box_id"]), row, img, px))

    # Shuffle, assign opaque ids, cut crops.
    items.sort(key=lambda t: t[0])
    width = len(str(len(items))) if len(items) > 9999 else 4
    by_img = {}
    for i, (_, row, img, px) in enumerate(items):
        by_img.setdefault(img, []).append(i)
    cut = {}
    for img, idx in by_img.items():
        with Image.open(img) as im0:
            im = im0.convert("L")
        for i in idx:
            cut[i] = crop_scaled(im, *items[i][3])
    crops, keyrows = [], []
    for i, (_, row, img, px) in enumerate(items):
        crop, padded = cut[i]
        cid = f"C{i + 1:0{width}d}"
        crops.append((cid, crop))
        keyrows.append({"crop_id": cid, **row, "crop_box_px": padded, "crop_size": list(crop.size),
                        "sheet": f"sheet-{i // PER_SHEET + 1:03d}", "row": i % PER_SHEET + 1})

    try:
        font = ImageFont.truetype(FONT, 30)
    except OSError:
        font = ImageFont.truetype(FONT_FALLBACK, 30)
    sa, sb = out / "sheets-a", out / "sheets-b"
    for d in (sa, sb):
        d.mkdir(exist_ok=True)
    sheet_w = MARGIN + GAP + MAX_W + GAP
    n_sheets = 0
    for s in range(0, len(crops), PER_SHEET):
        chunk = crops[s:s + PER_SHEET]
        rows_h = [max(TARGET_H, c.size[1]) + 2 * GAP for _, c in chunk]
        sheet = Image.new("L", (sheet_w, sum(rows_h) + 1), 255)
        dr = ImageDraw.Draw(sheet)
        y = 0
        for (cid, c), rh in zip(chunk, rows_h):
            dr.rectangle([0, y, MARGIN - 1, y + rh - 1], fill=225)
            tb = dr.textbbox((0, 0), cid, font=font)
            dr.text((12, y + (rh - (tb[3] - tb[1])) // 2 - tb[1]), cid, fill=0, font=font)
            sheet.paste(c, (MARGIN + GAP, y + (rh - c.size[1]) // 2))
            dr.line([0, y, sheet_w, y], fill=120, width=1)
            y += rh
        dr.line([0, y, sheet_w, y], fill=120, width=1)
        n_sheets += 1
        name = f"sheet-{n_sheets:03d}"
        sheet.save(sa / f"{name}.png", optimize=True)
        shutil.copyfile(sa / f"{name}.png", sb / f"{name}.png")
        ids = "".join(cid + "\n" for cid, _ in chunk)
        for d in (sa, sb):
            (d / f"{name}-crop-ids.txt").write_text(ids)

    with open(out / "key.jsonl", "w", encoding="utf-8") as f:
        for r in keyrows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    (out / "key.jsonl").chmod(0o600)

    n_add_f = len(added)
    manifest = {
        "issue": "#2128 Amendments 2 and 2.1", "summary_sha256": got, "pool_pages": len(pl),
        "pool_held": sum(held.values()), "page_id_format": "summary.jsonl doc_id, e.g. ghn:sn83020866_1828-02-21_ed-1_seq-1",
        "box_id_format": "boxes.jsonl id, e.g. r0b2p1l1 (found-s1 tid r0.2.1.1)",
        "cal_line_id_format": "loc:<lccn>_<date>_ed-N_seq-N:<ALTO TextLine ID>",
        "contrast": "greyscale (PIL L), resized to 1000 px wide with Image.BOX, numpy percentile 95 - 5 (linear)",
        "faded_rule": f"lowest floor(N/4) = {n_faded} pool pages by (contrast, page_id); cut contrast {faded_cut:.3f}",
        "sample_pages_sha256": pages_sha,
        "strata_pages": {"held": len(base_held), "not_held": len(base_not),
                         "faded_in_20": n_f, "added_faded": n_add_f, "faded_total": n_f + n_add_f},
        "crops": {"total": len(crops), "ghn_called": n_called, "ghn_sampled_not_called": n_sampled,
                  "cal_cherokee": len(cal_chr), "cal_cherokee_lookalike": sum(c["lookalike"] for c in cal_chr),
                  "cal_latin": len(cal_lat)},
        "cal_candidates": {"cherokee_pool": len(chr_cands),
                           "cherokee_lookalike": sum(c["lookalike"] for c in chr_cands),
                           "latin": len(lat_cands), "pages": len(cal_pages)},
        "cal_rules": {"cherokee": "testset v2 aligned.jsonl, drop in (null, wording, gap_run); lookalike share = lookalike letters / syllabary letters of the reference, >= 0.5",
                      "latin": "ALTO lines outside language=chr blocks, no syllabary, >=1 word of 3+ ASCII letters, "
                               "every such word (ends stripped of non-letters) in the word list, case-insensitive",
                      "wordlist": f"{a.wordlist} (sha256 {file_sha(a.wordlist)})",
                      "excluded_loc_pages_matching_ghn_sample": sorted(sample_loc & set(r['page'] for r in jsonl(a.aligned)))},
        "crop": {"padding_px": PAD, "target_h": TARGET_H, "max_w": MAX_W, "resample": "LANCZOS", "mode": "L"},
        "sheets": {"count": n_sheets, "per_sheet": PER_SHEET},
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, ensure_ascii=False) + "\n")
    print(json.dumps({k: manifest[k] for k in ("sample_pages_sha256", "strata_pages", "crops", "cal_candidates",
                                               "sheets", "faded_rule")}, indent=1))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("contrast")
    for k in ("--summary", "--pilot", "--inventory", "--ghn-root", "--out"):
        c.add_argument(k, required=True)
    c.add_argument("--workers", type=int, default=4)
    b = sub.add_parser("build")
    for k in ("--summary", "--summary-sha256", "--pilot", "--contrast", "--boxes-dir", "--found", "--inventory",
              "--ghn-root", "--aligned", "--loc-images", "--alto-root", "--wordlist", "--out-dir"):
        b.add_argument(k, required=True)
    a = ap.parse_args(argv)
    {"contrast": cmd_contrast, "build": cmd_build}[a.cmd](a)


if __name__ == "__main__":
    main()
