"""Blind word-pick tasks for check.usnewsmap.com: our Tesseract against LoC's OCR on Chronicling America English
body text (#2122, route 2). The pick-between-readings format of en_pick.py (#1867), with the second reading's words
placed by their own boxes instead of by text alignment.

Input (--pages) is JSON lines, one page each, as english_pages.py writes them:

  {"page": "<lccn>/<date>/ed-N/seq-N", "page_id": "<doc id>", "source": "<page URL>", "date": "YYYY-MM-DD",
   "label": "<shown above the line>", "alto": "<LoC ALTO>", "image": "<the scan>", "tsv": "<Tesseract TSV of the scan>",
   "damage_rate": 0.18, "regions": [{"id": "<region id>", "box": [x0, y0, x1, y1], "text": "<truth>"}]}

Paths are relative to --root (default: the folder of --pages). `regions` is for gold pages only (a human transcription
of some boxes, in scan pixels): a page with regions gives only tasks inside them, each with which reading matches it.

Words. LoC's words are the ALTO's Strings in reading order with a line's HYP kept on its last word, mapped onto the
scan by proportion (en_pick.parse_alto). Tesseract's are the TSV's level-5 rows in its output order.

Alignment, by box overlap (scan pixels). A LoC word i and a Tesseract word j are paired when
  * j is the Tesseract word with the largest intersection over union (IoU) with i, and i is the LoC word with the
    largest IoU with j (mutual best);
  * that IoU is at least --min-iou;
  * no other Tesseract word covers --max-other or more of i's box and no other LoC word covers that much of j's
    (one engine split what the other read as one word, or joined two).
Every LoC word that is not paired is counted by why: no Tesseract word overlaps it, another word covers it (split or
joined), or the best overlap is under --min-iou. A page where fewer than --min-page-aligned of LoC's words pair is
left out whole (the boxes do not line up: a different image, a rotation), and its words are counted.

Readings. Both words are normalised as the #2122 scorer does (score2122.py at b045416f; normalise() below is a copy):
NFKC, lowercase, line-end hyphens joined, punctuation removed except apostrophes inside words. A pair is
  * an agreement when the normalised readings are equal (counted as exact when the raw readings are too, else as
    agreeing after normalisation: case or punctuation only);
  * left out, counted by reason, when: the word is cut by a line-end hyphen in either reading (the word before the
    break and the first word of the next line: each half is part of a word, and the halves are scored joined);
    its LoC line is a merged line (merged_lines.py, #1978); either reading has no letter or digit once normalised
    (punctuation only); or either normalises to more than one word (U.S. against US);
  * otherwise a disagreement, and a candidate task.
German (--text-lang deu, #2127's US test). Each reading first gets #2127's normalisation
(scripts/ocr-survey/frak2127/frak_norm.py: NFC, long s to s, r rotunda to r, every quote mark to ", every dash and
hyphen to -), then the normalisation above. The hyphen-cut rule reads the #2127-normalised text, so Fraktur's
double hyphen (⸗, =) cuts a word as - does. Alignment, the reasons and the sampling are unchanged.
The page shows the normalised readings (small letters, no punctuation), so a volunteer judges exactly what is scored.

Sampling. Per page, up to --per-page candidates, those first by sha256(seed:task id:rank) (a simple random sample of
the page's disagreements, with no de-duplication, so each page's tasks can be weighted back to its candidates).

Which reading is `a` is drawn from a secret side key and the task id, as en_pick.py does: --side-key FILE (default:
OUT_DIR/side-key.private, kept if it exists, else a new random key; never loaded, stays on the build host).

Writes OUT_DIR/clips/<lang>/<set>/{lines,words}/..., OUT_DIR/tasks.jsonl, OUT_DIR/language.json, OUT_DIR/report.json
(totals and what was left out and why) and OUT_DIR/pages.jsonl (the same counts per page: the denominators for a
word error rate difference).

A task's `private` (kept off /api/next, and out of /api/export until the task settles: edge/check/README.md, "Blind
tasks") holds `a_is` and `b_is` (`tesseract` or `loc`), `page_id`, `loc_word` and `tess_word` (word indexes),
`iou` and `other_cover` (the alignment's confidence), `similarity` (of the two normalised readings), `raw` (both
readings as the engines gave them), `tess_conf`, `page_candidates` and `page_sampled` (for weights), `strata`
(`decade`, `damage_band`), and on a gold page `truth` (`tesseract`, `loc` or `neither`: which reading equals the
normalised truth word), `truth_word` and `region`.

  python3 tess_pick.py --pages pages.jsonl --lang en --set tess-loc-v1 --language-config languages/en.json \
    --out-dir OUT [--text-lang eng|deu] [--seed 2122] [--per-page 6] [--min-iou 0.5] [--max-other 0.2] [--min-page-aligned 0.5]
"""
import argparse
import hashlib
import json
import os
import re
import sys
import unicodedata
from collections import Counter
from difflib import SequenceMatcher

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)  # `python3 -I` leaves the script directory off the path
import build_tasks as bt  # noqa: E402
import en_pick  # noqa: E402
import merged_lines  # noqa: E402

READINGS = ("tesseract", "loc")
MAX_LINE_WIDTH = en_pick.MAX_LINE_WIDTH

# ---------------------------------------------------------------- normalisation (score2122.py at b045416f, verbatim)

HYPHENS = "-‐­¬"
APOS = {"'", "‘", "’", "ʼ"}
LINE_END_HYPHEN = re.compile(r"(?<=\w)[" + HYPHENS + r"][ \t]*\r?\n\s*(?=\w)")


def normalise(text):
    t = unicodedata.normalize("NFKC", text).lower()
    t = LINE_END_HYPHEN.sub("", t)
    t = t.replace("­", "")
    chars = []
    for ch in t:
        if ch in APOS:
            chars.append("'")
        elif unicodedata.category(ch).startswith("P"):
            chars.append(" ")
        else:
            chars.append(ch)
    out = []
    for i, ch in enumerate(chars):
        if ch == "'" and not (0 < i < len(chars) - 1 and chars[i - 1].isalnum() and chars[i + 1].isalnum()):
            out.append(" ")
        else:
            out.append(ch)
    return " ".join("".join(out).split())


_PRE = {"eng": None}


def prenorm(text_lang):
    """The normalisation a reading gets before normalise(): none for English, #2127's frak_norm for German."""
    if text_lang not in _PRE:
        if text_lang != "deu":
            raise SystemExit(f"--text-lang {text_lang}: eng or deu")
        sys.path.insert(0, os.path.join(HERE, "..", "..", "..", "scripts", "ocr-survey", "frak2127"))
        import frak_norm
        _PRE["deu"] = frak_norm.normalise
    return _PRE[text_lang]


def norm(text, text_lang="eng"):
    pre = prenorm(text_lang)
    return normalise(pre(text) if pre else text)


# ---------------------------------------------------------------- words


def parse_tsv(tsv: str) -> list:
    """Tesseract's words in output order: {text, box: [x, y, w, h], line: index, conf, line_box: [x0, y0, x1, y1]}."""
    words, line_of = [], {}
    for ln in tsv.splitlines()[1:]:
        f = ln.split("\t")
        if len(f) < 12 or f[0] != "5" or not f[11].strip():
            continue
        key = tuple(f[1:5])
        if key not in line_of:
            line_of[key] = len(line_of)
        x, y, w, h = (int(v) for v in f[6:10])
        words.append({"text": f[11], "box": [x, y, w, h], "line": line_of[key], "conf": float(f[10])})
    _line_boxes(words)
    return words


def _line_boxes(words: list) -> None:
    by_line = {}
    for w in words:
        by_line.setdefault(w["line"], []).append(w)
    for ws in by_line.values():
        box = [min(w["box"][0] for w in ws), min(w["box"][1] for w in ws),
               max(w["box"][0] + w["box"][2] for w in ws), max(w["box"][1] + w["box"][3] for w in ws)]
        for w in ws:
            w["line_box"] = box


def area(b) -> float:
    return max(0.0, b[2]) * max(0.0, b[3])


def inter(a, b) -> float:
    w = min(a[0] + a[2], b[0] + b[2]) - max(a[0], b[0])
    h = min(a[1] + a[3], b[1] + b[3]) - max(a[1], b[1])
    return w * h if w > 0 and h > 0 else 0.0


def iou(a, b) -> float:
    i = inter(a, b)
    u = area(a) + area(b) - i
    return i / u if u > 0 else 0.0


class Grid:
    """Boxes bucketed by rows of `step` pixels, for overlap queries."""

    def __init__(self, boxes: list, step: int = 64):
        self.step, self.rows = step, {}
        for k, b in enumerate(boxes):
            for r in range(int(b[1]) // step, int(b[1] + b[3]) // step + 1):
                self.rows.setdefault(r, []).append(k)
        self.boxes = boxes

    def overlapping(self, b) -> list:
        seen = set()
        for r in range(int(b[1]) // self.step, int(b[1] + b[3]) // self.step + 1):
            seen.update(self.rows.get(r, ()))
        return [(k, inter(b, self.boxes[k])) for k in sorted(seen) if inter(b, self.boxes[k]) > 0]


def align(loc: list, tess: list, min_iou: float, max_other: float) -> tuple:
    """({i: (j, iou, other_cover)} for paired words, {i: reason} for the other LoC words, Tesseract words unpaired)."""
    lb, tb = [w["box"] for w in loc], [w["box"] for w in tess]
    tg, lg = Grid(tb), Grid(lb)
    t_over = [tg.overlapping(b) for b in lb]
    best_i_of_j = {}
    for j, b in enumerate(tb):
        cands = [(iou(lb[i], b), -i) for i, _ in lg.overlapping(b)]
        if cands:
            best_i_of_j[j] = -max(cands)[1]
    pairs, why = {}, {}
    for i, over in enumerate(t_over):
        if not over:
            why[i] = "no_tesseract_word"
            continue
        score, j = max((iou(lb[i], tb[j]), -j) for j, _ in over)
        j = -j
        a_i = area(lb[i]) or 1.0
        others_on_i = max((c / a_i for k, c in over if k != j), default=0.0)
        a_j = area(tb[j]) or 1.0
        others_on_j = max((c / a_j for k, c in lg.overlapping(tb[j]) if k != i), default=0.0)
        other = max(others_on_i, others_on_j)
        if other >= max_other:
            why[i] = "split_or_joined"
        elif score < min_iou:
            why[i] = "low_overlap"
        elif best_i_of_j.get(j) != i:
            why[i] = "not_mutual_best"
        else:
            pairs[i] = (j, round(score, 4), round(other, 4))
    paired_t = {j for j, _, _ in pairs.values()}
    return pairs, why, len(tess) - len(paired_t)


def hyphen_cut(words: list, text_lang: str = "eng") -> set:
    """Indexes of words cut by a hyphen: a word ending in one, and the first word of the next line after it."""
    out, pre = set(), prenorm(text_lang)
    for k, w in enumerate(words):
        t = (pre(w["text"]) if pre else w["text"]).rstrip()
        if t and t[-1] in HYPHENS:
            out.add(k)
            if k + 1 < len(words) and words[k + 1]["line"] != w["line"]:
                out.add(k + 1)
    return out


def merged(words: list) -> set:
    """Indexes of LoC words on a merged line (merged_lines.py), from the lines' boxes."""
    boxes = {}
    for w in words:
        boxes.setdefault(w["line"], w["line_box"])
    lines = [{"id": k, "box": [b[0], b[1], b[2] - b[0], b[3] - b[1]]} for k, b in boxes.items()]
    _, flagged = merged_lines.flag_page(lines)
    bad = {f["id"] for f in flagged}
    return {k for k, w in enumerate(words) if w["line"] in bad}


def who_is_right(loc: str, tess: str, truth: str) -> str:
    if tess == truth and loc != truth:
        return "tesseract"
    if loc == truth and tess != truth:
        return "loc"
    return "neither"


def truth_words(loc: list, regions: list, text_lang: str = "eng") -> dict:
    """{LoC word index: (region id, normalised truth word)} for words whose box centre is in a region and that the
    region's truth aligns to (en_pick.truth_for on normalised words)."""
    out = {}
    for reg in regions:
        inside = _in_box(loc, reg["box"])
        toks = [norm(loc[k]["text"], text_lang) or "∅" for k in inside]
        for pos, word in en_pick.truth_for(toks, norm(reg["text"], text_lang).split()).items():
            out[inside[pos]] = (reg["id"], word)
    return out


def _in_box(words: list, box: list) -> list:
    x0, y0, x1, y1 = box
    return [k for k, w in enumerate(words)
            if x0 <= w["box"][0] + w["box"][2] / 2 < x1 and y0 <= w["box"][1] + w["box"][3] / 2 < y1]


def side(key, task_id: str) -> str:
    """Which reading is `a`, fixed by the secret side key and the task id (as en_pick.side)."""
    return READINGS[hashlib.sha256(f"{key}:{task_id}:side".encode()).digest()[0] & 1]


def page_candidates(page: dict, loc: list, tess: list, args, stats: Counter) -> tuple:
    """(candidate rows, per-page counts) for one page."""
    c = Counter(loc_words=len(loc), tess_words=len(tess))
    pairs, why, t_unpaired = align(loc, tess, args.min_iou, args.max_other)
    c["tess_unpaired"] = t_unpaired
    c["aligned"] = len(pairs)
    if not loc or len(pairs) / len(loc) < args.min_page_aligned:
        c["page_alignment_failed"] = 1
        stats.update(c)
        return [], c
    for r in why.values():
        c["unaligned_" + r] += 1
    tl = getattr(args, "text_lang", "eng")
    cut_l, cut_t, merged_l = hyphen_cut(loc, tl), hyphen_cut(tess, tl), merged(loc)
    truth = truth_words(loc, page["regions"], tl) if page.get("regions") else None
    key = en_pick.slug(page["page"])
    rows = []
    for i, (j, score, other) in sorted(pairs.items()):
        raw_l, raw_t = loc[i]["text"], tess[j]["text"]
        if i in merged_l:
            c["left_out_merged_line"] += 1
            continue
        if i in cut_l or j in cut_t:
            c["left_out_hyphen_cut"] += 1
            continue
        nl, nt = norm(raw_l, tl), norm(raw_t, tl)
        if nl == nt:
            c["agree_exact" if raw_l == raw_t else "agree_after_normalisation"] += 1
            continue
        if not nl or not nt:
            c["left_out_punctuation_only"] += 1
            continue
        if " " in nl or " " in nt:
            c["left_out_several_words"] += 1
            continue
        c["disagree"] += 1
        row = {"id": f"{args.lang}/{args.set}/{key}~{i}", "page": page, "i": i, "j": j, "loc": nl, "tesseract": nt,
               "raw": {"loc": raw_l, "tesseract": raw_t}, "iou": score, "other": other, "conf": tess[j]["conf"],
               "loc_box": loc[i]["box"], "tess_box": tess[j]["box"], "line_box": loc[i]["line_box"],
               "similarity": round(SequenceMatcher(None, nl, nt, autojunk=False).ratio(), 4)}
        if truth is not None:
            if i not in truth:
                c["outside_truth"] += 1  # a gold page gives tasks only where its truth aligns
                continue
            row["region"], row["truth_word"] = truth[i]
            row["truth"] = who_is_right(nl, nt, row["truth_word"])
            c["truth_" + row["truth"]] += 1
        rows.append(row)
    c["candidates"] = len(rows)
    stats.update(c)
    return rows, c


def sample(rows: list, per_page: int, seed) -> list:
    picked = sorted(rows, key=lambda r: en_pick.rank(seed, r["id"]))[:per_page]
    for r in picked:
        r["page_candidates"], r["page_sampled"] = len(rows), len(picked)
    return picked


def union(a, b) -> list:
    x0, y0 = min(a[0], b[0]), min(a[1], b[1])
    return [x0, y0, max(a[0] + a[2], b[0] + b[2]) - x0, max(a[1] + a[3], b[1] + b[3]) - y0]


def write_task(r: dict, args, img, out_dir: str, f) -> None:
    page = r["page"]
    x, y, w, h = union(r["loc_box"], r["tess_box"])
    prefix = f"{args.lang}/{args.set}"
    stem = r["id"].split("/", 2)[2].replace("~", "_w")
    word_key, line_key = f"{prefix}/words/{stem}.jpg", f"{prefix}/lines/{stem}.jpg"
    for k in (word_key, line_key):
        os.makedirs(os.path.dirname(os.path.join(out_dir, "clips", k)), exist_ok=True)
    wimg, _ = en_pick.clip(img, [x, y, w, h], max(4, h // 4), max(3, h // 6))
    wimg.save(os.path.join(out_dir, "clips", word_key), quality=args.quality)
    lx0, ly0, lx1, ly1 = r["line_box"]
    lb = union([lx0, ly0, lx1 - lx0, ly1 - ly0], [x, y, w, h])
    limg, (cx0, cy0, cx1, cy1) = en_pick.clip(img, lb, max(6, int(h // 2)), max(4, int(h // 3)))
    if limg.size[0] > MAX_LINE_WIDTH:
        limg = limg.resize((MAX_LINE_WIDTH, max(1, round(limg.size[1] * MAX_LINE_WIDTH / limg.size[0]))))
    limg.save(os.path.join(out_dir, "clips", line_key), quality=args.quality)
    cw, ch = cx1 - cx0, cy1 - cy0
    a_is = side(args.side_key, r["id"])
    b_is = READINGS[1 - READINGS.index(a_is)]
    a, b = r[a_is], r[b_is]
    ad, bd = bt.letter_diff(a, b, {})
    private = {"a_is": a_is, "b_is": b_is, "iou": r["iou"], "loc_word": r["i"], "other_cover": r["other"],
               "page_candidates": r["page_candidates"], "page_id": page.get("page_id"), "page_sampled": r["page_sampled"],
               "raw": r["raw"], "similarity": r["similarity"],
               "strata": {"damage_band": en_pick.band(page.get("damage_rate")), "decade": en_pick.decade(page["date"])},
               "tess_conf": r["conf"], "tess_word": r["j"]}
    if "truth" in r:
        private.update(region=r["region"], truth=r["truth"], truth_word=r["truth_word"])
    f.write(json.dumps({
        "id": r["id"], "lang": args.lang, "set": args.set, "page": page["page"], "source": page.get("source"),
        "label": page.get("label"), "line": line_key, "word": word_key,
        "box": [round((x - cx0) / cw, 4), round((y - cy0) / ch, 4), round(w / cw, 4), round(h / ch, 4)],
        "a": a, "b": b, "a_diff": ad, "b_diff": bd, "private": private,
    }, ensure_ascii=False) + "\n")


def load_side_key(args) -> str:
    key_file = args.side_key or os.path.join(args.out_dir, "side-key.private")
    if os.path.exists(key_file):
        return open(key_file, encoding="utf-8").read().strip()
    if args.side_key:
        raise SystemExit(f"--side-key {args.side_key}: no such file")
    key = os.urandom(16).hex()
    fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(key + "\n")
    return key


LEFT_OUT = ("unaligned_no_tesseract_word", "unaligned_split_or_joined", "unaligned_low_overlap", "unaligned_not_mutual_best",
            "left_out_merged_line", "left_out_hyphen_cut", "left_out_punctuation_only", "left_out_several_words")


def summary(stats: Counter, per_page: list) -> dict:
    rep = dict(sorted(stats.items()))
    differing = stats["disagree"] + sum(stats[k] for k in LEFT_OUT if k.startswith("left_out"))
    rep["shares_of_loc_words"] = {k: round(stats[k] / stats["loc_words"], 4) for k in
                                  ("aligned", "agree_exact", "agree_after_normalisation", "disagree") + LEFT_OUT
                                  if stats["loc_words"]}
    rep["aligned_words_left_out_share"] = round(
        sum(stats[k] for k in LEFT_OUT if k.startswith("left_out")) / differing, 4) if differing else None
    counts = sorted(p["disagree"] for p in per_page if not p.get("page_alignment_failed"))
    rep["disagreements_per_page"] = {"pages": len(counts), "min": counts[0] if counts else 0,
                                     "median": counts[len(counts) // 2] if counts else 0,
                                     "max": counts[-1] if counts else 0}
    return rep


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--pages", required=True)
    ap.add_argument("--root", help="folder the pages' paths are relative to (default: the folder of --pages)")
    ap.add_argument("--lang", default="en")
    ap.add_argument("--set", required=True)
    ap.add_argument("--language-config", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--text-lang", default="eng", choices=("eng", "deu"),
                    help="deu: #2127's normalisation (frak_norm.py) before the one above")
    ap.add_argument("--seed", default="2122", help="seeds the sampling (public)")
    ap.add_argument("--side-key", help="file holding the secret that decides which reading is a (default: a new random one)")
    ap.add_argument("--per-page", type=int, default=6)
    ap.add_argument("--min-iou", type=float, default=0.5)
    ap.add_argument("--max-other", type=float, default=0.2, help="another word covering this share of a box makes it a split or join")
    ap.add_argument("--min-page-aligned", type=float, default=0.5, help="leave out a page where fewer of LoC's words pair")
    ap.add_argument("--no-clips", action="store_true", help="count only: write the reports and no tasks or images")
    ap.add_argument("--quality", type=int, default=85)
    args = ap.parse_args(argv)
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = None  # newspaper scans are large

    root = args.root or os.path.dirname(os.path.abspath(args.pages))
    os.makedirs(args.out_dir, exist_ok=True)
    args.side_key = load_side_key(args)
    stats, per_page = Counter(), []
    tasks = open(os.path.join(args.out_dir, "tasks.jsonl"), "w", encoding="utf-8") if not args.no_clips else None
    for page in en_pick.read_jsonl(args.pages):
        stats["pages"] += 1
        img_path = os.path.join(root, page["image"])
        with Image.open(img_path) as im:
            size = im.size
        loc = en_pick.parse_alto(open(os.path.join(root, page["alto"]), "rb").read(), size)
        tess = parse_tsv(open(os.path.join(root, page["tsv"]), encoding="utf-8").read())
        rows, c = page_candidates(page, loc, tess, args, stats)
        picked = sample(rows, args.per_page, args.seed)
        stats["sampled"] += len(picked)
        per_page.append({"page": page["page"], "page_id": page.get("page_id"), "date": page["date"],
                         "damage_rate": page.get("damage_rate"), "sampled": len(picked), **dict(sorted(c.items()))})
        if tasks and picked:
            img = Image.open(img_path).convert("L")
            for r in sorted(picked, key=lambda r: r["id"]):
                write_task(r, args, img, args.out_dir, tasks)
    if tasks:
        tasks.close()
        cfg = json.load(open(args.language_config, encoding="utf-8"))
        language = {k: v for k, v in cfg.items() if k not in bt.BUILD_ONLY}
        language["examples"] = {}
        with open(os.path.join(args.out_dir, "language.json"), "w", encoding="utf-8") as f:
            json.dump(language, f, ensure_ascii=False, indent=1)
    with open(os.path.join(args.out_dir, "pages.jsonl"), "w", encoding="utf-8") as f:
        for p in per_page:
            f.write(json.dumps(p) + "\n")
    rep = summary(stats, per_page)
    with open(os.path.join(args.out_dir, "report.json"), "w", encoding="utf-8") as f:
        json.dump(rep, f, indent=1)
    print(json.dumps(rep, indent=1))


if __name__ == "__main__":
    main()
