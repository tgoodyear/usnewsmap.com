"""Turn printed lines with two OCR readings into check.usnewsmap.com tasks, one per word where they differ.

Input (--lines) is JSON lines, one printed line each:

  {"id": "<line id>", "image": "<line image, relative to --images>", "page": "<lccn>/<date>/ed-N/seq-N",
   "source": "<page URL>", "label": "<shown above the line>",
   "a": "<reading a>", "a_words": [{"text": "...", "box": [x, y, w, h]}, ...], "b": "<reading b>"}

`a_words` are reading a's words in reading order with their boxes in the line image's pixels, as OCR
layout files (ALTO, hOCR) give them. Reading b only needs its text: its words are paired with a's by
aligning the two word sequences, so each task uses a's box. Paired words that differ become tasks; a word
with no partner at least --min-similarity alike (one reading split, joined, dropped or garbled it) is left
out, and so is a word with no letter or digit in either reading.

Writes OUT_DIR/clips/<lang>/<set>/{lines,words}/..., OUT_DIR/tasks.jsonl and OUT_DIR/language.json (the
--language-config without its build-only fields, and no example letters). Needs Pillow.

  python3 build_tasks.py --lines lines.jsonl --images DIR --lang yi --set loc-yidj2-v1 \
    --a-is loc --b-is yidj2 --language-config languages/yi.json --out-dir OUT \
    [--equivalent װ=וו ...] [--min-similarity 0.34]
"""
import argparse
import difflib
import json
import os
import unicodedata

BUILD_ONLY = ("drop_examples", "min_examples", "equivalents")
BIDI_CONTROLS = dict.fromkeys(map(ord, "\u200e\u200f\u061c\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069"))


def clean(text: str) -> str:
    """NFC, without bidi control characters (some OCR puts a right-to-left mark after a word)."""
    return unicodedata.normalize("NFC", text.translate(BIDI_CONTROLS)).strip()


def graphemes(text: str) -> list:
    """Letters as the page highlights them: a base character with the marks that follow it (e.g. פּ, אַ)."""
    out = []
    for ch in clean(text):
        if out and unicodedata.combining(ch):
            out[-1] += ch
        else:
            out.append(ch)
    return out


def units(text: str, equivalents: dict) -> list:
    """(folded unit, letter index) pairs. A letter listed in `equivalents` (a ligature such as װ) becomes the
    letters it prints the same as (וו), so the two spellings compare equal and neither is highlighted."""
    out = []
    for i, g in enumerate(graphemes(text)):
        for u in equivalents.get(g, [g]):
            out.append((u, i))
    return out


def fold(text: str, equivalents: dict) -> str:
    return "".join(u for u, _ in units(text, equivalents))


def letter_diff(a: str, b: str, equivalents: dict) -> tuple:
    """Indexes of the letters of a and of b that differ (letters as in `graphemes`)."""
    ua, ub = units(a, equivalents), units(b, equivalents)
    sm = difflib.SequenceMatcher(None, [u for u, _ in ua], [u for u, _ in ub], autojunk=False)
    da, db = set(), set()
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op != "equal":
            da.update(ua[k][1] for k in range(i1, i2))
            db.update(ub[k][1] for k in range(j1, j2))
    return sorted(da), sorted(db)


def similarity(a: str, b: str, equivalents: dict) -> float:
    return difflib.SequenceMatcher(None, fold(a, equivalents), fold(b, equivalents), autojunk=False).ratio()


def best_pairs(a_words: list, b_words: list, equivalents: dict, min_similarity: float) -> list:
    """Pairs (i, j), in order, that maximise the summed similarity, counting only pairs at least
    `min_similarity` alike: words one reading split, joined, dropped or added stay unpaired."""
    n, m = len(a_words), len(b_words)
    sim = [[similarity(x, y, equivalents) for y in b_words] for x in a_words]
    score = [[0.0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            take = sim[i][j] + score[i + 1][j + 1] if sim[i][j] >= min_similarity else -1.0
            score[i][j] = max(take, score[i + 1][j], score[i][j + 1])
    out, i, j = [], 0, 0
    while i < n and j < m:
        if sim[i][j] >= min_similarity and score[i][j] == sim[i][j] + score[i + 1][j + 1]:
            out.append((i, j))
            i, j = i + 1, j + 1
        elif score[i][j] == score[i + 1][j]:
            i += 1
        else:
            j += 1
    return out


def pair_words(a_words: list, b_words: list, equivalents: dict, min_similarity: float) -> tuple:
    """([(index in a, b's word)], left out): the words of a that pair with a word of b and differ from it,
    and how many other words of a differ but have no partner alike enough."""
    fa = [fold(w, equivalents) for w in a_words]
    fb = [fold(w, equivalents) for w in b_words]
    sm = difflib.SequenceMatcher(None, fa, fb, autojunk=False)
    out, left = [], 0
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "equal":
            continue
        pairs = best_pairs(a_words[i1:i2], b_words[j1:j2], equivalents, min_similarity)
        left += (i2 - i1) - len(pairs)
        out.extend((i1 + i, b_words[j1 + j]) for i, j in pairs if fa[i1 + i] != fb[j1 + j])
    return out, left


def has_letter(word: str) -> bool:
    return any(c.isalnum() for c in word)


def parse_equivalents(items: list) -> dict:
    out = {}
    for item in items or []:
        left, _, right = item.partition("=")
        if not left or not right:
            raise SystemExit(f"--equivalent wants LETTER=LETTERS, got {item!r}")
        (g,) = graphemes(left)
        out[g] = graphemes(right)
    return out


def crop_box(box: list, pad: int, size: tuple) -> tuple:
    x, y, w, h = box
    return (max(0, x - pad), 0, min(size[0], x + w + pad), size[1])


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--lines", required=True, help="JSON lines, one printed line each (see above)")
    ap.add_argument("--images", required=True, help="folder the lines' `image` paths are relative to")
    ap.add_argument("--lang", required=True)
    ap.add_argument("--set", required=True)
    ap.add_argument("--a-is", required=True, help="what reading a is, e.g. loc")
    ap.add_argument("--b-is", required=True, help="what reading b is, e.g. yidj2")
    ap.add_argument("--language-config", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--equivalent", action="append", help="LETTER=LETTERS that print alike, e.g. װ=וו; repeatable. "
                    "Added to the config's `equivalents`")
    ap.add_argument("--min-similarity", type=float, default=0.34, help="leave out pairs less alike than this (0-1)")
    ap.add_argument("--quality", type=int, default=85, help="JPEG quality of the clips")
    a = ap.parse_args()

    from PIL import Image  # only the clip cutting needs Pillow

    cfg = json.load(open(a.language_config, encoding="utf-8"))
    equivalents = parse_equivalents([f"{k}={v}" for k, v in cfg.get("equivalents", {}).items()] + (a.equivalent or []))
    root = os.path.join(a.out_dir, "clips")
    prefix = f"{a.lang}/{a.set}"
    os.makedirs(os.path.join(root, prefix, "lines"), exist_ok=True)
    os.makedirs(os.path.join(root, prefix, "words"), exist_ok=True)

    lines = tasks = unpaired = 0
    with open(a.lines, encoding="utf-8") as src, open(os.path.join(a.out_dir, "tasks.jsonl"), "w", encoding="utf-8") as out:
        for raw in src:
            if not raw.strip():
                continue
            ln = json.loads(raw)
            lines += 1
            a_words = [clean(w["text"]) for w in ln["a_words"]]
            b_words = clean(ln["b"]).split()
            pairs, left = pair_words(a_words, b_words, equivalents, a.min_similarity)
            kept = [(k, wb) for k, wb in pairs if has_letter(a_words[k]) and has_letter(wb)]
            unpaired += left + len(pairs) - len(kept)
            pairs = kept
            if not pairs:
                continue
            img = Image.open(os.path.join(a.images, ln["image"])).convert("L")
            W, H = img.size
            line_key = f"{prefix}/lines/{ln['id']}.jpg"
            img.save(os.path.join(root, line_key), quality=a.quality)
            for k, wb in pairs:
                wa = ln["a_words"][k]
                x, y, w, h = wa["box"]
                ta = a_words[k]
                word_id = f"{ln['id']}~{k}"
                word_key = f"{prefix}/words/{ln['id']}_w{k}.jpg"
                img.crop(crop_box(wa["box"], max(4, h // 4), img.size)).save(os.path.join(root, word_key), quality=a.quality)
                ad, bd = letter_diff(ta, wb, equivalents)
                out.write(json.dumps({
                    "id": f"{prefix}/{word_id}", "lang": a.lang, "set": a.set,
                    "page": ln.get("page"), "source": ln.get("source"), "label": ln.get("label"),
                    "line": line_key, "word": word_key,
                    "box": [round(x / W, 4), round(y / H, 4), round(w / W, 4), round(h / H, 4)],
                    "a": ta, "b": wb,
                    "a_diff": ad, "b_diff": bd, "a_is": a.a_is, "b_is": a.b_is,
                }, ensure_ascii=False) + "\n")
                tasks += 1

    language = {k: v for k, v in cfg.items() if k not in BUILD_ONLY}
    language.setdefault("examples", {})
    with open(os.path.join(a.out_dir, "language.json"), "w", encoding="utf-8") as f:
        json.dump(language, f, ensure_ascii=False, indent=1)
    print(f"{lines} lines, {tasks} tasks, {unpaired} words of reading a left out (no partner alike enough, or no letters)")


if __name__ == "__main__":
    main()
