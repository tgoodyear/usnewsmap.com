"""Blind word-pick tasks for check.usnewsmap.com: Xiaomi-OCR-0 against LoC's OCR on Chronicling America English (#1867).

Input (--pages) is JSON lines, one page each:

  {"page": "<lccn>/<date>/ed-N/seq-N", "source": "<page URL>", "date": "YYYY-MM-DD", "label": "<shown above the line>",
   "alto": "<LoC ALTO file>", "image": "<the page image the ALTO belongs to>", "damage_rate": 0.18,
   "crops": [{"id": "<crop id>", "box": [x0, y0, x1, y1], "xiaomi": "<Xiaomi's text of the crop>", "truth": "<optional>"}]}

`box` is the crop in the page image's pixels. `damage_rate` is the page's damage rate (the share of LoC words a
dictionary doesn't know, as quality.page_summary has it); without it the page's damage band is "unknown".
`truth` is a human transcription of the crop (en-op and en-as): with it a task also carries which reading is right.
Paths are relative to --root (default: the folder of --pages).

For each crop it takes LoC's words whose box centre is in the crop (ALTO reading order, a line's hyphen kept on its
last word, as `usnm-ocr-survey alto-crop` does), aligns them with Xiaomi's whitespace-separated words and keeps the
words where the two differ. A pair is made only when it is certain:

  * the two readings agree on most of the crop (--min-agree): if not, they cover different stretches of print;
  * the differing stretch is bracketed by words both readings agree on (a word at the very start or end of a
    crop is cut by the crop's edge), and has as many words on each side, at most --max-block, paired by position;
  * each pair is at least --min-similarity alike (a word one reading split, joined or dropped is not a pair);
  * neither word is without letters or digits; with --skip-punctuation-only, a pair whose letters and digits agree is
    left out too (on by default; --keep-punctuation-only to keep them).

Every other differing word is counted and left out, never guessed. Per page it keeps up to --per-page words
(at most one for each distinct pair of readings), drawn with --seed, then at most --per-stratum for each decade and
damage band. Which reading is `a` is drawn from the seed and the task id, so neither side is always first.

Writes OUT_DIR/clips/en/<set>/{lines,words}/..., OUT_DIR/tasks.jsonl, OUT_DIR/language.json and OUT_DIR/report.json
(the counts: differing words, what was left out and why, and, with truth, how often each reading is right).

A task's `private` holds what must not reach a volunteer before the task settles: `a_is` and `b_is` (`xiaomi` or
`loc`), the strata (`decade`, `damage_band`, `crop`), and with truth `truth` (`xiaomi`, `loc` or `neither`: which
reading matches the truth word exactly) with `truth_word`. The Worker keeps `private` off /api/next and out of
/api/export until the task settles (edge/check/README.md, "Blind tasks").

  python3 en_pick.py --pages pages.jsonl --lang en --set xiaomi-loc-v1 --language-config languages/en.json \
    --out-dir OUT [--seed 1] [--per-page 8] [--per-stratum 500] [--min-similarity 0.5]
"""
import argparse
import hashlib
import importlib.util
import json
import os
import random
import re
import xml.etree.ElementTree as ET
from collections import Counter
from difflib import SequenceMatcher

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("build_tasks", os.path.join(HERE, "build_tasks.py"))
bt = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bt)

BANDS = (("fine", 0.10), ("moderate", 0.25))  # as docs/reports/2026-10-ocr-damage-ranking: <=0.1 fine, <=0.25 moderate
MAX_LINE_WIDTH = 1600


def band(rate) -> str:
    if rate is None:
        return "unknown"
    for name, top in BANDS:
        if rate <= top:
            return name
    return "bad"


def decade(date: str) -> int:
    return int(date[:4]) // 10 * 10


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def parse_alto(xml: bytes, image_size: tuple) -> list:
    """LoC's words in reading order: {text, box: [x, y, w, h] in image pixels, line: index, line_box}. The ALTO's
    page extent is mapped onto the image by proportion, as usnm-ocr-survey's position::load does."""
    root = ET.fromstring(xml)
    page = next(e for e in root.iter() if _local(e.tag) == "Page")
    sx = image_size[0] / float(page.get("WIDTH"))
    sy = image_size[1] / float(page.get("HEIGHT"))
    words, line_no = [], -1
    for line in root.iter():
        if _local(line.tag) != "TextLine":
            continue
        line_no += 1
        first = len(words)
        for el in line:
            tag = _local(el.tag)
            if tag == "String" and el.get("CONTENT"):
                x, y, w, h = (float(el.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
                words.append({"text": el.get("CONTENT"), "box": [x * sx, y * sy, w * sx, h * sy], "line": line_no})
            elif tag == "HYP" and len(words) > first and el.get("CONTENT"):
                words[-1]["text"] += el.get("CONTENT")
        if len(words) > first:
            xs = [w["box"][0] for w in words[first:]] + [w["box"][0] + w["box"][2] for w in words[first:]]
            ys = [w["box"][1] for w in words[first:]] + [w["box"][1] + w["box"][3] for w in words[first:]]
            for w in words[first:]:
                w["line_box"] = [min(xs), min(ys), max(xs), max(ys)]
    return words


def in_crop(words: list, box: list) -> list:
    x0, y0, x1, y1 = box
    return [w for w in words
            if x0 <= w["box"][0] + w["box"][2] / 2 < x1 and y0 <= w["box"][1] + w["box"][3] / 2 < y1]


def levenshtein(a: str, b: str) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def aligned_words(a: list, b: list, min_similarity: float, max_block: int) -> tuple:
    """(pairs [(i, j)] of differing words that are certain, counts). See the module docstring for the rules."""
    sm = SequenceMatcher(None, a, b, autojunk=False)
    c = Counter()
    pairs = []
    n, m = len(a), len(b)
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "equal":
            continue
        c["blocks"] += 1
        c["differing_loc_words"] += i2 - i1
        if i1 == 0 or j1 == 0 or i2 == n or j2 == m:
            c["left_out_at_crop_edge"] += i2 - i1
        elif op != "replace" or i2 - i1 != j2 - j1:
            c["left_out_split_joined_or_dropped"] += i2 - i1
        elif i2 - i1 > max_block:
            c["left_out_long_block"] += i2 - i1
        else:
            for k in range(i2 - i1):
                i, j = i1 + k, j1 + k
                if SequenceMatcher(None, a[i], b[j], autojunk=False).ratio() < min_similarity:
                    c["left_out_dissimilar"] += 1
                else:
                    pairs.append((i, j))
    return pairs, c


def truth_for(loc: list, truth: list) -> dict:
    """{index in loc: the truth word at that place}, for words LoC read the same as the truth and for 1:1 replacements."""
    out = {}
    for op, i1, i2, j1, j2 in SequenceMatcher(None, loc, truth, autojunk=False).get_opcodes():
        if op == "equal" or (op == "replace" and i2 - i1 == j2 - j1):
            out.update((i1 + k, truth[j1 + k]) for k in range(i2 - i1))
    return out


def who_is_right(loc: str, xiaomi: str, truth: str) -> str:
    if xiaomi == truth and loc != truth:
        return "xiaomi"
    if loc == truth and xiaomi != truth:
        return "loc"
    return "neither"


def side(seed, task_id: str) -> str:
    """Which reading is `a`: 'xiaomi' or 'loc', fixed by the seed and the task id."""
    return "xiaomi" if hashlib.sha256(f"{seed}:{task_id}:side".encode()).digest()[0] & 1 else "loc"


def rank(seed, task_id: str) -> str:
    return hashlib.sha256(f"{seed}:{task_id}:rank".encode()).hexdigest()


def slug(text: str) -> str:
    return re.sub(r"[^A-Za-z0-9._-]+", "_", text).strip("_")


def read_jsonl(path: str) -> list:
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def candidates(page: dict, args, root: str, stats: Counter) -> list:
    """Every certain differing word of one page, as dicts (no images yet)."""
    from PIL import Image
    with Image.open(os.path.join(root, page["image"])) as im:
        size = im.size
    with open(os.path.join(root, page["alto"]), "rb") as f:
        words = parse_alto(f.read(), size)
    key = slug(page["page"])
    out = []
    for crop in page["crops"]:
        stats["crops"] += 1
        loc_words = in_crop(words, crop["box"])
        loc = [bt.clean(w["text"]) for w in loc_words]
        xi = bt.clean(crop["xiaomi"]).split()
        stats["loc_words"] += len(loc)
        stats["xiaomi_words"] += len(xi)
        if not loc or not xi:
            stats["crops_without_a_reading"] += 1
            continue
        if SequenceMatcher(None, loc, xi, autojunk=False).ratio() < args.min_agree:
            stats["crops_left_out_readings_disagree_too_much"] += 1
            continue
        stats["crops_used"] += 1
        pairs, c = aligned_words(loc, xi, args.min_similarity, args.max_block)
        stats.update(c)
        tw = truth_for(loc, bt.clean(crop["truth"]).split()) if crop.get("truth") else None
        seen = set()
        for i, j in pairs:
            wl, wx = loc[i], xi[j]
            if not bt.has_letter(wl) or not bt.has_letter(wx):
                stats["left_out_no_letters"] += 1
                continue
            if args.skip_punctuation_only and bt.letters_only(wl, {}) == bt.letters_only(wx, {}):
                stats["left_out_punctuation_only"] += 1
                continue
            if (wl, wx) in seen:
                stats["left_out_repeated_pair"] += 1
                continue
            seen.add((wl, wx))
            stats["candidates"] += 1
            task_id = f"{args.lang}/{args.set}/{key}_{slug(str(crop['id']))}~{i}"
            row = {"id": task_id, "page": page, "crop": crop["id"], "k": i, "loc": wl, "xiaomi": wx, "word": loc_words[i]}
            if tw is not None:
                if i not in tw:
                    stats["left_out_no_truth_word"] += 1
                    continue
                row["truth_word"] = tw[i]
                row["truth"] = who_is_right(wl, wx, tw[i])
                stats["truth_" + row["truth"]] += 1
                row["closer"] = ("xiaomi" if levenshtein(wx, tw[i]) < levenshtein(wl, tw[i])
                                 else "loc" if levenshtein(wl, tw[i]) < levenshtein(wx, tw[i]) else "tie")
                stats["closer_" + row["closer"]] += 1
            out.append(row)
    return out


def sample(rows: list, args) -> list:
    """Up to --per-page words per page, then up to --per-stratum per decade and damage band, by seeded rank."""
    by_page = {}
    for r in rows:
        by_page.setdefault(r["page"]["page"], []).append(r)
    kept = []
    for page_rows in by_page.values():
        kept.extend(sorted(page_rows, key=lambda r: rank(args.seed, r["id"]))[:args.per_page])
    if args.per_stratum:
        strata = {}
        for r in sorted(kept, key=lambda r: rank(args.seed, r["id"])):
            strata.setdefault((decade(r["page"]["date"]), band(r["page"].get("damage_rate"))), []).append(r)
        kept = [r for rs in strata.values() for r in rs[:args.per_stratum]]
    return sorted(kept, key=lambda r: r["id"])


def clip(page_img, box: list, pad_x: int, pad_y: int) -> tuple:
    x, y, w, h = box
    W, H = page_img.size
    c = (max(0, int(x - pad_x)), max(0, int(y - pad_y)), min(W, int(x + w + pad_x)), min(H, int(y + h + pad_y)))
    return page_img.crop(c), c


def write_task(r: dict, args, root: str, out_dir: str, images: dict, f) -> None:
    from PIL import Image
    page = r["page"]
    if page["image"] not in images:
        images.clear()  # one page at a time: the rows come sorted by id, so a page's words are together
        images[page["image"]] = Image.open(os.path.join(root, page["image"])).convert("L")
    img = images[page["image"]]
    x, y, w, h = r["word"]["box"]
    prefix = f"{args.lang}/{args.set}"
    stem = r["id"].split("/", 2)[2].replace("~", "_w")
    word_key, line_key = f"{prefix}/words/{stem}.jpg", f"{prefix}/lines/{stem}.jpg"
    for key in (word_key, line_key):
        os.makedirs(os.path.dirname(os.path.join(out_dir, "clips", key)), exist_ok=True)
    wimg, _ = clip(img, [x, y, w, h], max(4, h // 4), max(3, h // 6))
    wimg.save(os.path.join(out_dir, "clips", word_key), quality=args.quality)
    lx0, ly0, lx1, ly1 = r["word"]["line_box"]
    limg, (cx0, cy0, cx1, cy1) = clip(img, [lx0, ly0, lx1 - lx0, ly1 - ly0], max(6, int(h // 2)), max(4, int(h // 3)))
    if limg.size[0] > MAX_LINE_WIDTH:
        scale = MAX_LINE_WIDTH / limg.size[0]
        limg = limg.resize((MAX_LINE_WIDTH, max(1, round(limg.size[1] * scale))))
    limg.save(os.path.join(out_dir, "clips", line_key), quality=args.quality)
    cw, ch = cx1 - cx0, cy1 - cy0
    a_is = side(args.seed, r["id"])
    b_is = "loc" if a_is == "xiaomi" else "xiaomi"
    a, b = r[a_is], r[b_is]
    ad, bd = bt.letter_diff(a, b, {})
    private = {"a_is": a_is, "b_is": b_is, "crop": r["crop"],
               "strata": {"decade": decade(page["date"]), "damage_band": band(page.get("damage_rate"))}}
    if "truth" in r:
        private.update(truth=r["truth"], truth_word=r["truth_word"], closer=r["closer"])
    f.write(json.dumps({
        "id": r["id"], "lang": args.lang, "set": args.set, "page": page["page"], "source": page.get("source"),
        "label": page.get("label"), "line": line_key, "word": word_key,
        "box": [round((x - cx0) / cw, 4), round((y - cy0) / ch, 4), round(w / cw, 4), round(h / ch, 4)],
        "a": a, "b": b, "a_diff": ad, "b_diff": bd, "private": private,
    }, ensure_ascii=False) + "\n")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--pages", required=True)
    ap.add_argument("--root", help="folder the pages' paths are relative to (default: the folder of --pages)")
    ap.add_argument("--lang", default="en")
    ap.add_argument("--set", required=True)
    ap.add_argument("--language-config", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--seed", default="1")
    ap.add_argument("--per-page", type=int, default=8)
    ap.add_argument("--per-stratum", type=int, default=0, help="cap per decade and damage band (0: no cap)")
    ap.add_argument("--min-agree", type=float, default=0.5, help="a crop whose readings agree on less of its words is left out")
    ap.add_argument("--min-similarity", type=float, default=0.5)
    ap.add_argument("--max-block", type=int, default=3)
    ap.add_argument("--keep-punctuation-only", dest="skip_punctuation_only", action="store_false")
    ap.add_argument("--no-clips", action="store_true", help="count only: write the report and no tasks or images")
    ap.add_argument("--quality", type=int, default=85)
    args = ap.parse_args()

    root = args.root or os.path.dirname(os.path.abspath(args.pages))
    stats = Counter()
    rows = []
    pages = read_jsonl(args.pages)
    for page in pages:
        stats["pages"] += 1
        rows.extend(candidates(page, args, root, stats))
    chosen = sample(rows, args)
    stats["sampled"] = len(chosen)
    os.makedirs(args.out_dir, exist_ok=True)
    if not args.no_clips:
        images = {}
        with open(os.path.join(args.out_dir, "tasks.jsonl"), "w", encoding="utf-8") as f:
            for r in chosen:
                write_task(r, args, root, args.out_dir, images, f)
        cfg = json.load(open(args.language_config, encoding="utf-8"))
        language = {k: v for k, v in cfg.items() if k not in bt.BUILD_ONLY}
        language["examples"] = {}
        with open(os.path.join(args.out_dir, "language.json"), "w", encoding="utf-8") as f:
            json.dump(language, f, ensure_ascii=False, indent=1)
    per_page = Counter(r["page"]["page"] for r in rows)
    counts = sorted(per_page.get(p["page"], 0) for p in pages)
    report = dict(sorted(stats.items()))
    report["differing_words_per_page"] = {
        "pages": len(counts), "min": counts[0] if counts else 0, "median": counts[len(counts) // 2] if counts else 0,
        "mean": round(sum(counts) / len(counts), 2) if counts else 0, "max": counts[-1] if counts else 0}
    left_out = sum(v for k, v in stats.items() if k.startswith("left_out_") and k in (
        "left_out_at_crop_edge", "left_out_split_joined_or_dropped", "left_out_long_block", "left_out_dissimilar"))
    report["alignment_skip_rate"] = round(left_out / stats["differing_loc_words"], 4) if stats["differing_loc_words"] else None
    with open(os.path.join(args.out_dir, "report.json"), "w", encoding="utf-8") as f:
        json.dump(report, f, indent=1)
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
