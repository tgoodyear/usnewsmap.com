#!/usr/bin/env python3
"""Trusted words on arm C's bank pages (#2127, amendment 1), cut as word crops for usnm-glyphs.

Each bank page is read by stock frak2021 0.905 (Tesseract 5.5.0, PSM 3, word boxes as TSV) and
matched to LoC's ALTO words by box: a LoC word's partner is the frak2021 word whose box overlaps it
with the highest intersection over union, if that is at least MIN_IOU. The word is trusted when:
  - normalise(LoC word) == normalise(frak2021 word) (frak_norm: ſ folded to s, quotes, dashes);
  - its core (the normalised word with outer non-letters stripped) is letters only, at least
    MIN_LETTERS of them;
  - the core, lower-cased, is in the German word list (build_wordlist.py).
The crop is usnm-glyphs' word_crop: the LoC word's columns and its LoC line's rows, widened by 15% of
the line's height (at least 2 px). Crops go to --out/crops/<page>/<id>.png and words.jsonl lists them
(label = the core) for `usnm-glyphs build-words`.

    trusted_words.py --pages bank-pages/pages.jsonl --scans scans-raw --alto-dir loc \
        --wordlist wordlist-de.txt --tessdata tessdata --out trusted --jobs 20
"""
import argparse, collections, json, os, subprocess, sys, tempfile, time
from multiprocessing import Pool

from lxml import etree
from PIL import Image

from frak_norm import normalise

MIN_IOU = 0.5
MIN_LETTERS = 3
LANG = "frak2021"
Image.MAX_IMAGE_PIXELS = None


def local(tag):
    return tag.rsplit("}", 1)[-1] if isinstance(tag, str) else ""


def iou(a, b):
    ix = min(a[2], b[2]) - max(a[0], b[0])
    iy = min(a[3], b[3]) - max(a[1], b[1])
    if ix <= 0 or iy <= 0:
        return 0.0
    inter = ix * iy
    return inter / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter)


def core_of(word):
    i, j = 0, len(word)
    while i < j and not word[i].isalpha():
        i += 1
    while j > i and not word[j - 1].isalpha():
        j -= 1
    return word[i:j]


def word_crop(wx0, wx1, ly0, ly1, width, height):
    """usnm_glyphs::word_crop."""
    pad = max(0.15 * (ly1 - ly0), 2.0)
    cl = lambda v, hi: int(max(0.0, min(float(hi), v)))
    x0, x1, y0, y1 = cl(wx0 - pad, width), cl(wx1 + pad, width), cl(ly0 - pad, height), cl(ly1 + pad, height)
    return (x0, y0, x1, y1) if x1 > x0 + 2 and y1 > y0 + 2 else None


def loc_words(alto_path, img_w, img_h):
    root = etree.parse(alto_path).getroot()
    page = next(e for e in root.iter() if local(e.tag) == "Page")
    sx, sy = img_w / float(page.get("WIDTH")), img_h / float(page.get("HEIGHT"))
    for line in (e for e in root.iter() if local(e.tag) == "TextLine"):
        ly0 = float(line.get("VPOS")) * sy
        ly1 = ly0 + float(line.get("HEIGHT")) * sy
        for s in line:
            if local(s.tag) != "String":
                continue
            x0, y0 = float(s.get("HPOS")) * sx, float(s.get("VPOS")) * sy
            yield s.get("CONTENT", ""), (x0, y0, x0 + float(s.get("WIDTH")) * sx, y0 + float(s.get("HEIGHT")) * sy), (ly0, ly1)


def frak_words(tsv):
    out = []
    with open(tsv, encoding="utf-8") as f:
        next(f)
        for line in f:
            p = line.rstrip("\n").split("\t")
            if len(p) == 12 and p[0] == "5" and p[11].strip():
                l, t, w, h = map(int, p[6:10])
                out.append((p[11], (l, t, l + w, t + h)))
    return out


WORDS = set()


def load_words(path):
    WORDS.update(open(path, encoding="utf-8").read().split())


def read_page(image, tsv_base, tessdata):
    """frak2021's PSM 3 reading as TSV; written under a temporary name and renamed when complete."""
    part = tsv_base + ".part"
    r = subprocess.run(["tesseract", image, part, "--psm", "3", "-l", LANG, "--tessdata-dir", tessdata, "tsv"],
                       env=dict(os.environ, OMP_THREAD_LIMIT="1"), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if r.returncode == 0:
        os.rename(part + ".tsv", tsv_base + ".tsv")
    elif os.path.exists(part + ".tsv"):
        os.remove(part + ".tsv")
    return r.returncode == 0


def do_page(job):
    r, scans, alto_dir, tessdata, out = job
    words = WORDS
    pid = r["page_id"]
    lccn, date, ed, seq = pid.split("_")[0], r["date"], pid.split("_ed-")[1].split("_")[0], pid.split("_seq-")[1]
    y, m, d = date.split("-")
    alto = os.path.join(alto_dir, lccn, y, m, d, f"ed-{ed}", f"seq-{seq}", "ocr.xml")
    scan = os.path.join(scans, r["scan_path"][len("keep/"):])
    st = collections.Counter(pages=1)
    if not os.path.exists(alto):
        st["no_alto"] += 1
        return pid, st, []
    tsv_base = os.path.join(out, "frak2021-tsv", pid)
    if not os.path.exists(tsv_base + ".tsv"):
        if not read_page(scan, tsv_base, tessdata):
            # Tesseract 5.5.0 crashed on the scan as given (one JP2 did, SIGFPE): try once more
            # from a PNG of the same pixels, else the page gives no words.
            with tempfile.TemporaryDirectory() as td:
                png = os.path.join(td, "page.png")
                Image.open(scan).convert("L").save(png)
                if not read_page(png, tsv_base, tessdata):
                    st["tesseract_failed"] += 1
                    return pid, st, []
            st["read_from_png"] += 1
    im = Image.open(scan)
    im.load()
    im = im.convert("L")
    fw = frak_words(tsv_base + ".tsv")
    # A coarse grid over frak2021 boxes, so each LoC word is compared with its neighbours only.
    cell = 200
    grid = collections.defaultdict(list)
    for k, (_, b) in enumerate(fw):
        for gx in range(b[0] // cell, b[2] // cell + 1):
            for gy in range(b[1] // cell, b[3] // cell + 1):
                grid[(gx, gy)].append(k)
    rows = []
    cdir = os.path.join(out, "crops", pid)
    os.makedirs(cdir, exist_ok=True)
    for wi, (text, box, (ly0, ly1)) in enumerate(loc_words(alto, im.width, im.height)):
        st["loc_words"] += 1
        cands = {k for gx in range(int(box[0]) // cell, int(box[2]) // cell + 1)
                 for gy in range(int(box[1]) // cell, int(box[3]) // cell + 1) for k in grid.get((gx, gy), [])}
        best, bk = 0.0, None
        for k in cands:
            v = iou(box, fw[k][1])
            if v > best:
                best, bk = v, k
        if bk is None or best < MIN_IOU:
            st["no_box_match"] += 1
            continue
        st["box_matched"] += 1
        nl, nf = normalise(text), normalise(fw[bk][0])
        if nl != nf:
            st["readings_differ"] += 1
            continue
        st["agree"] += 1
        core = core_of(nl)
        if len(core) < MIN_LETTERS or not core.isalpha():
            st["short_or_not_letters"] += 1
            continue
        if core.lower() not in words:
            st["not_in_wordlist"] += 1
            continue
        c = word_crop(box[0], box[2], ly0, ly1, im.width, im.height)
        if c is None:
            st["bad_crop"] += 1
            continue
        wid = f"{pid}_w{wi}"
        im.crop(c).save(os.path.join(cdir, wid + ".png"))
        st["trusted"] += 1
        st["trusted_letters"] += len(core)
        rows.append({"id": wid, "page": pid, "crop": os.path.join("crops", pid, wid + ".png"), "label": core,
                     "loc_word": text, "frak_word": fw[bk][0], "iou": round(best, 3), "box": list(c)})
    return pid, st, rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pages", required=True); ap.add_argument("--scans", required=True)
    ap.add_argument("--alto-dir", required=True); ap.add_argument("--wordlist", required=True)
    ap.add_argument("--tessdata", required=True); ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=8, help="a page read takes up to about 1 GB")
    a = ap.parse_args()
    pages = [json.loads(l) for l in open(a.pages)]
    assert len(pages) == 250
    os.makedirs(os.path.join(a.out, "frak2021-tsv"), exist_ok=True)
    jobs = [(r, a.scans, a.alto_dir, a.tessdata, a.out) for r in pages]
    total, t0 = collections.Counter(), time.time()
    per_page = {}
    with Pool(a.jobs, initializer=load_words, initargs=(a.wordlist,)) as pool, open(os.path.join(a.out, "words.jsonl"), "w", encoding="utf-8") as f:
        for n, (pid, st, rows) in enumerate(pool.imap_unordered(do_page, jobs), 1):
            total.update(st)
            per_page[pid] = st.get("trusted", 0)
            for r in rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
            print(f"[trusted] {n}/{len(jobs)} pages, {total['trusted']} trusted words, {time.time() - t0:.0f}s", flush=True)
    summary = {"min_iou": MIN_IOU, "min_letters": MIN_LETTERS, "counts": dict(total),
               "pages_with_trusted": sum(1 for v in per_page.values() if v), "per_page": per_page}
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), indent=1)
    print(json.dumps(summary["counts"]))


if __name__ == "__main__":
    sys.exit(main())
