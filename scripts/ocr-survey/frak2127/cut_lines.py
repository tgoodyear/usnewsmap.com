#!/usr/bin/env python3
"""Cut PAGE-XML text lines into Tesseract line crops for #2127.

Each TextLine with text becomes `<name>.png` (8-bit grey) and `<name>.gt.txt` (the normalised
text, see frak_norm.py). The crop is the bounding box of the line's Coords polygon, padded by
PAD_PX on every side and clipped to the page. A line is skipped when its own TextEquiv is missing
or normalises to nothing (`empty`), or when its polygon has fewer than three points or its clipped
box is under MIN_SIDE_PX on a side (`degenerate`).

Sets and splits (the issue's rule; nothing else decides them):
  cg       Chronicling Germany 66b50f53. train = KoeZ, not Test, not Validation;
           dev = KoeZ Validation; heldout = the 115-page list; the rest is unused.
  rz       Reichsanzeiger-GT (the plain GT-PAGE folder, not the with-TableRegion copy): train.
  nzz      NZZ black-letter ground truth: train.
  onb      ONB/NewsEye Austrian, training and validation folders: train.

The held-out list is checked against its sha256 and against the selection rule recomputed from
split.csv. train and dev go under --out; held-out lines go under --heldout-out, a separate tree.
Training crops over TRAIN_MAX_HEIGHT_PX tall are scaled down to it. Nothing here reads a line
with a model.

    cut_lines.py --data /srv/frak2127/data --heldout heldout-pages.txt \
        --unicharset frak2021.lstm-unicharset --out lines --heldout-out lines-heldout --jobs 12
"""
import argparse, csv, glob, hashlib, json, os, re, subprocess, sys, time
import unicodedata
from multiprocessing import Pool

from lxml import etree
from PIL import Image

from frak_norm import normalise

PAD_PX = 4          # fixed padding around the polygon's bounding box, in page pixels
MIN_SIDE_PX = 5     # a clipped box narrower or shorter than this is degenerate
# Training crops taller than this are scaled down to it (aspect kept, box filter, which is area
# averaging like the area map Tesseract itself uses to shrink a line) so the training set fits the
# CT's disk. 36 px is frak2021's network input height: Tesseract scales every line to it anyway.
# Dev and held-out crops keep the page's own resolution. Training crops are kept only inside .lstmf.
TRAIN_HEIGHT_PX = 36
LSTMF_LINES = 100   # lines per .lstmf (tesseract reloads the file for every page it appends)
START_LANG = "frak2021"
HELDOUT_SHA256 = "6b0c03e714d1d254e7bf144c8d93d3413fc1a2eae6710cdabd5d694c89e97ce5"
YEAR = re.compile(r"_(\d{4})(?:-\d{2}-\d{2})?_")
Image.MAX_IMAGE_PIXELS = None


def local(tag):
    return tag.rsplit("}", 1)[-1] if isinstance(tag, str) else ""


def cg_split(root, heldout):
    """Page stem -> split for Chronicling Germany, after checking the held-out list."""
    rows = {r["Filename"]: r for r in csv.DictReader(open(os.path.join(root, "split.csv")))}
    rule = set()
    for stem, r in rows.items():
        m = YEAR.search(stem + "_")
        if r["Test"] != "1" and r["KoeZ"] != "1" and m and 1830 <= int(m.group(1)) <= 1933:
            rule.add(stem)
    assert rule == heldout, f"held-out list differs from the rule: {len(rule ^ heldout)} pages"
    out = {}
    for stem, r in rows.items():
        if stem in heldout:
            out[stem] = "heldout"
        elif r["Test"] == "1":
            out[stem] = "unused-test"
        elif r["KoeZ"] == "1" and r["Validation"] == "1":
            out[stem] = "dev"
        elif r["KoeZ"] == "1":
            out[stem] = "train"
        else:
            out[stem] = "unused"
    # The separation the issue asks for, asserted on the split table itself.
    for stem, sp in out.items():
        if sp in ("train", "dev"):
            assert stem not in heldout and rows[stem]["Test"] != "1" and rows[stem]["KoeZ"] == "1", stem
    return out


def pages(data, heldout):
    """(set, stem, xml path, image dir, split) for every page of every set."""
    cg = os.path.join(data, "cg")
    for stem, sp in sorted(cg_split(cg, heldout).items()):
        yield "cg", stem, os.path.join(cg, "annotations", stem + ".xml"), os.path.join(cg, "images"), sp
    for p in sorted(glob.glob(os.path.join(data, "reichsanzeiger/gt/*/data/reichsanzeiger-1820-1939/GT-PAGE/*.xml"))):
        yield "rz", os.path.basename(p)[:-4], p, os.path.join(data, "reichsanzeiger/images"), "train"
    for p in sorted(glob.glob(os.path.join(data, "nzz/nzz/*/xml/NZZ_groundtruth/*.xml"))):
        yield "nzz", os.path.basename(p)[:-4], p, os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(p))), "img"), "train"
    for p in sorted(glob.glob(os.path.join(data, "austrian/*/*/*.xml"))):
        yield "onb", os.path.basename(p)[:-4], p, os.path.dirname(p), "train"


def line_text(tl):
    """The line's own TextEquiv (index 0 if several), not its words'."""
    eqs = [e for e in tl if local(e.tag) == "TextEquiv"]
    eqs.sort(key=lambda e: int(e.get("index", "0") or 0))
    for e in eqs:
        for u in e:
            if local(u.tag) == "Unicode" and u.text:
                return u.text
    return None


def write_lstmf(base, part, tessdata):
    """One multi-page TIFF of line crops plus a WordStr box per page (chr_finetune.sh's line box,
    with the page number), read by `tesseract --psm 13 lstm.train` into base.lstmf."""
    part[0][1].save(base + ".tif", save_all=True, append_images=[c for _, c, _ in part[1:]], compression="tiff_deflate")
    with open(base + ".box", "w", encoding="utf-8") as f:
        for i, (_, c, text) in enumerate(part):
            w, h = c.size
            f.write(f"WordStr 0 0 {w} {h} {i} #{text}\n\t {w} {h} {w + 1} {h + 1} {i}\n")
    subprocess.run(["tesseract", base + ".tif", base, "--psm", "13", "-l", START_LANG, "--tessdata-dir", tessdata,
                    "lstm.train"], check=True, env=dict(os.environ, OMP_THREAD_LIMIT="1"),
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    os.remove(base + ".tif")
    os.remove(base + ".box")


def lstmf_lines(path):
    """Number of lines in an .lstmf (a DocumentData: a count, then the pages)."""
    with open(path, "rb") as f:
        return int.from_bytes(f.read(4), "little")


def cut_page(job):
    st, stem, xml, imgdir, split, outdir, lstmf_dir, tessdata, charset = job
    for_lstmf, lstmf_of = [], {}
    stats = {"lines": 0, "kept": 0, "chars": 0, "empty": 0, "degenerate": 0, "unencodable": 0, "unencodable_chars": 0}
    rows = []
    root = etree.parse(xml).getroot()
    page_el = next(e for e in root.iter() if local(e.tag) == "Page")
    img_path = os.path.join(imgdir, page_el.get("imageFilename"))
    if not os.path.exists(img_path):
        alt = glob.glob(os.path.join(imgdir, stem + ".*"))
        assert alt, f"no image for {st}:{stem}"
        img_path = alt[0]
    im = None
    if split != "train":
        os.makedirs(outdir, exist_ok=True)
    k = 0
    for tl in (e for e in root.iter() if local(e.tag) == "TextLine"):
        k += 1
        stats["lines"] += 1
        raw = line_text(tl)
        text = normalise(raw) if raw else ""
        if not text:
            stats["empty"] += 1
            continue
        coords = next((e for e in tl if local(e.tag) == "Coords"), None)
        pts = [tuple(int(float(v)) for v in p.split(",")) for p in (coords.get("points", "").split() if coords is not None else [])]
        if im is None:
            im = Image.open(img_path)
            im.load()
            im = im.convert("L")
            sx = im.width / float(page_el.get("imageWidth") or im.width)
            sy = im.height / float(page_el.get("imageHeight") or im.height)
        if len(pts) < 3:
            stats["degenerate"] += 1
            continue
        xs, ys = [p[0] * sx for p in pts], [p[1] * sy for p in pts]
        box = (max(0, int(min(xs)) - PAD_PX), max(0, int(min(ys)) - PAD_PX),
               min(im.width, int(max(xs)) + 1 + PAD_PX), min(im.height, int(max(ys)) + 1 + PAD_PX))
        if box[2] - box[0] < MIN_SIDE_PX or box[3] - box[1] < MIN_SIDE_PX:
            stats["degenerate"] += 1
            continue
        name = f"{st}__{re.sub(r'[^A-Za-z0-9_.-]', '_', stem)}__{k:04d}"
        crop = im.crop(box)
        bad = sorted({c for c in unicodedata.normalize("NFD", text) if c != " " and c not in charset})
        stats["kept"] += 1
        stats["chars"] += len(text)
        if bad:
            stats["unencodable"] += 1
            stats["unencodable_chars"] += len(text)
        if split == "train":
            if crop.height > TRAIN_HEIGHT_PX:
                crop = crop.resize((max(1, round(crop.width * TRAIN_HEIGHT_PX / crop.height)), TRAIN_HEIGHT_PX), Image.BOX)
                stats["scaled"] = stats.get("scaled", 0) + 1
        else:
            crop.save(os.path.join(outdir, name + ".png"))
            with open(os.path.join(outdir, name + ".gt.txt"), "w", encoding="utf-8") as f:
                f.write(text + "\n")
        if split in ("train", "dev") and not bad:
            for_lstmf.append((name, crop, text))
        rows.append({"name": name, "set": st, "page": stem, "split": split, "line_id": tl.get("id"),
                     "box": box, "chars": len(text), "unencodable": "".join(bad), "text": text})
    # Training and dev lines go to .lstmf files of at most LSTMF_LINES lines each.
    if for_lstmf:
        ldir = os.path.join(lstmf_dir, split)
        os.makedirs(ldir, exist_ok=True)
        for j in range(0, len(for_lstmf), LSTMF_LINES):
            part = for_lstmf[j:j + LSTMF_LINES]
            base = os.path.join(ldir, f"{part[0][0]}__{len(part)}")
            write_lstmf(base, part, tessdata)
            got = lstmf_lines(base + ".lstmf")
            stats["lstmf_lines"] = stats.get("lstmf_lines", 0) + got
            for name, _, _ in part:
                lstmf_of[name] = os.path.basename(base) + ".lstmf"
    for r in rows:
        r["lstmf"] = lstmf_of.get(r["name"])
    return st, stem, split, stats, rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", required=True)
    ap.add_argument("--heldout", required=True)
    ap.add_argument("--unicharset", required=True, help="the start model's lstm-unicharset")
    ap.add_argument("--out", required=True)
    ap.add_argument("--heldout-out", required=True)
    ap.add_argument("--lstmf-out", required=True, help="train/ and dev/ .lstmf files go here")
    ap.add_argument("--tessdata", required=True, help="holds frak2021.traineddata and configs/ (for lstm.train)")
    ap.add_argument("--jobs", type=int, default=12)
    a = ap.parse_args()
    raw = open(a.heldout, "rb").read()
    assert hashlib.sha256(raw).hexdigest() == HELDOUT_SHA256, "held-out list sha256 mismatch"
    heldout = set(raw.decode().split())
    assert len(heldout) == 115
    assert os.path.realpath(a.out) != os.path.realpath(a.heldout_out)
    assert "heldout" not in os.path.realpath(a.lstmf_out)
    for d in (a.out, a.heldout_out, a.lstmf_out):
        if os.path.isdir(d) and os.listdir(d):
            sys.exit(f"{d} is not empty; give a new directory (nothing is deleted)")
    charset = set()
    with open(a.unicharset, encoding="utf-8") as f:
        next(f)
        for line in f:
            charset.update(unicodedata.normalize("NFD", line.split(" ", 1)[0]))
    jobs = []
    for st, stem, xml, imgdir, split in pages(a.data, heldout):
        if split.startswith("unused"):
            continue
        base = a.heldout_out if split == "heldout" else a.out
        jobs.append((st, stem, xml, imgdir, split, os.path.join(base, split), a.lstmf_out, a.tessdata, charset))
    os.makedirs(a.out, exist_ok=True)
    os.makedirs(a.heldout_out, exist_ok=True)
    idx = {d: open(os.path.join(d, "lines.jsonl"), "w", encoding="utf-8") for d in (a.out, a.heldout_out)}
    summary, page_split, t0 = {}, {}, time.time()
    with Pool(a.jobs) as pool:
        for n, (st, stem, split, stats, rows) in enumerate(pool.imap_unordered(cut_page, jobs), 1):
            page_split[f"{st}:{stem}"] = split
            agg = summary.setdefault(f"{st}/{split}", {"pages": 0})
            agg["pages"] += 1
            for k, v in stats.items():
                agg[k] = agg.get(k, 0) + v
            f = idx[a.heldout_out if split == "heldout" else a.out]
            for r in rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
            if n % 25 == 0 or n == len(jobs):
                print(f"[cut] {n}/{len(jobs)} pages, {time.time() - t0:.0f}s", flush=True)
    for f in idx.values():
        f.close()
    # No train or dev line may come from a held-out or official-Test page.
    test_pages = {r["Filename"] for r in csv.DictReader(open(os.path.join(a.data, "cg", "split.csv"))) if r["Test"] == "1"}
    with open(os.path.join(a.out, "lines.jsonl"), encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            assert r["split"] in ("train", "dev"), r
            if r["set"] == "cg":
                assert r["page"] not in heldout and r["page"] not in test_pages, r
    with open(os.path.join(a.heldout_out, "lines.jsonl"), encoding="utf-8") as f:
        assert {json.loads(l)["page"] for l in f} <= heldout
    out = {"pad_px": PAD_PX, "min_side_px": MIN_SIDE_PX, "train_height_px": TRAIN_HEIGHT_PX, "lstmf_lines": LSTMF_LINES, "heldout_sha256": HELDOUT_SHA256, "by_set_split": summary}
    for d in (a.out, a.heldout_out):
        json.dump(out, open(os.path.join(d, "summary.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(out, indent=1, sort_keys=True))


if __name__ == "__main__":
    sys.exit(main())
