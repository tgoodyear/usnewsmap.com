"""Turn printed lines with two OCR readings into check.usnewsmap.com tasks, one per word where they differ.

Input (--lines) is JSON lines, one printed line each:

  {"id": "<line id>", "image": "<line image, relative to --images>", "page": "<lccn>/<date>/ed-N/seq-N",
   "source": "<page URL>", "label": "<shown above the line>",
   "a": "<reading a>", "a_words": [{"text": "...", "box": [x, y, w, h]}, ...], "b": "<reading b>",
   "a_is": "<optional: what reading a is on this line>", "b_is": "<optional>"}

`a_words` are reading a's words in reading order with their boxes in the line image's pixels, as OCR
layout files (ALTO, hOCR) give them. Reading b only needs its text: its words are paired with a's by
aligning the two word sequences, so each task uses a's box. Paired words that differ become tasks; a word
with no partner at least --min-similarity alike (one reading split, joined, dropped or garbled it) is left
out, and so is a word with no letter or digit in either reading, or a pair of one- or two-letter readings
with no letter in common (specks, picture texture).

A line row with `"merged_line": true` is left out and counted: the line tools (phoenix_lines.py, yiddish_lines.py,
serbian_lines.py) already drop merged lines (merged_lines.py, #1978), so this only guards a hand-made lines file.

A line's own `a_is` and `b_is` replace --a-is and --b-is, for sets whose readings come from different sources.

A word clip is the line image's full height. With --clip-band (the Cherokee sets, #2122's rule in line_clip.py) a clip whose
line image reaches into the printed line above or below is cut to the word's own line, taken from the line's word boxes, when
that makes it more than --clip-shrink shorter; those tasks get new word clip keys (--clip-suffix), since served clips are
cached as immutable. Ids, readings and line clips are unchanged, and the summary counts the cut tasks.

Look-alike letters (lookalike.py): for each pair of letters that differ at the same position, both are drawn in the font
the page asks for first and the pair is recorded in the task's `lookalikes` when the pictures nearly coincide. Cherokee
uses the font the site serves; for a script the site serves no font for, the installed reference font is used, or one given
with --font SCRIPT=PATH, and pairs with no font or glyph are counted, not guessed. The page labels the letters of a flagged pair.
A pair of the same base letter that differs only in combining marks is not measured: it is recorded in `mark_only`, and the page
names the marks (the language configuration's `marks`).

Writes OUT_DIR/clips/<lang>/<set>/{lines,words}/..., OUT_DIR/tasks.jsonl and OUT_DIR/language.json (the
--language-config without its build-only fields; example letters only from --examples). Needs Pillow.

  python3 build_tasks.py --lines lines.jsonl --images DIR --lang yi --set loc-yidj2-v1 \
    --a-is loc --b-is yidj2 --language-config languages/yi.json --out-dir OUT \
    [--equivalent װ=וו ...] [--min-similarity 0.34] [--skip-punctuation-only] [--examples examples.json] \
    [--font Hebrew=font.ttf ...] [--lookalike-threshold 0.82] [--clip-band]
"""
import argparse
import difflib
import json
import math
import os
import re
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # `python3 -I` leaves the script directory off the path
import line_clip  # noqa: E402
BUILD_ONLY = ("drop_examples", "glyph_font", "glyph_ligatures", "glyphs", "min_examples")
BIDI_CONTROLS = dict.fromkeys(map(ord, "\u200e\u200f\u061c\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069"))


# Single-character fractions (½, ¼, ⅞ and the rest of Unicode's vulgar fractions) and the fraction slash, as the same
# text typed with digits and "/": a volunteer on a US keyboard types 1/2 where the print has ½, and OCR writes either.
# Only these characters are folded (not NFKC on the whole word, which would also fold ſ, ligatures and superscripts).
FRACTION_FOLD = {ch: unicodedata.normalize("NFKC", ch).replace("\u2044", "/")
                 for ch in map(chr, [*range(0xBC, 0xBF), *range(0x2150, 0x2160), 0x2189])}
FRACTION_FOLD["\u2044"] = "/"


def fold_fractions(text: str) -> str:
    """The text with each single-character fraction written as digits and "/" (½ is 1/2, 103½ is 1031/2)."""
    return "".join(FRACTION_FOLD.get(ch, ch) for ch in text)


def clean(text: str) -> str:
    """NFC, without bidi control characters (some OCR puts a right-to-left mark after a word)."""
    return unicodedata.normalize("NFC", text.translate(BIDI_CONTROLS)).strip()


def graphemes(text: str) -> list:
    """Letters as the page highlights them: a base character with the marks that follow it (e.g. פּ, אַ)."""
    out = []
    for ch in clean(text):
        if out and unicodedata.category(ch).startswith("M"):  # as app.js: \p{M} joins the letter before
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
        pairs = [(i1 + i, j1 + j) for i, j in best_pairs(a_words[i1:i2], b_words[j1:j2], equivalents, min_similarity)]
        # only words in this unmatched span count as neighbours: a word that matched exactly isn't part of a split
        pairs = [(i, j) for i, j in pairs if not split_or_joined(fa[i1:i2], fb[j1:j2], i - i1, j - j1)]
        left += (i2 - i1) - len(pairs)
        out.extend((i, b_words[j]) for i, j in pairs if fa[i] != fb[j])
    return out, left


def split_or_joined(fa: list, fb: list, i: int, j: int) -> bool:
    """True when a's word i matches b's word j joined with a neighbour better than b's word j alone, or
    the other way round: then the two readings cover different stretches of print and the pair would
    compare a whole word with part of it."""
    def ratio(x, y):
        return difflib.SequenceMatcher(None, x, y, autojunk=False).ratio()
    alone = ratio(fa[i], fb[j])
    joined_b = [fb[k] + fb[k + 1] for k in (j - 1, j) if 0 <= k and k + 1 < len(fb)]
    joined_a = [fa[k] + fa[k + 1] for k in (i - 1, i) if 0 <= k and k + 1 < len(fa)]
    return any(ratio(fa[i], x) > alone for x in joined_b) or any(ratio(x, fb[j]) > alone for x in joined_a)


# An @ that touches a letter ("@ourse" for "course") is an OCR error: print has @ only between quantities and prices
# ("10 bbls @ $4.50", "6@7c"), never inside a word. A reading with one is plainly wrong, so it is never offered as a
# reading (build_tasks.py, en_pick.py) and loses in scoring (en_pick_analysis.py). A bare @ or one by digits is left alone.
STRAY_AT = re.compile(r"[^\W\d_]@|@[^\W\d_]")


def stray_at(word: str) -> bool:
    return bool(STRAY_AT.search(word))


def has_letter(word: str) -> bool:
    return any(c.isalnum() for c in word)


def letters_only(word: str, equivalents: dict) -> str:
    """The word's letters and digits, folded: what's left once punctuation is taken out."""
    return "".join(u for u, _ in units(word, equivalents) if any(c.isalnum() for c in u))


def stray_marks(a: str, b: str, equivalents: dict) -> bool:
    """Both readings are one or two letters with none in common: usually specks or picture texture."""
    ua, ub = {u for u, _ in units(a, equivalents)}, {u for u, _ in units(b, equivalents)}
    return len(graphemes(a)) <= 2 and len(graphemes(b)) <= 2 and not ua & ub


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
    ap.add_argument("--examples", help="JSON {letter: [clip key, ...]} for language.json's `examples`: pictures already on the "
                    "site (for example the ones /api/languages/chr serves), so a new set shows the same letter pictures")
    ap.add_argument("--quality", type=int, default=85, help="JPEG quality of the clips")
    ap.add_argument("--font", action="append", help="SCRIPT=PATH: reference font for the look-alike check of a script "
                    "(default: the site's Cherokee font, installed fonts for other scripts); repeatable")
    ap.add_argument("--lookalike-threshold", type=float, default=None, help="flag letter pairs whose pictures overlap at least this "
                    "much (0-1; default: lookalike.THRESHOLD)")
    ap.add_argument("--skip-punctuation-only", action="store_true",
                    help="leave out pairs whose letters and digits are the same (they differ only in punctuation)")
    ap.add_argument("--clip-band", action="store_true", help="cut a word clip that reaches into another printed line to its "
                    "line (line_clip.py) when that makes it more than --clip-shrink shorter; such tasks get new word clip keys")
    ap.add_argument("--clip-shrink", type=float, default=line_clip.SHRINK)
    ap.add_argument("--clip-suffix", default="_b", help="added to the word clip key of a cut task")
    a = ap.parse_args()

    from PIL import Image  # only the clip cutting and the look-alike check need Pillow
    import lookalike

    fonts = lookalike.Fonts(lookalike.parse_fonts(a.font))
    threshold = lookalike.THRESHOLD if a.lookalike_threshold is None else a.lookalike_threshold
    flagged = marked = 0

    cfg = json.load(open(a.language_config, encoding="utf-8"))
    equivalents = parse_equivalents([f"{k}={v}" for k, v in cfg.get("equivalents", {}).items()] + (a.equivalent or []))
    root = os.path.join(a.out_dir, "clips")
    prefix = f"{a.lang}/{a.set}"
    os.makedirs(os.path.join(root, prefix, "lines"), exist_ok=True)
    os.makedirs(os.path.join(root, prefix, "words"), exist_ok=True)

    lines = tasks = unpaired = merged = clipped = stray = 0
    with open(a.lines, encoding="utf-8") as src, open(os.path.join(a.out_dir, "tasks.jsonl"), "w", encoding="utf-8") as out:
        for raw in src:
            if not raw.strip():
                continue
            ln = json.loads(raw)
            if ln.get("merged_line"):
                merged += 1
                continue
            lines += 1
            a_words = [clean(w["text"]) for w in ln["a_words"]]
            b_words = clean(ln["b"]).split()
            if "words" in ln:
                # The line says which words differ (the Cherokee sets build a and b from one word list, changing
                # only those), so pair them by position: aligning would re-pair repeated words and shift the ids.
                if len(a_words) != len(b_words):
                    raise SystemExit(f"{ln['id']}: `words` needs readings of equal length")
                pairs = [(k, b_words[k]) for k in ln["words"] if a_words[k] != b_words[k]]
                left = 0
            else:
                pairs, left = pair_words(a_words, b_words, equivalents, a.min_similarity)
            kept = [(k, wb) for k, wb in pairs
                    if has_letter(a_words[k]) and has_letter(wb) and not stray_marks(a_words[k], wb, equivalents)
                    and not (stray_at(a_words[k]) or stray_at(wb))
                    and not (a.skip_punctuation_only
                             and letters_only(a_words[k], equivalents) == letters_only(wb, equivalents))]
            stray += sum(stray_at(a_words[k]) or stray_at(wb) for k, wb in pairs)
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
                x0, y0, x1, y1 = crop_box(wa["box"], max(4, h // 4), img.size)
                cut = False
                if a.clip_band:
                    band = line_clip.printed_band([w["box"] for w in ln["a_words"]], wa["box"])
                    top, bottom, cut = line_clip.clip_span(y0, y1, band, a.clip_shrink)
                    y0, y1 = (max(0, int(top)), min(H, math.ceil(bottom))) if cut else (y0, y1)
                clipped += cut
                word_key = f"{prefix}/words/{ln['id']}_w{k}{a.clip_suffix if cut else ''}.jpg"
                img.crop((x0, y0, x1, y1)).save(os.path.join(root, word_key), quality=a.quality)
                ad, bd = letter_diff(ta, wb, equivalents)
                alike, marks = lookalike.analyse(ta, wb, equivalents, fonts, threshold)
                flagged += bool(alike)
                marked += bool(marks)
                out.write(json.dumps({
                    "id": f"{prefix}/{word_id}", "lang": a.lang, "set": a.set,
                    "page": ln.get("page"), "source": ln.get("source"), "label": ln.get("label"),
                    "line": line_key, "word": word_key,
                    "box": [round(x / W, 4), round(y / H, 4), round(w / W, 4), round(h / H, 4)],
                    "a": ta, "b": wb,
                    "a_diff": ad, "b_diff": bd, "a_is": ln.get("a_is", a.a_is), "b_is": ln.get("b_is", a.b_is),
                    **({"lookalikes": alike} if alike else {}),
                    **({"mark_only": marks} if marks else {}),
                }, ensure_ascii=False) + "\n")
                tasks += 1

    language = {k: v for k, v in cfg.items() if k not in BUILD_ONLY}
    language["examples"] = json.load(open(a.examples, encoding="utf-8")) if a.examples else {}
    # The page sets these spellings with no gap inside, so it gets the mapping the comparison used,
    # --equivalent additions included.
    language.pop("equivalents", None)
    if equivalents:
        language["equivalents"] = {k: "".join(v) for k, v in sorted(equivalents.items())}
    with open(os.path.join(a.out_dir, "language.json"), "w", encoding="utf-8") as f:
        json.dump(language, f, ensure_ascii=False, indent=1)
    print(f"{lines} lines, {tasks} tasks, {unpaired} words of reading a left out (no partner alike enough, or no letters)")
    if stray:
        print(f"{stray} of them with an @ touching a letter in a reading (plainly wrong, never offered)")
    if merged:
        print(f"{merged} lines marked merged_line left out")
    if a.clip_band:
        print(f"{clipped} word clips cut to their printed line (new keys ending {a.clip_suffix})")
    print(f"{flagged} tasks with look-alike letters (overlap at least {threshold}), {marked} differing only in marks; reference fonts: "
          + ", ".join(f"{s or 'other'}={os.path.basename(p) if p else 'none found'}" for s, p in sorted(fonts.paths.items())))
    if fonts.unmeasured:
        print("letter pairs not checked (no font or no glyph): " + ", ".join(f"{s or 'other'} {n}" for s, n in sorted(fonts.unmeasured.items())))
    if lookalike.unshaped_warning(fonts):
        print(lookalike.unshaped_warning(fonts), file=sys.stderr)


if __name__ == "__main__":
    main()
