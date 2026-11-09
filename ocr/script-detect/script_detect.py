#!/usr/bin/env python3
"""Per-word script detection for mixed-script lines, Cherokee first (#1666).

    script_detect.py survey --alto-dir bank/loc --vocab chr-attested.txt --out survey.json
    script_detect.py words --alto-dir bank/loc --images bank/images --images testset/v2/images \
        --vocab chr-attested.txt --exclude-manifest testset/v1/manifest.json --out words.jsonl
    script_detect.py ocr --words words.jsonl --images bank/images --tessdata DIR --lang eng --out ocr-eng.jsonl
    script_detect.py ocr --testset-lines testset/v2/crops/lines.jsonl --crops testset/v2/crops ...
    script_detect.py features --words words.jsonl --images bank/images --ocr eng=ocr-eng.jsonl \
        --ocr chr=ocr-chr_phoenix.jsonl --vocab chr-attested.txt --english english-words.txt --out feats
    script_detect.py synth --words words.jsonl --alto-dir bank/loc --images bank/images \
        --english english-words.txt --exclude-manifest testset/v1/manifest.json --lines 3000 --seed 1666 --out synth
    script_detect.py glyph-donors --alto-dir bank/loc --images bank/images --exclude-manifest testset/v1/manifest.json --out donors
    script_detect.py glyph-crops --feats feats --words words.jsonl --images bank/images   (and synth-feats with synth/words.jsonl)
    script_detect.py glyph-cnn --feats feats --donors donors --hand labels/hand.jsonl --score feats --score synth-feats --out cnn
    script_detect.py train --feats feats --feats synth-feats --hand labels/hand.jsonl --out model
    script_detect.py route --model model/model.pkl --feats feats --words words.jsonl \
        --testset-lines testset/v2/crops/lines.jsonl --crops testset/v2/crops --tessdata tessdata \
        --chr-lang chr_phoenix --base-pred chr-psm13.jsonl --out routed.jsonl
    script_detect.py score --testset-dir testset/v2 --checked checked.json --pred chr=chr-psm13.jsonl --pred routed=routed.jsonl \
        [--compare chr:routed --bootstrap 10000]
    script_detect.py sheet --words sample.jsonl --images bank/images --out-dir sheets [--rows 20]
"""
from __future__ import annotations

import argparse
import collections
import json
import math
import os
import re
import sys

import sd_alto

PUNCT = ".,;:!?\"'“”‘’()[]-—*"
DIGITS = re.compile(r"[0-9]")


def token_kind(text, vocab):
    core = text.strip(PUNCT)
    if not core:
        return "punct"
    if sd_alto.LATIN.search(core) and not sd_alto.SYL.search(core):
        return "latin"
    if sd_alto.LATIN.search(core):
        return "mixed"
    if DIGITS.search(core) and not sd_alto.SYL.search(core):
        return "digits"
    if sd_alto.SYL.search(core):
        return "syl-attested" if core in vocab else "syl-other"
    return "other"


def cmd_survey(a):
    vocab = {l.strip() for l in open(a.vocab, encoding="utf-8") if l.strip()}
    kinds = collections.Counter()
    paren = collections.Counter()
    pages = lines = 0
    examples = collections.defaultdict(list)
    for slug, path in sd_alto.alto_pages(a.alto_dir):
        blocks = sd_alto.read_page(path, 1.0)
        pages += 1
        for b in blocks:
            for ln in b["lines"]:
                if not sd_alto.syllabary_line(ln):
                    continue
                lines += 1
                for w in ln["words"]:
                    k = token_kind(w["text"], vocab)
                    kinds[k] += 1
                    if "(" in w["text"] or ")" in w["text"]:
                        paren[k] += 1
                    if len(examples[k]) < 40:
                        examples[k].append(w["text"])
    out = {"pages": pages, "syllabary_lines": lines, "token_kinds": kinds, "in_parentheses": paren,
           "examples": examples}
    json.dump(out, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(json.dumps({k: v for k, v in out.items() if k != "examples"}, ensure_ascii=False))


def cmd_words(a):
    """Every word of every syllabary line, with its line, the lines either side and its block."""
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    vocab = {l.strip() for l in open(a.vocab, encoding="utf-8") if l.strip()}
    test = sd_alto.excluded_pages(a.exclude_manifest)
    n = collections.Counter()
    with open(a.out, "w", encoding="utf-8") as out:
        for slug, path in sd_alto.alto_pages(a.alto_dir):
            img = sd_alto.image_path(a.images, slug)
            if img is None:
                n["no_image"] += 1
                continue
            with Image.open(img) as im:
                width, height = im.size
            n["pages"] += 1
            for b in sd_alto.read_page(path, width):
                lines = b["lines"]
                bw = [w["text"] for ln in lines for w in ln["words"]]
                block_syl = sum(1 for t in bw if sd_alto.syllabary_word(t)) / len(bw)
                for i, ln in enumerate(lines):
                    if not sd_alto.syllabary_line(ln):
                        continue
                    n["lines"] += 1
                    prev = lines[i - 1] if i > 0 else None
                    nxt = lines[i + 1] if i + 1 < len(lines) else None
                    for j, w in enumerate(ln["words"]):
                        n["words"] += 1
                        out.write(json.dumps({
                            "page": slug, "split": "test" if slug in test else "train",
                            "page_size": [width, height], "block": b["id"], "block_box": b["box"],
                            "line": ln["id"], "line_index": i, "line_box": ln["box"],
                            "prev_box": prev and prev["box"], "next_box": nxt and nxt["box"],
                            "prev_loc": prev and " ".join(x["text"] for x in prev["words"]),
                            "next_loc": nxt and " ".join(x["text"] for x in nxt["words"]),
                            "block_lines": len(lines), "block_syllabary": round(block_syl, 3),
                            "word": w["id"], "word_index": j,
                            "words_in_line": len(ln["words"]), "box": w["box"], "loc": w["text"],
                            "kind": token_kind(w["text"], vocab),
                            "line_loc": " ".join(x["text"] for x in ln["words"]),
                        }, ensure_ascii=False) + "\n")
    print(json.dumps(n))


def cmd_sheet(a):
    """Contact sheets for labelling by eye: each row is a word's line with the word boxed and numbered."""
    from PIL import Image, ImageDraw
    Image.MAX_IMAGE_PIXELS = None
    rows = [json.loads(l) for l in open(a.words, encoding="utf-8")]
    os.makedirs(a.out_dir, exist_ok=True)
    pages = {}
    W, H = a.width, a.row_height
    for s0 in range(0, len(rows), a.rows):
        chunk = rows[s0:s0 + a.rows]
        sheet = Image.new("RGB", (W + 60, H * len(chunk)), "white")
        d = ImageDraw.Draw(sheet)
        for k, r in enumerate(chunk):
            if r["page"] not in pages:
                pages.clear()
                pages[r["page"]] = Image.open(sd_alto.image_path(a.images, r["page"])).convert("RGB")
            pg = pages[r["page"]]
            lx0, ly0, lx1, ly1 = r["line_box"]
            wx0, wy0, wx1, wy1 = r["box"]
            # A window of the line around the word, so the word is large enough to read.
            h = max(ly1, wy1) - min(ly0, wy0)
            half = int(h * W / H / 2)
            cx = (wx0 + wx1) // 2
            x0, x1 = max(0, cx - half), cx + half
            y0, y1 = min(ly0, wy0) - h // 6, max(ly1, wy1) + h // 6
            crop = pg.crop((x0, y0, x1, y1))
            sc = H / crop.height
            crop = crop.resize((max(1, int(crop.width * sc)), H))
            dc = ImageDraw.Draw(crop)
            dc.rectangle([(wx0 - x0) * sc - 2, (wy0 - y0) * sc - 2, (wx1 - x0) * sc + 2, (wy1 - y0) * sc + 2],
                         outline="red", width=2)
            sheet.paste(crop.crop((0, 0, W, H)), (60, k * H))
            d.text((4, k * H + H // 3), str(s0 + k), fill="blue")
            d.line([(0, k * H), (W + 60, k * H)], fill="gray")
        sheet.save(os.path.join(a.out_dir, f"sheet-{s0:05d}.png"))
    print(len(rows))


LINE_PAD = 0.2  # of the line height, above and below, as the training lines are cut


def tesseract_words(png, tessdata, lang, psm):
    """Tesseract's words on one image (TSV level 5): text, confidence and box in the image's pixels."""
    import subprocess
    env = dict(os.environ, OMP_THREAD_LIMIT="1")
    run = subprocess.run(["tesseract", png, "-", "--psm", str(psm), "--oem", "1", "--tessdata-dir", tessdata,
                          "-l", lang, "-c", "tessedit_create_tsv=1"], capture_output=True, text=True, env=env)
    if run.returncode != 0:  # a missing model or a bad image must not be saved as an empty reading
        raise RuntimeError(f"tesseract failed on {png} ({lang}): {run.stderr.strip()[-500:]}")
    out = run.stdout
    words = []
    for row in out.splitlines()[1:]:
        f = row.split("\t")
        if len(f) == 12 and f[0] == "5" and f[11].strip():
            x, y, w, h = map(int, f[6:10])
            words.append({"text": f[11], "conf": float(f[10]), "box": [x, y, x + w, y + h]})
    return words


def line_jobs(a):
    """(key, page, crop box or None, png path or None) for each line to read."""
    if a.words:
        seen = set()
        for l in open(a.words, encoding="utf-8"):
            r = json.loads(l)
            key = r["page"] + "#" + r["line"]
            if key in seen:
                continue
            seen.add(key)
            if r.get("png"):
                yield key, r["page"], [0, 0, 0, 0], r["png"]
                continue
            x0, y0, x1, y1 = r["line_box"]
            pad = max(4, int(LINE_PAD * (y1 - y0)))
            yield key, r["page"], [max(0, x0 - 4), max(0, y0 - pad), x1 + 4, y1 + pad], None
    else:
        for l in open(a.testset_lines, encoding="utf-8"):
            r = json.loads(l)
            yield r["page"] + "#" + r["line_id"], r["page"], r["pixel_box"], os.path.join(a.crops, r["crop"])


def cmd_ocr(a):
    """One model over every line crop; word boxes come back in page pixels. Resumable."""
    import concurrent.futures as cf
    import tempfile
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None
    done = set()
    if os.path.exists(a.out):
        done = {json.loads(l)["key"] for l in open(a.out, encoding="utf-8")}
    jobs = [j for j in line_jobs(a) if j[0] not in done]
    tmp = tempfile.mkdtemp(prefix="sd-ocr-")
    page_cache = {}

    def crop(job):
        key, page, box, png = job
        if png:
            return png
        if page not in page_cache:
            page_cache.clear()
            page_cache[page] = Image.open(sd_alto.image_path(a.images, page)).convert("L")
        p = os.path.join(tmp, key.replace("/", "_").replace("#", "_") + ".png")
        page_cache[page].crop(box).save(p)
        return p

    def run(job, png):
        words = tesseract_words(png, a.tessdata, a.lang, a.psm)
        x0, y0 = job[2][0], job[2][1]
        for w in words:
            b = w["box"]
            w["box"] = [b[0] + x0, b[1] + y0, b[2] + x0, b[3] + y0]
        if not job[3]:
            os.remove(png)
        return {"key": job[0], "words": words}

    with open(a.out, "a", encoding="utf-8") as f, cf.ThreadPoolExecutor(a.jobs) as ex:
        pending = []
        for job in jobs:  # pages are cut in order on this thread; tesseract runs in the pool
            pending.append(ex.submit(run, job, crop(job)))
            while len(pending) > 4 * a.jobs:
                f.write(json.dumps(pending.pop(0).result(), ensure_ascii=False) + "\n")
                f.flush()
        for p in pending:
            f.write(json.dumps(p.result(), ensure_ascii=False) + "\n")
    print(len(done) + len(jobs))


def load_ocr(path):
    return {r["key"]: r["words"] for r in map(json.loads, open(path, encoding="utf-8"))}


def assign(loc_words, tess_words):
    """Each Tesseract word goes to the LoC word it overlaps most horizontally; a LoC word's reading
    is its words' texts joined and their width-weighted confidence (-1 when nothing landed on it)."""
    got = [[] for _ in loc_words]
    for t in tess_words:
        tx0, _, tx1, _ = t["box"]
        best, bi = 0, -1
        for i, w in enumerate(loc_words):
            ov = min(tx1, w["box"][2]) - max(tx0, w["box"][0])
            if ov > best:
                best, bi = ov, i
        if bi >= 0:
            got[bi].append(t)
    out = []
    for g in got:
        if not g:
            out.append(("", -1.0))
            continue
        wsum = sum(max(1, t["box"][2] - t["box"][0]) for t in g)
        out.append(("".join(t["text"] for t in g),
                    sum(t["conf"] * max(1, t["box"][2] - t["box"][0]) for t in g) / wsum))
    return out


def cmd_features(a):
    """The feature matrix for every word (sd_features.NAMES), and a metadata line per word."""
    import numpy as np
    from PIL import Image
    import sd_features as F
    Image.MAX_IMAGE_PIXELS = None
    vocab = {l.strip() for l in open(a.vocab, encoding="utf-8") if l.strip()}
    english = {l.strip().lower() for l in open(a.english, encoding="utf-8") if l.strip()}
    ocr = dict(kv.split("=", 1) for kv in a.ocr)
    eng, chr_ = load_ocr(ocr["eng"]), load_ocr(ocr["chr"])
    by_line = collections.OrderedDict()
    for l in open(a.words, encoding="utf-8"):
        r = json.loads(l)
        by_line.setdefault(r["page"] + "#" + r["line"], []).append(r)
    os.makedirs(a.out, exist_ok=True)
    X, meta = [], open(os.path.join(a.out, "meta.jsonl"), "w", encoding="utf-8")
    page_id, page = None, None
    crop_cache, block_cache, gray_cache, bq_cache = {}, {}, {}, {}

    def block_quality(box, H):
        k = tuple(box)
        if k not in bq_cache:
            x0, y0, x1, y1 = box
            g = page[y0:y1, x0:x1]
            bq_cache[k] = F.quality(g, F.binarize(g), H)
        return bq_cache[k]

    def line_crop(box):
        x0, y0, x1, y1 = box
        pad = max(4, int(LINE_PAD * (y1 - y0)))
        X0, Y0 = max(0, x0 - 4), max(0, y0 - pad)
        g = page[Y0:y1 + pad, X0:x1 + 4]
        gray_cache[tuple(box)] = g
        ink = F.binarize(g)
        top, bot = F.body(ink)
        return ink, top, bot, X0, Y0

    def line_info(box):
        k = tuple(box)
        if k not in crop_cache:
            ink, top, bot, X0, Y0 = line_crop(box)
            crop_cache[k] = (ink, top, bot, X0, Y0, F.shape(ink, top, bot))
        return crop_cache[k]

    def block_info(box, H):
        k = tuple(box)
        if k not in block_cache:
            x0, y0, x1, y1 = box
            ink = F.binarize(page[y0:y1, x0:x1])
            boxes, sizes = F.components(ink)
            h = boxes[:, 2] - boxes[:, 0] if len(boxes) else np.zeros(0)
            h = h[h >= 6]
            if len(h) < 5:
                block_cache[k] = (0.0, 0.0, 0.0)
            else:
                ref = np.percentile(h, 75)
                rows = ink.sum(axis=1)
                block_cache[k] = (float(np.mean(h < 0.7 * ref)), float(np.median(h)),
                                  float(np.std(rows) / (np.mean(rows) + 1e-6)))
        short, med_h, rowvar = block_cache[k]
        return short, rowvar, med_h / H if H else 0.0

    n = 0
    for key, words in by_line.items():
        pg = words[0]["page"]
        if pg != page_id:
            page_id = pg
            page = np.asarray(Image.open(sd_alto.image_path(a.images, pg)).convert("L"))
            crop_cache.clear()
            block_cache.clear()
            gray_cache.clear()
            bq_cache.clear()
        r0 = words[0]
        if r0.get("png"):  # a synthetic line: its own image, word boxes in its pixels
            lgray = np.asarray(Image.open(r0["png"]).convert("L"))
            ink = F.binarize(lgray)
            top, bot = F.body(ink)
            X0 = Y0 = 0
            lsh = F.shape(ink, top, bot)
        else:
            ink, top, bot, X0, Y0, lsh = line_info(r0["line_box"])
            lgray = gray_cache[tuple(r0["line_box"])]
        H = max(4.0, float(bot - top))
        lq = F.quality(lgray, ink, H)
        bq = block_quality(r0["block_box"], H)
        reads = {"eng": assign(words, eng.get(key, [])), "chr": assign(words, chr_.get(key, []))}
        rm = {m: [F.reading(t, c, vocab, english) for t, c in reads[m]] for m in reads}
        lm = [F.loc_measures(w["loc"], vocab) for w in words]
        side = {}
        for sname, bkey, lkey in (("prev", "prev_box", "prev_loc"), ("next", "next_box", "next_loc")):
            if r0[bkey]:
                sink, stop, sbot, *_rest, ssh = line_info(r0[bkey])
                side[sname] = (ssh, max(4.0, float(sbot - stop)) / H)
            else:
                side[sname] = (dict.fromkeys(F.SHAPE, 0.0), 0.0)
            side[sname + "_loc"] = F.line_loc_measures(r0[lkey], vocab)
        bshort, browvar, bh = block_info(r0["block_box"], H)
        nw = len(words)
        line_ctx = [np.mean([m["english"] for m in rm["eng"]]), np.mean([m["lower"] for m in rm["eng"]]),
                    np.mean([m["cherokee"] for m in rm["chr"]]), np.mean([m["loc_attested"] for m in lm]),
                    np.mean([m["conf"] for m in rm["eng"]]), np.mean([m["conf"] for m in rm["chr"]]), float(nw)]
        lx0, lx1 = r0["line_box"][0], r0["line_box"][2]
        for j, w in enumerate(words):
            wx0, wy0, wx1, wy1 = w["box"]
            c0, c1 = max(0, wx0 - X0), max(0, wx1 - X0)
            wsh = F.shape(ink[:, c0:max(c1, c0 + 1)], top, bot)
            gb = (wx0 - words[j - 1]["box"][2]) / H if j > 0 else -1.0
            ga = (words[j + 1]["box"][0] - wx1) / H if j + 1 < nw else -1.0
            row = [wsh[k] for k in F.SHAPE]
            row += [(wy1 - wy0) / H, (wx1 - wx0) / H, gb, ga, (wx0 - lx0) / max(1, lx1 - lx0), j / max(1, nw - 1)]
            row += [lsh[k] for k in F.SHAPE]
            row += [wsh[k] - lsh[k] for k in ("upper", "short", "tall", "desc", "comp_w", "gap", "density", "n_per_h")]
            for sname in ("prev", "next"):
                ssh, hrel = side[sname]
                row += [ssh[k] for k in F.SHAPE] + [hrel]
                row += [side[sname + "_loc"][k] for k in ("syl", "attested", "latin", "present")]
            row += [bshort, browvar, bh, r0["block_syllabary"], float(r0["block_lines"])]
            wq = F.quality(lgray[:, c0:max(c1, c0 + 1)], ink[:, c0:max(c1, c0 + 1)], H)
            row += [wq[k] for k in F.QUALITY] + [lq[k] for k in F.QUALITY] + [bq[k] for k in F.QUALITY]
            for m in ("eng", "chr"):
                row += [rm[m][j][k] for k in F.READING]
            row += [rm["eng"][j]["conf"] - rm["chr"][j]["conf"]]
            row += [lm[j][k] for k in F.LOC]
            for d in (-1, 1):
                i = j + d
                if 0 <= i < nw:
                    row += [1.0, rm["eng"][i]["english"], rm["eng"][i]["lower"], rm["chr"][i]["cherokee"],
                            lm[i]["loc_attested"], lm[i]["loc_paren"], rm["eng"][i]["conf"], rm["chr"][i]["conf"]]
                else:
                    row += [0.0] * 8
            # From the eng readings only: LoC's text is never an input.
            left_open = (j > 0 and reads["eng"][j - 1][0].endswith("(")) or reads["eng"][j][0].startswith("(")
            right_close = reads["eng"][j][0].rstrip(".,;:").endswith(")")
            row += [float(left_open), float(right_close)]
            row += line_ctx
            assert len(row) == len(F.NAMES), (len(row), len(F.NAMES))
            X.append(row)
            meta.write(json.dumps({"page": w["page"], "word": w["word"], "line": key, "split": w["split"],
                                   "synth": w.get("label"), "donor_page": w.get("donor_page"),
                                   "kind": w["kind"], "loc": w["loc"], "eng": reads["eng"][j][0],
                                   "eng_conf": round(reads["eng"][j][1], 1), "chr": reads["chr"][j][0],
                                   "chr_conf": round(reads["chr"][j][1], 1)}, ensure_ascii=False) + "\n")
            n += 1
    np.save(os.path.join(a.out, "X.npy"), np.asarray(X, dtype=np.float32))
    json.dump(F.NAMES, open(os.path.join(a.out, "names.json"), "w"))
    print(json.dumps({"words": n, "features": len(F.NAMES)}))


def block_pitch(lines):
    """Median distance between the tops of consecutive lines of a block: the type's body size."""
    ys = sorted(ln["box"][1] for ln in lines)
    d = sorted(b - a for a, b in zip(ys, ys[1:]) if b > a)
    return d[len(d) // 2] if d else None


def cmd_synth(a):
    """Syllabary lines with real Latin words from the same paper pasted in (#1666).

    Donors are words of English lines on the host's own page where it has any, else another
    training page: that LoC read as English words (or
    capitalised names), cut with their printed punctuation; one word or a run of two to four
    consecutive words replaces as many host words at a random place in a syllabary line. The donor
    is scaled so its capital height matches the host's letter height (times a random factor from
    0.85 to 1.5), its baseline set on the host's, and its paper and ink tones mapped onto the host's. Host words keep their label from the
    page; the pasted words are Latin. The new lines are written as `<out>/lines/*.png` and their words
    to `<out>/words.jsonl` in the `words` format, with `png` and `label` set.
    """
    import random
    import numpy as np
    from PIL import Image
    import sd_features as F
    Image.MAX_IMAGE_PIXELS = None
    rng = random.Random(a.seed)
    english = {l.strip().lower() for l in open(a.english, encoding="utf-8") if l.strip()}
    test = sd_alto.excluded_pages(a.exclude_manifest)
    os.makedirs(os.path.join(a.out, "lines"), exist_ok=True)
    # Donor runs, by page: consecutive trusted English words on lines with no syllabary.
    donors = collections.defaultdict(list)
    for slug, path in sd_alto.alto_pages(a.alto_dir):
        if slug in test or not sd_alto.image_path(a.images, slug):
            continue
        with Image.open(sd_alto.image_path(a.images, slug)) as im:
            width = im.size[0]
        for b in sd_alto.read_page(path, width):
            pitch = block_pitch(b["lines"])
            if not pitch:
                continue
            for ln in b["lines"]:
                ws = ln["words"]
                if any(sd_alto.syllabary_word(w["text"]) for w in ws) or len(ws) < 4:
                    continue

                def ok(w):
                    core = re.sub(r"[^A-Za-z]", "", w["text"])
                    return (len(core) >= 2 and core.lower() in english) or (len(core) >= 3 and core.istitle())
                run = []
                for w in ws + [None]:
                    if w is not None and ok(w):
                        run.append(w)
                        continue
                    for i in range(len(run)):
                        for k in range(1, 5):
                            if i + k > len(run):
                                continue
                            core = re.sub(r"[^A-Za-z]", "", run[i]["text"])
                            if k == 1 and len(core) < 4 and not core[:1].isupper():
                                continue  # a lone short word: see the docstring
                            donors[slug].append({"words": run[i:i + k], "line_box": ln["box"], "pitch": pitch})
                    run = []
    donor_pages = sorted(donors)
    hosts = collections.defaultdict(list)
    for l in open(a.words, encoding="utf-8"):
        r = json.loads(l)
        if r["split"] == "train":
            hosts[r["page"] + "#" + r["line"]].append(r)
    # Hosts page by page (each page's scan is decoded once), an even share of the lines per page.
    host_keys = [k for k, v in hosts.items() if len(v) >= 3]
    rng.shuffle(host_keys)
    per_page = collections.defaultdict(list)
    for k in host_keys:
        per_page[hosts[k][0]["page"]].append(k)
    share = 2 * a.lines // max(1, len(per_page)) + 1
    host_keys = [k for pg in sorted(per_page) for k in per_page[pg][:share]]
    page_cache = {}

    def page(slug):
        if slug not in page_cache:
            if len(page_cache) > 3:
                page_cache.pop(next(iter(page_cache)))
            page_cache[slug] = np.asarray(Image.open(sd_alto.image_path(a.images, slug)).convert("L"))
        return page_cache[slug]

    def tones(g):
        return float(np.percentile(g, 3)), float(np.percentile(g, 60))

    made = 0
    out = open(os.path.join(a.out, "words.jsonl"), "w", encoding="utf-8")
    for hk in host_keys:
        if made >= a.lines:
            break
        hw = hosts[hk]
        h0 = hw[0]
        # A donor from the same scan where the page has English lines; else any training page.
        dslug = h0["page"] if donors.get(h0["page"]) else rng.choice(donor_pages)
        want = 1 if rng.random() < 0.5 else rng.randint(2, 4)
        cands = [d for d in donors[dslug] if len(d["words"]) == want] or donors[dslug]
        d = rng.choice(cands)
        # The host line, cut as `ocr` cuts it.
        x0, y0, x1, y1 = h0["line_box"]
        pad = max(4, int(LINE_PAD * (y1 - y0)))
        HX0, HY0 = max(0, x0 - 4), max(0, y0 - pad)
        hg = page(h0["page"])[HY0:y1 + pad, HX0:x1 + 4].astype(np.float32)
        hink = F.binarize(hg.astype(np.uint8))
        htop, hbot = F.body(hink)
        # The donor span, cut with the same padding from its own line.
        ly0, ly1 = d["line_box"][1], d["line_box"][3]
        if ly1 - ly0 > 1.6 * d["pitch"]:
            continue  # LoC's line box spans more than one printed line
        dx0 = max(0, d["words"][0]["box"][0] - 2)
        dx1 = d["words"][-1]["box"][2] + 2
        dpad = max(4, int(LINE_PAD * (ly1 - ly0)))
        DY0 = max(0, ly0 - dpad)
        dg = page(dslug)[DY0:ly1 + dpad, dx0:dx1]
        if dg.size == 0:
            continue
        # Scale: the syllabary was cut at the height of the roman capitals, so the donor's capital
        # height (the tall letters' tops to its baseline) is set to the host's letter height.
        dink = F.binarize(dg)
        dtop, dbot = F.body(dink)
        boxes, _ = F.components(dink)
        sit = boxes[np.abs(boxes[:, 2] - dbot) <= 0.25 * max(4, dbot - dtop)] if len(boxes) else boxes
        if len(sit) < 3:
            continue
        cap = float(np.percentile(dbot - sit[:, 0], 90))
        scale = (hbot - htop) / max(1.0, cap)
        # Some Latin is set larger than that (the 1828 vocabulary lesson's transliterations have an
        # x-height near the syllabary's height), so the scale is jittered upward as well as down.
        scale *= math.exp(rng.uniform(math.log(0.85), math.log(1.5)))
        if not 0.4 < scale < 2.5:
            continue
        # Guards against a badly found body on either side: the donor's x-height lands at 40-125% of
        # the host's letter height, the host's letters fill 40-100% of LoC's line box, and the donor has ink.
        if not 0.4 <= (dbot - dtop) * scale / max(1, hbot - htop) <= 1.25:
            continue
        if not 0.4 <= (hbot - htop) / max(1, y1 - y0) <= 1.0 or dink.mean() < 0.03:
            continue
        dimg = Image.fromarray(dg).resize((max(1, int(dg.shape[1] * scale)), max(1, int(dg.shape[0] * scale))),
                                          Image.BILINEAR)
        dg = np.asarray(dimg).astype(np.float32)
        dbot = int(round(dbot * scale))
        di, db = tones(dg)
        hi, hb = tones(hg)
        dg = np.clip(hi + (dg - di) * (hb - hi) / max(1.0, db - di), 0, 255)
        # Replace `want` host words (or as many as the line has after the start).
        n = len(d["words"])
        j = rng.randint(0, max(0, len(hw) - n))
        rep = hw[j:j + n]
        cut0 = rep[0]["box"][0] - HX0 - 1
        cut1 = rep[-1]["box"][2] - HX0 + 1
        left, right = hg[:, :max(0, cut0)], hg[:, max(0, cut1):]
        # Vertical: the donor's baseline (its body's bottom) on the host's baseline.
        H = hg.shape[0]
        mid = np.full((H, dg.shape[1]), hb, dtype=np.float32)
        off = hbot - dbot
        for yy in range(dg.shape[0]):
            ty = yy + off
            if 0 <= ty < H:
                mid[ty] = dg[yy]
        img = np.concatenate([left, mid, right], axis=1)
        name = f"synth{made:05d}"
        png = os.path.join(a.out, "lines", name + ".png")
        Image.fromarray(img.astype(np.uint8)).save(png)
        shift = mid.shape[1] - (max(0, cut1) - max(0, cut0))
        recs = []
        for i, w in enumerate(hw[:j]):
            recs.append((w, [w["box"][0] - HX0, w["box"][1] - HY0, w["box"][2] - HX0, w["box"][3] - HY0], None))
        for w in d["words"]:
            bx0 = max(0, cut0) + int((w["box"][0] - max(0, dx0)) * scale)
            bx1 = max(0, cut0) + int((w["box"][2] - max(0, dx0)) * scale)
            # Its own height, moved as the paste was (cut at DY0, scaled, set off by `off`), so its
            # capitals and descenders are inside the box the glyph CNN's crop is cut from.
            by0 = min(H - 1, max(0, int((w["box"][1] - DY0) * scale) + off))
            by1 = min(H, max(by0 + 1, int((w["box"][3] - DY0) * scale) + off))
            recs.append(({"word": "synth-" + w["id"], "loc": w["text"]}, [bx0, by0, bx1, by1], "l"))
        for w in hw[j + n:]:
            recs.append((w, [w["box"][0] - HX0 + shift, w["box"][1] - HY0, w["box"][2] - HX0 + shift,
                             w["box"][3] - HY0], None))
        line_box = [4, min(pad, H - 1), img.shape[1] - 4, max(pad + 1, H - pad)]
        for i, (w, box, lab) in enumerate(recs):
            out.write(json.dumps({
                **{k: h0[k] for k in ("page", "page_size", "block", "block_box", "line_index", "prev_box", "next_box",
                                       "prev_loc", "next_loc", "block_lines", "block_syllabary")},
                "split": "train", "line": name, "line_box": line_box, "png": png, "word": w["word"],
                "word_index": i, "words_in_line": len(recs), "box": box, "loc": "" if lab else w["loc"],
                "kind": "synth-latin" if lab else w["kind"], "label": lab or "host",
                "donor_page": dslug, "line_loc": "",
            }, ensure_ascii=False) + "\n")
        made += 1
    print(json.dumps({"lines": made, "donor_pages": len(donor_pages), "donor_runs": sum(map(len, donors.values()))}))


# Inputs the model never sees: LoC's own reading (the labels are built from it, and pages outside
# LoC have none) and counts that only describe the block's LoC text.
NOT_MODEL = ("loc_", "left_loc_", "right_loc_", "prev_loc_", "next_loc_", "line_loc_", "block_syllabary")


def model_columns(names):
    return [i for i, n in enumerate(names) if not n.startswith(NOT_MODEL)]


def bootstrap_label(m, row, col):
    """High-precision labels from LoC's reading and the two models' readings (None: no label).

    s: LoC read a New Testament form of two or more letters; or, in a block where nine in ten LoC
       words are syllabary, a syllabary reading of three or more letters that `eng` does not read
       as an English word of four or more letters.
    o: LoC read no letters at all: numerals (verse numbers, dates), punctuation, rules.
    l: `eng` read an English word of four or more letters with two lowercase letters at confidence
       85 or more, where LoC's reading is not a New Testament form.
    """
    if m.get("synth") == "l":
        return "l"
    kind = m["kind"]
    if kind in ("digits", "punct", "other"):
        return "o"
    core = m["loc"].strip(PUNCT)
    attested = kind == "syl-attested"
    eng_word = row[col["eng_english_long"]] > 0
    if attested and len(core) >= 2:
        return "s"
    if eng_word and row[col["eng_conf"]] >= 85 and row[col["eng_n_lower"]] >= 2:
        return "l"
    if kind == "syl-other" and len(core) >= 3 and row[col["block_syllabary"]] >= 0.9 and not eng_word:
        return "s"
    return None


THRESHOLDS = (0.5, 0.7, 0.8, 0.9, 0.95, 0.98)


def dev_page(page):
    """One training page in five is held out for development, by a stable hash of its name."""
    import zlib
    return zlib.crc32(page.encode()) % 5 == 0


CNN_NAMES = ["cnn_l", "cnn_o", "cnn_s", "left_cnn_l", "right_cnn_l", "left_cnn_s", "right_cnn_s",
             "line_cnn_l", "line_cnn_s"]


def cnn_columns(P, meta):
    """The glyph CNN's probabilities for each word, for the words either side and the line's mean."""
    import numpy as np
    out = np.zeros((len(meta), len(CNN_NAMES)), dtype=np.float32)
    out[:, :3] = P
    lines = collections.OrderedDict()
    for i, m in enumerate(meta):
        lines.setdefault(m["line"], []).append(i)
    for idx in lines.values():
        pl, ps = P[idx, 0], P[idx, 2]
        for k, i in enumerate(idx):
            out[i, 3] = pl[k - 1] if k > 0 else -1
            out[i, 4] = pl[k + 1] if k + 1 < len(idx) else -1
            out[i, 5] = ps[k - 1] if k > 0 else -1
            out[i, 6] = ps[k + 1] if k + 1 < len(idx) else -1
            out[i, 7] = pl.mean()
            out[i, 8] = ps.mean()
    return out


def load_feats(dirs, cnn=True, cnn_name="cnn.npy"):
    """Feature rows, metadata and names of one or more `features` directories; with the glyph CNN's
    columns appended when every directory has its `cnn.npy` (from `glyph-cnn`)."""
    import numpy as np
    Xs, M = [], []
    names = None
    with_cnn = cnn and all(os.path.exists(os.path.join(d, cnn_name)) for d in dirs)
    for d in dirs:
        X = np.load(os.path.join(d, "X.npy"))
        meta = [json.loads(l) for l in open(os.path.join(d, "meta.jsonl"), encoding="utf-8")]
        if with_cnn:
            X = np.concatenate([X, cnn_columns(np.load(os.path.join(d, cnn_name)), meta)], axis=1)
        Xs.append(X)
        M += meta
        names = json.load(open(os.path.join(d, "names.json"))) + (CNN_NAMES if with_cnn else [])
    return np.concatenate(Xs), M, names


def confusion(y_true, y_pred):
    c = collections.Counter(zip(y_true, y_pred))
    return {f"{t}->{p}": n for (t, p), n in sorted(c.items())}


def cmd_train(a):
    """Fit on training pages, report on the held-out fifth, then refit on all and save."""
    import pickle
    import numpy as np
    from sklearn.ensemble import HistGradientBoostingClassifier
    X, M, names = load_feats(a.feats, cnn_name=a.cnn_name)
    col = {n: i for i, n in enumerate(names)}

    def fit(idx):
        clf = HistGradientBoostingClassifier(max_iter=a.trees, max_depth=a.depth, learning_rate=0.08,
                                             l2_regularization=1.0, early_stopping=False, random_state=a.seed)
        return clf.fit(X[idx][:, cols], y[idx], sample_weight=weights(idx))
    hand = {(r["page"], r["word"]): r["label"] for r in map(json.loads, open(a.hand, encoding="utf-8"))} if a.hand else {}
    lab, src = [], []
    for m, row in zip(M, X):
        h = hand.get((m["page"], m["word"]))
        synth = "#synth" in m["line"]
        # A hand-labelled word copied into a synthetic line keeps its label but is counted with
        # the synthetic lines, so the real and synthetic tables don't mix.
        src.append("synth" if synth else "hand" if h else "rule")
        if m["split"] == "test" and not h:
            lab.append(None)
            continue
        lab.append(h or bootstrap_label(m, row, col))
    lab, src = np.array(lab, dtype=object), np.array(src)
    cols = model_columns(names)
    test = np.array([m["split"] == "test" for m in M])
    extra = set(a.extra_dev_page or [])
    test_pages = {m["page"] for m in M if m["split"] == "test"}
    is_dev = lambda pg: dev_page(pg) or pg in extra
    dev = np.array([is_dev(m["page"]) for m in M])
    # A synthetic line whose donor page sits on the other side of the split (or in the test set)
    # would carry glyphs across it: such lines are dropped.
    cross = {m["line"] for m in M if m.get("donor_page") and
             (m["donor_page"] in test_pages or is_dev(m["donor_page"]) != is_dev(m["page"]))}
    use = np.array([l is not None for l in lab]) & ~test & np.array([m["line"] not in cross for m in M])
    print("synthetic lines dropped for a donor across the split:", len(cross))
    y = np.array([0 if l == "s" else 1 for l in lab])
    print("labels", {f"{s_}/{l}": n for (s_, l), n in sorted(collections.Counter(zip(src[use], lab[use])).items())})

    def weights(idx):
        # Each class carries equal total weight, so rare Latin is not drowned by the syllabary.
        w = np.ones(len(idx))
        for c in ("s", "o", "l"):
            k = lab[idx] == c
            if k.any():
                w[k] = len(idx) / (3 * k.sum())
        return w

    def table(idx, p, title):
        rows = {}
        for t in THRESHOLDS:
            pred = np.where(p >= t, "eng", "chr")
            per = {}
            for s_ in ("rule", "synth", "hand", "all"):
                k = np.ones(len(idx), bool) if s_ == "all" else src[idx] == s_
                if k.any():
                    per[s_] = confusion(lab[idx][k], pred[k])
            rows[str(t)] = per
            c = per["all"]
            s_eng, s_all = c.get("s->eng", 0), c.get("s->eng", 0) + c.get("s->chr", 0)
            l_eng, l_all = c.get("l->eng", 0), c.get("l->eng", 0) + c.get("l->chr", 0)
            o_eng, o_all = c.get("o->eng", 0), c.get("o->eng", 0) + c.get("o->chr", 0)
            print(f"{title} t={t}: syllabary to eng {s_eng}/{s_all}; Latin to eng {l_eng}/{l_all}; "
                  f"other to eng {o_eng}/{o_all}")
        return rows

    tr = np.nonzero(use & ~dev)[0]
    dv = np.nonzero(use & dev)[0]
    model = fit(tr)
    p = model.predict_proba(X[dv][:, cols])[:, 1]
    report = {"labels": {f"{s_}/{l}": n for (s_, l), n in collections.Counter(zip(src[use], lab[use])).items()},
              "train_words": int(len(tr)), "dev_words": int(len(dv))}
    report["dev"] = table(dv, p, "dev pages")
    # Final model on every labelled training word; the hand labels on the frozen test pages
    # (never trained on) are scored with it.
    allidx = np.nonzero(use)[0]
    final = fit(allidx)
    te = np.nonzero(test & np.array([l is not None for l in lab]))[0]
    if len(te):
        report["test_hand"] = table(te, final.predict_proba(X[te][:, cols])[:, 1], "test pages, hand labels")
    os.makedirs(a.out, exist_ok=True)
    # Importance: the drop in dev log-loss when a feature is shuffled (on the dev-fold model).
    from sklearn.inspection import permutation_importance
    pi = permutation_importance(model, X[dv][:, cols], y[dv], scoring="neg_log_loss", n_repeats=3,
                                random_state=a.seed, sample_weight=weights(dv))
    report["importance"] = sorted(((names[c], round(float(v), 4)) for c, v in zip(cols, pi.importances_mean)
                                   if v > 0), key=lambda x: -x[1])[:30]
    pickle.dump({"model": final, "features": [names[c] for c in cols], "threshold": a.threshold,
                 "positive": "not syllabary (send to eng)"}, open(os.path.join(a.out, "model.pkl"), "wb"))
    json.dump(report, open(os.path.join(a.out, "report.json"), "w"), indent=1)
    dev_out = open(os.path.join(a.out, "dev-preds.jsonl"), "w", encoding="utf-8")
    for i, pi in zip(dv, p):
        dev_out.write(json.dumps({**M[i], "label": lab[i], "source": src[i], "p": round(float(pi), 4)},
                                 ensure_ascii=False) + "\n")
    print(json.dumps(report["importance"][:15]))


def cmd_route(a):
    """Per-word routing on the frozen test set's lines, for its CER score.

    A line where the classifier sends no word to `eng` keeps the syllabary model's reading of the
    whole line (`--base-pred`). Otherwise the line is cut at the gaps between runs of words with the
    same route, each run is read at PSM 13 by its model, and the readings are joined in order.
    """
    import concurrent.futures as cf
    import tempfile
    import numpy as np
    from PIL import Image
    import pickle
    model = pickle.load(open(a.model, "rb"))
    thr = a.threshold if a.threshold is not None else model["threshold"]
    X, M, names = load_feats(a.feats, cnn_name=a.cnn_name)
    col = {n: i for i, n in enumerate(names)}
    cols = [col[n] for n in model["features"]]
    test = np.array([m["split"] == "test" for m in M])
    p = np.zeros(len(M))
    p[test] = model["model"].predict_proba(X[test][:, cols])[:, 1]
    prob = {(m["page"], m["word"]): float(pi) for m, pi, t in zip(M, p, test) if t}
    words = collections.defaultdict(list)
    for l in open(a.words, encoding="utf-8"):
        r = json.loads(l)
        if r["split"] == "test":
            words[r["page"] + "#" + r["line"]].append(r)
    base = {r["crop"]: r["text"] for r in map(json.loads, open(a.base_pred, encoding="utf-8"))}
    tmp = tempfile.mkdtemp(prefix="sd-route-")
    stats = collections.Counter()

    def run(line):
        key = line["page"] + "#" + line["line_id"]
        ws = sorted(words.get(key, []), key=lambda w: w["box"][0])
        routes = ["eng" if prob.get((w["page"], w["word"]), 0.0) >= thr else "chr" for w in ws]
        if "eng" not in routes:
            return line["crop"], base[line["crop"]], []
        im = Image.open(os.path.join(a.crops, line["crop"]))
        X0 = line["pixel_box"][0]
        groups = []
        for w, r in zip(ws, routes):
            if groups and groups[-1][0] == r:
                groups[-1][1].append(w)
            else:
                groups.append((r, [w]))
        texts = []
        for gi, (r, gw) in enumerate(groups):
            left = 0 if gi == 0 else (groups[gi - 1][1][-1]["box"][2] + gw[0]["box"][0]) // 2 - X0
            right = im.width if gi == len(groups) - 1 else (gw[-1]["box"][2] + groups[gi + 1][1][0]["box"][0]) // 2 - X0
            left, right = max(0, left), min(im.width, right)
            if right - left < 4:
                continue
            png = os.path.join(tmp, f"{line['crop']}.{gi}.png")
            im.crop((left, 0, right, im.height)).save(png)
            lang = a.chr_lang if r == "chr" else a.latin_lang
            texts.append(" ".join(w["text"] for w in tesseract_words(png, a.tessdata, lang, 13)))
            os.remove(png)
        return line["crop"], " ".join(t for t in texts if t), routes

    lines = [json.loads(l) for l in open(a.testset_lines, encoding="utf-8")]
    with open(a.out, "w", encoding="utf-8") as f, cf.ThreadPoolExecutor(a.jobs) as ex:
        for crop, text, routes in ex.map(run, lines):
            stats["lines"] += 1
            stats["routed_lines"] += bool(routes)
            stats["eng_words"] += routes.count("eng")
            f.write(json.dumps({"crop": crop, "text": text, "routes": routes}, ensure_ascii=False) + "\n")
    print(json.dumps(stats))


def cmd_score(a):
    """Syllabary-letter CER (the test set's own measure) on the silver tier, on every line, and on
    the hand-checked lines (their `checked` text as the reference).

    With --compare A:B, a paired page bootstrap of CER(B) - CER(A): the test set's pages are drawn
    with replacement (--bootstrap times), both readings are scored on the same draw, and the 95%
    interval is the 2.5th to 97.5th percentile of the differences. Pages, not lines, are drawn,
    because lines of one page share type, paper and scan."""
    import importlib.util
    import numpy as np
    spec = importlib.util.spec_from_file_location("cherokee_testset", os.path.join(a.testset_dir, "cherokee_testset.py"))
    T = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(T)
    lines = [json.loads(l) for l in open(os.path.join(a.testset_dir, "crops", "lines.jsonl"), encoding="utf-8")]
    page_of = {r["crop"]: r["page"] for r in lines}
    checked = json.load(open(a.checked, encoding="utf-8")) if a.checked else []
    sets = {"silver": [(r["crop"], r["reference"]) for r in lines if r["tier"] == "silver"],
            "all": [(r["crop"], r["reference"]) for r in lines],
            "checked": [(r["crop"], r["checked"]) for r in checked]}
    per = {}  # name -> tier -> page -> [errors, letters]
    out = {}
    for spec_ in a.pred:
        name, _, path = spec_.partition("=")
        if name == "loc" and not path:  # LoC's own text of each line
            preds = {r["crop"]: r["loc_text"] for r in lines}
        else:
            preds = {p["crop"]: p["text"] for p in map(json.loads, open(path, encoding="utf-8"))}
        out[name], per[name] = {}, {}
        for sname, rows in sets.items():
            pages = collections.defaultdict(lambda: [0, 0])
            for crop, gold in rows:
                g = "".join(T.SYLLABARY.findall(gold))
                pages[page_of[crop]][0] += T.letter_edit_distance(g, "".join(T.SYLLABARY.findall(preds.get(crop) or "")))
                pages[page_of[crop]][1] += len(g)
            err, ref = sum(v[0] for v in pages.values()), sum(v[1] for v in pages.values())
            out[name][sname] = {"cer": round(err / max(1, ref), 4), "lines": len(rows), "letters": ref,
                                "pages": len(pages)}
            per[name][sname] = pages
    if a.compare:
        rng = np.random.default_rng(a.seed)
        out["compare"] = {}
        for pair in a.compare:
            base, new_ = pair.split(":")
            res = {}
            for sname in sets:
                pages = sorted(per[base][sname])
                if not pages:  # no lines in this tier (no --checked): nothing to draw
                    continue
                A = np.array([per[base][sname][p] for p in pages], dtype=float)
                B = np.array([per[new_][sname][p] for p in pages], dtype=float)
                draws = rng.integers(0, len(pages), size=(a.bootstrap, len(pages)))
                da = A[draws].sum(axis=1)
                db = B[draws].sum(axis=1)
                diff = db[:, 0] / db[:, 1] - da[:, 0] / da[:, 1]
                point = B[:, 0].sum() / B[:, 1].sum() - A[:, 0].sum() / A[:, 1].sum()
                lo, hi = np.percentile(diff, [2.5, 97.5])
                res[sname] = {"diff": round(float(point), 4), "ci95": [round(float(lo), 4), round(float(hi), 4)],
                              "pages": len(pages), "share_of_draws_better": round(float((diff < 0).mean()), 4)}
            out["compare"][f"{new_} - {base}"] = res
    print(json.dumps(out, indent=1, ensure_ascii=False))
    if a.out:
        json.dump(out, open(a.out, "w"), indent=1, sort_keys=True, ensure_ascii=False)


def cmd_glyph_donors(a):
    """Word crops from the English lines of training pages: Latin words (not all capitals) and
    numerals or punctuation, the glyph CNN's examples of those classes."""
    import random
    import numpy as np
    from PIL import Image
    import sd_cnn
    Image.MAX_IMAGE_PIXELS = None
    test = sd_alto.excluded_pages(a.exclude_manifest)
    rng = random.Random(a.seed)
    crops, meta = [], []
    for slug, path in sd_alto.alto_pages(a.alto_dir):
        img = sd_alto.image_path(a.images, slug)
        if slug in test or not img:
            continue
        page = None
        lat, oth = [], []
        with Image.open(img) as im:
            width = im.size[0]
        for b in sd_alto.read_page(path, width):
            for ln in b["lines"]:
                if any(sd_alto.syllabary_word(w["text"]) for w in ln["words"]):
                    continue
                for w in ln["words"]:
                    core = w["text"].strip(PUNCT)
                    letters = re.sub(r"[^A-Za-z]", "", core)
                    if len(letters) >= 2 and re.fullmatch(r"[A-Za-z][A-Za-z'’-]*[A-Za-z]", core) \
                            and not letters.isupper():
                        lat.append(w)
                    elif not letters and (DIGITS.search(core) or w["text"].strip()):
                        oth.append(w)
        picked = [("l", w) for w in rng.sample(lat, min(a.latin_per_page, len(lat)))]
        picked += [("o", w) for w in rng.sample(oth, min(a.other_per_page, len(oth)))]
        if not picked:
            continue
        page = np.asarray(Image.open(img).convert("L"))
        for lab, w in picked:
            crops.append(sd_cnn.word_crop(page, w["box"]))
            meta.append({"page": slug, "word": w["id"], "label": lab, "text": w["text"]})
    os.makedirs(a.out, exist_ok=True)
    np.save(os.path.join(a.out, "crops.npy"), np.asarray(crops, dtype=np.float16))
    with open(os.path.join(a.out, "meta.jsonl"), "w", encoding="utf-8") as f:
        for m in meta:
            f.write(json.dumps(m, ensure_ascii=False, sort_keys=True) + "\n")
    print(json.dumps(collections.Counter(m["label"] for m in meta)))


def cmd_glyph_crops(a):
    """A word crop for every row of a `features` directory, in its order (crops.npy)."""
    import numpy as np
    from PIL import Image
    import sd_cnn
    Image.MAX_IMAGE_PIXELS = None
    recs = {}
    for l in open(a.words, encoding="utf-8"):
        r = json.loads(l)
        recs[(r["page"] + "#" + r["line"], r["word"])] = r
    meta = [json.loads(l) for l in open(os.path.join(a.feats, "meta.jsonl"), encoding="utf-8")]
    out = np.zeros((len(meta), sd_cnn.CROP_H, sd_cnn.CROP_W), dtype=np.float16)
    cur, img = None, None
    for i, m in enumerate(meta):
        r = recs[(m["line"], m["word"])]
        src = r.get("png") or r["page"]
        if src != cur:
            cur = src
            img = np.asarray(Image.open(r["png"] if r.get("png") else sd_alto.image_path(a.images, r["page"])).convert("L"))
        out[i] = sd_cnn.word_crop(img, r["box"])
    np.save(os.path.join(a.feats, "crops.npy"), out)
    print(len(meta))


def cmd_glyph_cnn(a):
    """Two glyph CNNs, one per half of the pages; every row of each --score directory gets the
    probabilities of the CNN that did not train on its page (test pages: the mean of both)."""
    import numpy as np
    import sd_cnn
    import random
    rng = random.Random(a.seed)
    X, M, names = load_feats([a.feats], cnn=False)
    col = {n: i for i, n in enumerate(names)}
    hand = {(r["page"], r["word"]): r["label"] for r in map(json.loads, open(a.hand, encoding="utf-8"))}
    crops = np.load(os.path.join(a.feats, "crops.npy"))
    # Syllabary-line words with a label, at most --syllabary-per-page syllabary ones per page.
    per_page = collections.defaultdict(list)
    for i, (m, row) in enumerate(zip(M, X)):
        if m["split"] == "test":
            continue
        lab = hand.get((m["page"], m["word"])) or bootstrap_label(m, row, col)
        if lab:
            per_page[m["page"]].append((i, lab))
    tX, ty, tpage = [], [], []
    # Pages left out of both CNNs (the development fifth, the --exclude-page holdouts) are scored
    # like test pages, so a router evaluated on them never sees features from a CNN that learned them.
    excluded = set(a.exclude_page or [])
    if a.exclude_dev:
        excluded |= {m["page"] for m in M if dev_page(m["page"])}
    for pg, items in per_page.items():
        if pg in excluded:
            continue
        syl = [i for i, l in items if l == "s"]
        keep = set(rng.sample(syl, min(a.syllabary_per_page, len(syl))))
        for i, l in items:
            if l != "s" or i in keep:
                tX.append(crops[i])
                ty.append(sd_cnn.CLASSES.index(l))
                tpage.append(pg)
    dcrops = np.load(os.path.join(a.donors, "crops.npy"))
    for m, c in zip(map(json.loads, open(os.path.join(a.donors, "meta.jsonl"), encoding="utf-8")), dcrops):
        if m["page"] in excluded:
            continue
        tX.append(c)
        ty.append(sd_cnn.CLASSES.index(m["label"]))
        tpage.append(m["page"])
    tX = np.asarray(tX, dtype=np.float32)
    ty = np.asarray(ty)
    tf = np.array([sd_cnn.fold(p) for p in tpage])
    print("training crops", {f"fold{f}/{c}": int(((tf == f) & (ty == k)).sum())
                             for f in (0, 1) for k, c in enumerate(sd_cnn.CLASSES)})
    os.makedirs(a.out, exist_ok=True)
    models = []
    import torch
    for f in (0, 1):
        saved = os.path.join(a.out, f"{a.name}-fold{f}.pt")
        if a.reuse and os.path.exists(saved):  # score again with the CNNs trained before
            m = sd_cnn.net()
            m.load_state_dict(torch.load(saved))
            m.eval()
            models.append(m)
            continue
        m = sd_cnn.train(tX[tf != f], ty[tf != f], epochs=a.epochs, seed=a.seed + f, threads=a.threads,
                         log=lambda t, f=f: print(f"fold {f}: {t}", flush=True))
        torch.save(m.state_dict(), saved)
        models.append(m)
        held = tf == f
        p = sd_cnn.predict(m, tX[held]).argmax(1)
        print(f"fold {f} held-out crops: " + json.dumps({f"{sd_cnn.CLASSES[t]}->{sd_cnn.CLASSES[q]}": int(n) for (t, q), n in
                                                         sorted(collections.Counter(zip(ty[held], p)).items())}), flush=True)
    for d in a.score:
        meta = [json.loads(l) for l in open(os.path.join(d, "meta.jsonl"), encoding="utf-8")]
        c = np.load(os.path.join(d, "crops.npy")).astype(np.float32)
        P = np.zeros((len(meta), 3), dtype=np.float32)
        # The crop's own page decides: a pasted synthetic word was cut from its donor page.
        f_of = np.array([sd_cnn.fold(m.get("donor_page") if m.get("synth") == "l" and m.get("donor_page")
                                     else m["page"]) for m in meta])
        src_page = [m.get("donor_page") if m.get("synth") == "l" and m.get("donor_page") else m["page"] for m in meta]
        test = np.array([m["split"] == "test" or pg in excluded or m["page"] in excluded
                         for m, pg in zip(meta, src_page)])
        for f in (0, 1):
            # models[f] was trained with fold f left out, so it scores fold f's pages.
            k = (f_of == f) & ~test
            P[k] = sd_cnn.predict(models[f], c[k])
        P[test] = (sd_cnn.predict(models[0], c[test]) + sd_cnn.predict(models[1], c[test])) / 2
        np.save(os.path.join(d, a.name + ".npy"), P)
        print(d, "scored", len(meta))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("survey", help="count LoC's token kinds on syllabary lines")
    s.add_argument("--alto-dir", required=True)
    s.add_argument("--vocab", required=True)
    s.add_argument("--out", required=True)
    s = sub.add_parser("words", help="one record per word of every syllabary line")
    s.add_argument("--alto-dir", required=True)
    s.add_argument("--images", required=True, action="append")
    s.add_argument("--vocab", required=True)
    s.add_argument("--exclude-manifest", required=True)
    s.add_argument("--out", required=True)
    s = sub.add_parser("ocr", help="one Tesseract model over every syllabary line")
    g = s.add_mutually_exclusive_group(required=True)
    g.add_argument("--words", help="lines cut from the page scans, as `words` lists them")
    g.add_argument("--testset-lines", help="the frozen test set's line crops")
    s.add_argument("--crops", help="the test set's crop directory")
    s.add_argument("--images", action="append", default=[])
    s.add_argument("--tessdata", required=True)
    s.add_argument("--lang", required=True)
    s.add_argument("--psm", type=int, default=13)
    s.add_argument("--jobs", type=int, default=2)
    s.add_argument("--out", required=True)
    s = sub.add_parser("features", help="the classifier's features for every word")
    s.add_argument("--words", required=True)
    s.add_argument("--images", required=True, action="append")
    s.add_argument("--ocr", required=True, action="append", help="eng=PATH and chr=PATH, from `ocr`")
    s.add_argument("--vocab", required=True)
    s.add_argument("--english", required=True)
    s.add_argument("--out", required=True)
    s = sub.add_parser("synth", help="syllabary lines with real Latin words pasted in")
    s.add_argument("--words", required=True)
    s.add_argument("--alto-dir", required=True)
    s.add_argument("--images", required=True, action="append")
    s.add_argument("--english", required=True)
    s.add_argument("--exclude-manifest", required=True)
    s.add_argument("--lines", type=int, default=3000)
    s.add_argument("--seed", type=int, default=1666)
    s.add_argument("--out", required=True)
    s = sub.add_parser("train", help="fit and report the word classifier")
    s.add_argument("--feats", required=True, action="append", help="directories written by `features`")
    s.add_argument("--hand", help="hand labels (page, word, label)")
    s.add_argument("--trees", type=int, default=300)
    s.add_argument("--depth", type=int, default=3)
    s.add_argument("--seed", type=int, default=1666)
    s.add_argument("--threshold", type=float, default=0.9)
    s.add_argument("--cnn-name", default="cnn.npy", help="the glyph CNN score file to read from each --feats")
    s.add_argument("--extra-dev-page", action="append",
                   help="hold this page out of the development fit as well (e.g. the pages with real Latin)")
    s.add_argument("--out", required=True)
    s = sub.add_parser("route", help="route each word of the test set's lines and read them")
    s.add_argument("--model", required=True)
    s.add_argument("--threshold", type=float)
    s.add_argument("--feats", required=True, action="append")
    s.add_argument("--words", required=True)
    s.add_argument("--testset-lines", required=True)
    s.add_argument("--crops", required=True)
    s.add_argument("--tessdata", required=True)
    s.add_argument("--chr-lang", default="chr_phoenix")
    s.add_argument("--cnn-name", default="cnn.npy", help="the glyph CNN score file to read from each --feats")
    s.add_argument("--latin-lang", default="eng",
                   help="the model for words routed away from the syllabary (the syllabary model again: the cut-only ablation)")
    s.add_argument("--base-pred", required=True, help="the syllabary model's whole-line readings")
    s.add_argument("--jobs", type=int, default=2)
    s.add_argument("--out", required=True)
    s = sub.add_parser("score", help="test-set CER of line readings")
    s.add_argument("--testset-dir", required=True)
    s.add_argument("--checked", help="the hand-checked lines (crop, checked)")
    s.add_argument("--pred", required=True, action="append", help="NAME=PATH, one JSON line per crop; `loc` alone is LoC's text")
    s.add_argument("--compare", action="append", help="A:B, two --pred names: a paired page bootstrap of CER(B) - CER(A)")
    s.add_argument("--bootstrap", type=int, default=10000)
    s.add_argument("--seed", type=int, default=1666)
    s.add_argument("--out")
    s = sub.add_parser("glyph-donors", help="English-line word crops for the glyph CNN")
    s.add_argument("--alto-dir", required=True)
    s.add_argument("--images", required=True, action="append")
    s.add_argument("--exclude-manifest", required=True)
    s.add_argument("--latin-per-page", type=int, default=300)
    s.add_argument("--other-per-page", type=int, default=40)
    s.add_argument("--seed", type=int, default=1666)
    s.add_argument("--out", required=True)
    s = sub.add_parser("glyph-crops", help="a word crop for every row of a features directory")
    s.add_argument("--feats", required=True)
    s.add_argument("--words", required=True)
    s.add_argument("--images", action="append", default=[])
    s = sub.add_parser("glyph-cnn", help="train the cross-fitted glyph CNNs and score features directories")
    s.add_argument("--feats", required=True, help="the real words' features directory, for training labels")
    s.add_argument("--donors", required=True)
    s.add_argument("--hand", required=True)
    s.add_argument("--score", required=True, action="append")
    s.add_argument("--syllabary-per-page", type=int, default=300)
    s.add_argument("--reuse", action="store_true", help="load the fold CNNs already in --out instead of training")
    s.add_argument("--exclude-dev", action="store_true", help="leave the development pages out of both CNNs")
    s.add_argument("--exclude-page", action="append", help="leave this page out of both CNNs")
    s.add_argument("--name", default="cnn", help="names the saved CNNs and the score file in each --score directory")
    s.add_argument("--epochs", type=int, default=8)
    s.add_argument("--threads", type=int, default=3)
    s.add_argument("--seed", type=int, default=1666)
    s.add_argument("--out", required=True)
    s = sub.add_parser("sheet", help="contact sheets of words in their lines")
    s.add_argument("--words", required=True)
    s.add_argument("--images", required=True, action="append")
    s.add_argument("--out-dir", required=True)
    s.add_argument("--rows", type=int, default=20)
    s.add_argument("--width", type=int, default=900)
    s.add_argument("--row-height", type=int, default=60)
    a = ap.parse_args()
    return {"survey": cmd_survey, "words": cmd_words, "ocr": cmd_ocr, "features": cmd_features, "synth": cmd_synth, "train": cmd_train, "route": cmd_route, "score": cmd_score, "glyph-donors": cmd_glyph_donors,
            "glyph-crops": cmd_glyph_crops, "glyph-cnn": cmd_glyph_cnn, "sheet": cmd_sheet}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
