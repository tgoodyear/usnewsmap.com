"""Pick Cherokee Phoenix lines for check.usnewsmap.com (#1620, #1621) from LoC's ALTO and our production re-read.

Reads, for each page, LoC's ALTO (alto-2 or 3, the batches unpacked) and the ALTO the line jobs published
(`chr_phoenix` readings merged into LoC's lines, `curated/ocr-local/alto/`). The re-read numbers its lines afresh but
keeps LoC's line boxes in its own pixels, so each re-read line is paired with the LoC line whose top left corner is
nearest after scaling. Where a line has the same number of syllabary words in both readings the words pair one to one,
and each word that differs is a candidate task.

Writes OUT_DIR/<kind>/lines.jsonl for `build_tasks.py` (one printed line each, reading a = LoC's, reading b = the
other, with a_is and b_is on the lines), OUT_DIR/<kind>/images/<line id>.jpg and OUT_DIR/<kind>/manifest.jsonl (what
`scripts/chr_disputes_score.py` reads: every word of both readings and which differ). Pillow cuts the images; with
--no-images nothing needs it.

  disputes  Lines of at least three words where the two readings differ in exactly one word, on the test set's pages
            only (the held-out ones), one task per line. Lines that differ only in a word-initial G-shaped letter are
            left to `nah`. News lines (not aligned to the New Testament) come before scripture.
  rare      Words with a letter the glyph bank has few or no samples of, on pages outside the test set.
  nah       Words whose first letter OCR reads as Ꮎ or one of the G-shaped letters, where the New Testament spells
            the word with Ꮎ and not Ꮐ, on pages outside the test set: a = the Ꮎ spelling, b = the Ꮐ spelling.

  python3 phoenix_lines.py disputes --prod-alto DIR --loc-alto DIR --scans DIR --pages phoenix-pages.jsonl \
    --test-lines testset/crops/lines.jsonl --out-dir OUT
"""
import argparse
import bisect
import collections
import datetime
import difflib
import glob
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

SYL = re.compile("[Ꭰ-Ᏽ]")
NA, NAH = "Ꮎ", "Ꮐ"
# What OCR reads a word-initial Ꮎ as: Ꮹ (wa), Ꮳ (tsa), Ᏻ (yu), Ꮐ (nah), as in usnm_core::cherokee_type.
G_SHAPED = "ᏩᏣᏳ" + NAH
# Ꮊ me, Ꮌ mo, Ꮍ mu, Ꮐ nah, Ꮛ quv (no samples in the glyph bank) and Ꮯ tli, Ꮚ quu, Ꮬ dla, Ꮾ wv (almost none).
RARE = "ᎺᎼᎽᏀᏋᏟᏊᏜᏮ"
MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]


def letters(text: str) -> str:
    """The syllabary letters of `text`: no punctuation, digits or Latin."""
    return "".join(SYL.findall(text))


def label(page: str) -> str:
    """`sn83020866/1828-07-23/ed-1/seq-4` -> `Cherokee Phoenix, 23 July 1828, page 4`."""
    _, date, _, seq = page.split("/")
    d = datetime.date.fromisoformat(date)
    return f"Cherokee Phoenix, {d.day} {MONTHS[d.month - 1]} {d.year}, page {seq.split('-')[1]}"


def stem(page: str, line_id: str) -> str:
    """The id of a line: `sn83020866_1828-07-23_ed-1_seq-4_P4_TL00078`."""
    return f"{page.replace('/', '_')}_{line_id}"


def similarity(a: str, b: str) -> float:
    return difflib.SequenceMatcher(None, a, b, autojunk=False).ratio()


# ---------- ALTO ----------

def _tag(e) -> str:
    return e.tag.rsplit("}", 1)[-1]


def alto_lines(path: str) -> tuple:
    """(page width, page height, [{"id", "box": [x, y, w, h], "words": [{"text", "box"}]}]), in the file's units."""
    root = ET.parse(path).getroot()
    page = next(e for e in root.iter() if _tag(e) == "Page")
    out = []
    for tl in root.iter():
        if _tag(tl) != "TextLine":
            continue
        words = [{"text": s.get("CONTENT", ""), "box": [int(s.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")]}
                 for s in tl if _tag(s) == "String"]
        out.append({"id": tl.get("ID"), "box": [int(tl.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")], "words": words})
    return float(page.get("WIDTH")), float(page.get("HEIGHT")), out


def line_number(line_id: str):
    m = re.search(r"TL0*(\d+)$", line_id or "")
    return int(m.group(1)) if m else None


def paired_lines(loc_path: str, prod_path: str, tolerance: float = 4.0) -> tuple:
    """([line], unmatched): LoC's lines that the re-read also has, as {"id", "box", "loc_words", "ours_words"}
    with LoC's boxes (LoC's units) and the re-read's words. The re-read numbers its lines afresh but keeps LoC's
    line boxes, in its own pixels, so a line pairs with the LoC line whose top left corner is nearest after
    scaling (within `tolerance` pixels, each line used once); re-read lines with no such partner count as unmatched."""
    lw, _, loc = alto_lines(loc_path)
    pw, _, prod = alto_lines(prod_path)
    scale = pw / lw
    order = sorted(range(len(loc)), key=lambda i: loc[i]["box"][1])
    ys = [loc[i]["box"][1] * scale for i in order]
    taken, out, bad = set(), [], 0
    for p in prod:
        px, py = p["box"][0], p["box"][1]
        best = None
        for r in range(bisect.bisect_left(ys, py - tolerance), len(order)):
            if ys[r] > py + tolerance:
                break
            i = order[r]
            d = abs(loc[i]["box"][0] * scale - px) + abs(ys[r] - py)
            if i not in taken and d <= 2 * tolerance and (best is None or d < best[0]):
                best = (d, i)
        if best is None:
            bad += 1
            continue
        taken.add(best[1])
        ln = loc[best[1]]
        out.append({"id": ln["id"], "box": ln["box"], "loc_words": ln["words"],
                    "ours_words": [w for s in p["words"] for w in s["text"].split()]})
    return out, bad


def syllabary_words(words: list) -> list:
    """Words with at least one syllabary letter, as {"text": letters only, "box", "raw"}; boxes are kept when given."""
    out = []
    for w in words:
        raw = w["text"] if isinstance(w, dict) else w
        t = letters(raw)
        if t:
            out.append({"text": t, "box": w.get("box") if isinstance(w, dict) else None, "raw": raw})
    return out


# ---------- which pairs make a task ----------

def usable_pair(a: str, b: str, min_similarity: float = 0.5) -> bool:
    """A pair worth showing: not two specks (one or two letters with none in common), and alike enough that
    one reading is the other with a letter or two misread rather than a different stretch of print."""
    if len(a) <= 2 and len(b) <= 2 and not set(a) & set(b):
        return False
    return similarity(a, b) >= min_similarity


def g_initial_only(a: str, b: str) -> bool:
    """The words differ only in a word-initial G-shaped letter: the Ꮐ-for-Ꮎ question, not a plain dispute."""
    return len(a) == len(b) and a[1:] == b[1:] and a[0] != b[0] and a[0] in G_SHAPED + NA and b[0] in G_SHAPED + NA


def disputes(lines: list, min_words: int = 3) -> tuple:
    """([candidate], rejected): lines of `paired_lines` whose syllabary words pair one to one and differ in
    exactly one pair that `usable_pair` accepts. rejected counts why the others were left out."""
    keep, why = [], collections.Counter()
    for ln in lines:
        loc, ours = syllabary_words(ln["loc_words"]), syllabary_words(ln["ours_words"])
        if len(loc) < min_words or not ours:
            why["short"] += 1
        elif len(loc) != len(ours):
            why["word_count_differs"] += 1
        else:
            diff = [i for i, (a, b) in enumerate(zip(loc, ours)) if a["text"] != b["text"]]
            if not diff:
                why["same"] += 1
            elif len(diff) > 1:
                why["several_words"] += 1
            elif g_initial_only(loc[diff[0]]["text"], ours[diff[0]]["text"]):
                why["initial_g"] += 1
            elif not usable_pair(loc[diff[0]]["text"], ours[diff[0]]["text"]):
                why["unusable_pair"] += 1
            elif min(loc[diff[0]]["box"][2:]) < 4:
                why["tiny_box"] += 1
            else:
                keep.append({**ln, "loc": loc, "ours": ours, "diff": diff})
                why["kept"] += 1
    return keep, why


def partners(pairs: list) -> dict:
    """{rare letter: Counter of the letters the other reading has in its place}, from (a, b) word pairs of one
    length where the words differ in single letters."""
    out = collections.defaultdict(collections.Counter)
    for a, b in pairs:
        if a == b or len(a) != len(b):
            continue
        for x, y in zip(a, b):
            if x != y and x in RARE:
                out[x][y] += 1
            if x != y and y in RARE:
                out[y][x] += 1
    return out


def rare_candidates(lines: list, known: set, freq: collections.Counter, part: dict, max_freq: int = 10) -> list:
    """Words with a rare letter, or that the word list says should have one, on paired lines. Each is
    {"line", "k", "letter", "kind", "a", "b", "a_is", "b_is"}: LoC's word k is shown as a, the other
    reading as b. `kind`: `ours-loc` (the readings differ and one has the letter), `agree` (both have it; b
    swaps it for a letter OCR confuses it with) and `word-list` (LoC's word is not a New Testament form and
    putting the letter in makes one; words OCR reads often are left alone, they are probably spelt so)."""
    out = []
    for ln in lines:
        loc, ours = syllabary_words(ln["loc_words"]), syllabary_words(ln["ours_words"])
        pair = len(loc) == len(ours)
        for k, lw in enumerate(loc):
            if not lw["box"] or min(lw["box"][2:]) < 4:
                continue
            a = lw["text"]
            b = ours[k]["text"] if pair else a
            first = len(out)  # a word gives one candidate: the first of these that applies
            if a != b:
                sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
                hit = [c for op, i1, i2, j1, j2 in sm.get_opcodes() if op != "equal" for c in a[i1:i2] + b[j1:j2] if c in RARE]
                if hit and usable_pair(a, b, 0.4):
                    out.append({"line": ln, "k": k, "letter": hit[0], "kind": "ours-loc", "a": a, "b": b, "a_is": "loc", "b_is": "chr_phoenix"})
            elif len(a) >= 3 and any(c in RARE for c in a):
                i, c = next((i, c) for i, c in enumerate(a) if c in RARE)
                alts = [x for x, _ in part.get(c, collections.Counter()).most_common(3)]
                known_alts = [x for x in alts if a[:i] + x + a[i + 1:] in known]
                pick = known_alts[0] if known_alts else (alts[0] if alts and part[c][alts[0]] >= 2 else None)
                if pick:
                    out.append({"line": ln, "k": k, "letter": c, "kind": "agree", "a": a, "b": a[:i] + pick + a[i + 1:],
                                "a_is": "ocr", "b_is": "word_list" if known_alts else "confusion"})
            if len(out) == first and a not in known and len(a) >= 3 and freq[a] <= max_freq and not any(c in RARE for c in a):
                found = next(((r, a[:i] + r + a[i + 1:]) for i in range(len(a)) for r in RARE if a[:i] + r + a[i + 1:] in known), None)
                if found:
                    out.append({"line": ln, "k": k, "letter": found[0], "kind": "word-list", "a": a, "b": found[1], "a_is": "ocr", "b_is": "word_list"})
    return out


def nah_candidates(lines: list, known: set) -> list:
    """Words OCR reads with a G-shaped first letter that the New Testament spells with Ꮎ (ᎾᏍᎩ read ᏩᏍᎩ):
    a = the Ꮎ spelling, b = the same with Ꮐ."""
    out = []
    for ln in lines:
        loc, ours = syllabary_words(ln["loc_words"]), syllabary_words(ln["ours_words"])
        if len(loc) != len(ours):
            continue
        for k, (lw, ow) in enumerate(zip(loc, ours)):
            w = lw["text"]
            if not lw["box"] or min(lw["box"][2:]) < 4 or len(w) < 2 or w[0] not in G_SHAPED + NA:
                continue
            if NA + w[1:] in known and NAH + w[1:] not in known:
                out.append({"line": ln, "k": k, "letter": NAH, "kind": "nah", "a": NA + w[1:], "b": NAH + w[1:],
                            "a_is": "na", "b_is": "nah", "read_as": {"loc": w, "chr_phoenix": ow["text"]}})
    return out


def spread(items: list, per_letter: int, per_page: int) -> list:
    """At most `per_letter` items per letter and `per_page` per letter and page, dealt round-robin over the
    pages so a few pages don't supply them all."""
    by_letter = collections.defaultdict(lambda: collections.defaultdict(list))
    for it in sorted(items, key=lambda it: -similarity(it["a"], it["b"])):
        by_letter[it["letter"]][it["line"]["page"]].append(it)
    out = []
    for letter in sorted(by_letter):
        pages = by_letter[letter]
        rank = 0
        chosen = []
        while len(chosen) < per_letter and rank < per_page:
            for page in sorted(pages):
                if rank < len(pages[page]) and len(chosen) < per_letter:
                    chosen.append(pages[page][rank])
            rank += 1
        out.extend(chosen)
    return out


# ---------- output ----------

def cut_line(scan, box: list, words: list, loc_width: float) -> tuple:
    """(line image, word boxes in it): the line's box padded by 15% of its height, from `scan` (a Pillow image of
    the page), with each LoC word's box [x, y, w, h] in the image's pixels. `loc_width` is the ALTO page width."""
    s = scan.size[0] / loc_width
    x, y, w, h = (v * s for v in box)
    pad = max(4.0, 0.15 * h)
    x0, y0 = max(0, int(x - pad)), max(0, int(y - pad))
    x1, y1 = min(scan.size[0], int(x + w + pad)), min(scan.size[1], int(y + h + pad))
    img = scan.crop((x0, y0, x1, y1)).convert("L")
    boxes = []
    for wd in words:
        bx, by, bw, bh = (v * s for v in wd["box"])
        bx0, by0 = max(0, bx - x0), max(0, by - y0)
        boxes.append([int(bx0), int(by0), max(1, int(min(bw, img.size[0] - bx0))), max(1, int(min(bh, img.size[1] - by0)))])
    return img, boxes


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("mode", choices=["disputes", "rare", "nah"])
    ap.add_argument("--prod-alto", required=True, help="folder of the re-read's ALTO files, <lccn>_<date>_ed-N_seq-N.<...>.xml")
    ap.add_argument("--loc-alto", required=True, help="LoC's batches unpacked: <lccn>/<yyyy>/<mm>/<dd>/ed-N/seq-N/ocr.xml")
    ap.add_argument("--scans", help="the page scans, <lccn>_<date>_ed-N_seq-N.jpg (needed unless --no-images)")
    ap.add_argument("--pages", required=True, help="phoenix-pages.jsonl (docs/reports/2026-11-cherokee)")
    ap.add_argument("--test-lines", required=True, help="the test set's lines.jsonl: its `line_id`s are the scripture lines")
    ap.add_argument("--wordlist", help="chr-attested.txt (rare and nah)")
    ap.add_argument("--per-letter", type=int, default=30)
    ap.add_argument("--per-page", type=int, default=2)
    ap.add_argument("--no-images", action="store_true")
    ap.add_argument("--out-dir", required=True)
    a = ap.parse_args()

    pages = [json.loads(x) for x in open(a.pages, encoding="utf-8") if x.strip()]
    scripture = {(r["page"], r["line_id"]) for r in map(json.loads, open(a.test_lines, encoding="utf-8"))}
    known = set(x.strip() for x in open(a.wordlist, encoding="utf-8")) if a.wordlist else set()
    held_out = a.mode == "disputes"
    all_lines, mismatched, missing = [], 0, []
    for pg in pages:
        # Disputes are scored on the test set's pages, which the model never trained on; the others feed
        # the glyph bank, which never takes a test-set page.
        if not pg["lines"] or bool(pg["test_set"]) != held_out:
            continue
        lccn, date, ed, seq = pg["page"].split("/")
        y, m, d = date.split("-")
        prod = sorted(glob.glob(os.path.join(a.prod_alto, f"{lccn}_{date}_{ed}_{seq}.*")))
        loc = os.path.join(a.loc_alto, lccn, y, m, d, ed, seq, "ocr.xml")
        if not prod or not os.path.exists(loc):
            missing.append(pg["page"])
            continue
        lines, bad = paired_lines(loc, prod[-1])
        mismatched += bad
        lw = alto_lines(loc)[0]
        for ln in lines:
            ln.update(page=pg["page"], loc_width=lw, kind="scripture" if (pg["page"], ln["id"]) in scripture else "news")
        all_lines.extend(lines)

    out = os.path.join(a.out_dir, a.mode)
    os.makedirs(os.path.join(out, "images"), exist_ok=True)
    report = {"pages_missing": missing, "lines_mismatched": mismatched, "lines": len(all_lines)}
    chosen = []  # [(line, {word index: (a, b, a_is, b_is)})]
    if a.mode == "disputes":
        keep, why = disputes(all_lines)
        keep.sort(key=lambda c: (c["kind"] != "news", c["page"], line_number(c["id"])))
        report["rejected"] = dict(why)
        report["kept_by_kind"] = dict(collections.Counter(c["kind"] for c in keep))
        for c in keep:
            k = c["diff"][0]
            chosen.append((c, {k: (c["loc"][k]["text"], c["ours"][k]["text"], "loc", "chr_phoenix")}))
    else:
        if a.mode == "rare":
            pairs = [(syllabary_words(ln["loc_words"]), syllabary_words(ln["ours_words"])) for ln in all_lines]
            part = partners([(x["text"], y["text"]) for lw, ow in pairs if len(lw) == len(ow) for x, y in zip(lw, ow)])
            freq = collections.Counter(w["text"] for lw, ow in pairs for w in lw + ow)
            cands = rare_candidates(all_lines, known, freq, part)
        else:
            cands = nah_candidates(all_lines, known)
        report["candidates"] = collections.Counter(c["letter"] + ":" + c["kind"] for c in cands)
        report["candidates"] = dict(report["candidates"])
        cands = spread(cands, a.per_letter, a.per_page)
        # Words whose readings come from different sources (OCR against OCR, OCR against the word list) go in
        # separate rows, since a row's `a_is` and `b_is` label every task made from it.
        per_line = collections.defaultdict(dict)
        for c in cands:
            key = (c["line"]["page"], c["line"]["id"], c["a_is"], c["b_is"])
            per_line[key].setdefault("line", c["line"])
            per_line[key][c["k"]] = (c["a"], c["b"], c["a_is"], c["b_is"])
        for v in per_line.values():
            ln = v.pop("line")
            chosen.append((ln, v))
        report["chosen"] = collections.Counter(c["letter"] for c in cands)
        report["chosen"] = dict(report["chosen"])

    scans = {}
    n_tasks = 0
    with open(os.path.join(out, "lines.jsonl"), "w", encoding="utf-8") as lines_out, \
            open(os.path.join(out, "manifest.jsonl"), "w", encoding="utf-8") as man, \
            open(os.path.join(out, "glyph-index.jsonl"), "w", encoding="utf-8") as gidx:
        for ln, swaps in chosen:
            sid = stem(ln["page"], ln["id"])
            loc = syllabary_words(ln["loc_words"])
            a_words = [{"text": swaps[k][0] if k in swaps else w["text"], "box": w["box"]} for k, w in enumerate(loc)]
            b_words = [swaps[k][1] if k in swaps else w["text"] for k, w in enumerate(loc)]
            row = {"id": sid, "image": f"images/{sid}.jpg", "page": ln["page"], "source": f"https://chroniclingamerica.loc.gov/lccn/{ln['page']}/",
                   "label": label(ln["page"]), "a": " ".join(w["text"] for w in a_words), "a_words": a_words, "b": " ".join(b_words),
                   "words": sorted(swaps),  # build_tasks.py pairs exactly these by position, never re-aligning
                   "a_is": next(iter(swaps.values()))[2], "b_is": next(iter(swaps.values()))[3]}
            if not a.no_images:
                from PIL import Image
                Image.MAX_IMAGE_PIXELS = None
                path = os.path.join(a.scans, ln["page"].replace("/", "_") + ".jpg")
                scan = scans.setdefault(path, Image.open(path))
                img, boxes = cut_line(scan, ln["box"], a_words, ln["loc_width"])
                img.save(os.path.join(out, row["image"]), quality=88)
                row["a_words"] = [{"text": w["text"], "box": b} for w, b in zip(a_words, boxes)]
                if a.mode != "disputes":  # the glyph bank reads these (`usnm-glyphs add-checked`); it never takes test-set pages
                    sc = scan.size[0] / ln["loc_width"]
                    for k, (ta, tb, _, _) in swaps.items():
                        gidx.write(json.dumps({"word": f"{sid}~{k}", "page": ln["page"], "a": ta, "b": tb,
                                               "line": [round(v * sc, 1) for v in (ln["box"][0], ln["box"][1], ln["box"][0] + ln["box"][2], ln["box"][1] + ln["box"][3])],
                                               "box": [round(v * sc, 1) for v in (loc[k]["box"][0], loc[k]["box"][1], loc[k]["box"][0] + loc[k]["box"][2], loc[k]["box"][1] + loc[k]["box"][3])]},
                                              ensure_ascii=False, sort_keys=True) + "\n")
                if len(scans) > 4:
                    scans.pop(next(iter(scans))).close()
            lines_out.write(json.dumps(row, ensure_ascii=False) + "\n")
            man.write(json.dumps({"id": sid, "page": ln["page"], "kind": ln["kind"], "mode": a.mode, "loc": [w["text"] for w in loc],
                                  "ours": [w["text"] for w in syllabary_words(ln["ours_words"])],
                                  "words": sorted(swaps), "a_is": row["a_is"], "b_is": row["b_is"]}, ensure_ascii=False, sort_keys=True) + "\n")
            n_tasks += len(swaps)
    report["lines_out"] = len(chosen)
    report["words_out"] = n_tasks
    json.dump(report, open(os.path.join(out, "report.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1, sort_keys=True)
    print(json.dumps(report, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    sys.exit(main())
