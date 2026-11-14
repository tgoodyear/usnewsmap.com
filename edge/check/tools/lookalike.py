"""Find letters of two readings that the page draws almost the same (#1939).

check.usnewsmap.com asks a person to pick the reading whose letters match the print. When two readings differ only
in letters that the font draws alike (Ꭱ and Ꮢ in a serif Cherokee font), the person has nothing to go on. This module
renders each pair of letters that differ at the same position between two readings, in the font the page asks for
first for that script, at the size the page shows them, and flags the pair when the two bitmaps nearly coincide.

The measure is intersection over union of the inked pixels (grey level above 127) of the two glyphs drawn on one
baseline and origin, the best of the shifts of up to one pixel. The page draws a word at 2rem (style.css, `.script`),
so letters are drawn at PAGE_PX. THRESHOLD was set from the table in test_lookalike.py: the bundled font's closest
pair of Cherokee letters scores below it and the pair a macOS fallback font draws as one R scores above it.

Fonts. Cherokee comes from the font the site serves (public/fonts). The site serves no font for other scripts, so for
them the result only describes a reference font: the first family of the page's `--font-script` stack that can draw the
script and is installed here (Hebrew: Noto Serif Hebrew, then Times New Roman; others: Times New Roman, then Noto
Sans), or a file given with --font SCRIPT=PATH. A visitor whose machine has a different font sees different shapes.
Letters are drawn with Pillow's basic layout engine, which is enough for a letter of one character (Cherokee,
Cyrillic, bare Hebrew letters), so those numbers don't depend on whether Raqm is installed. A letter with a combining
mark (פּ, אַ) needs shaping: the basic engine sets the mark beside the letter instead of on it, and a pair measured that
way says nothing about what the browser draws. Such a pair is drawn with the Raqm layout engine (HarfBuzz, what a browser
uses), and counted in `unshaped`, not measured, when this Pillow was built without Raqm.

Command line: measure the letter pairs of an export (`GET /api/export?lang=...`) or a tasks.jsonl, with no rebuild.

  python3 lookalike.py --export check-chr.jsonl [--export ...] [--font SCRIPT=PATH ...] [--threshold 0.82] [--ids]
"""
import argparse
import collections
import difflib
import json
import os
import re
import shutil
import subprocess
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_tasks as bt  # noqa: E402

PAGE_PX = 32  # 2rem at the browser's default 16px: `.script` in public/style.css
THRESHOLD = 0.82
MAX_SHIFT = 1
HERE = os.path.dirname(os.path.abspath(__file__))
BUNDLED = {"CHEROKEE": os.path.join(HERE, "..", "public", "fonts", "NotoSansCherokee-Regular.woff2")}
# The families of style.css's `--font-script`, after the served Cherokee font, that can draw each script.
STACKS = {"HEBREW": ["Noto Serif Hebrew", "Times New Roman"], "": ["Times New Roman", "Noto Sans"]}


SCRIPTS = ("CHEROKEE", "HEBREW", "CYRILLIC", "LATIN", "GREEK", "ARABIC")


def script_of(letter: str) -> str:
    """CHEROKEE, HEBREW, CYRILLIC... from the base character's Unicode name; "" for digits, punctuation and the rest."""
    for ch in letter:
        if not unicodedata.category(ch).startswith("M"):
            word = unicodedata.name(ch, "").split(" ")[0]
            return word if word in SCRIPTS else ""
    return ""


def label(letter: str) -> str:
    """The plain name shown under a reading: the syllable for a Cherokee letter ("CHEROKEE LETTER SV" -> "sv"),
    otherwise the Unicode character names of its characters, lower case, joined by " + "."""
    names = [unicodedata.name(ch, "") or f"U+{ord(ch):04X}" for ch in letter]
    m = re.fullmatch(r"CHEROKEE LETTER ([A-Z]+)", names[0]) if len(names) == 1 else None
    return m.group(1).lower() if m else " + ".join(names).lower()


def installed_font(family: str):
    """Path of an installed font whose family is exactly `family`, or None (fontconfig's fc-match)."""
    exe = shutil.which("fc-match")
    if not exe:
        return None
    try:
        out = subprocess.run([exe, "-f", "%{family}\t%{file}", family], capture_output=True, text=True, timeout=20).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    fam, _, path = out.partition("\t")
    return path if path and family.lower() in [f.strip().lower() for f in fam.split(",")] else None


class Fonts:
    """The reference font per script, and glyph bitmaps drawn with them."""

    def __init__(self, overrides: dict = None):
        from PIL import ImageFont
        self._font_cls = ImageFont
        self.paths = {}  # script -> path or None
        self.overrides = {k.upper(): v for k, v in (overrides or {}).items()}
        self._fonts, self._bitmaps = {}, {}
        self.unmeasured = collections.Counter()  # script -> pairs with no font or no glyph
        self.unshaped = collections.Counter()  # script -> pairs with a combining mark, not measured: no Raqm to shape them
        from PIL import features
        self.raqm = bool(features.check("raqm"))

    def path(self, script: str):
        if script not in self.paths:
            if script in self.overrides:
                self.paths[script] = self.overrides[script]
            elif script in BUNDLED:
                self.paths[script] = BUNDLED[script]
            else:
                found = (installed_font(f) for f in STACKS.get(script, STACKS[""]))
                self.paths[script] = next((p for p in found if p), None)
        return self.paths[script]

    def _font(self, path, shaped=False):
        if (path, shaped) not in self._fonts:
            layout = self._font_cls.Layout.RAQM if shaped else self._font_cls.Layout.BASIC
            try:
                self._fonts[(path, shaped)] = self._font_cls.truetype(path, PAGE_PX, layout_engine=layout)
            except OSError:
                if not path.endswith(".woff2"):
                    raise
                # a FreeType built without Brotli can't read woff2: unwrap it with fontTools, when that is installed
                try:
                    import io
                    from fontTools.ttLib import TTFont
                except ImportError:
                    raise OSError(f"this Pillow can't read {path} (woff2 needs a FreeType built with Brotli); install fonttools and brotli") from None
                font = TTFont(path)
                font.flavor = None
                buf = io.BytesIO()
                font.save(buf)
                buf.seek(0)
                self._fonts[(path, shaped)] = self._font_cls.truetype(buf, PAGE_PX, layout_engine=layout)
        return self._fonts[(path, shaped)]

    def bitmap(self, path: str, text: str, shaped: bool = False, direction: str = None):
        """Black and white picture of `text` at PAGE_PX on a fixed baseline, or None when the font has no glyph.
        `shaped`: lay it out with Raqm (needs self.raqm), so combining marks sit on their letter."""
        key = (path, text, shaped, direction)
        if key not in self._bitmaps:
            self._bitmaps[key] = self._draw(self._font(path, shaped), text, direction if shaped else None)
        return self._bitmaps[key]

    @staticmethod
    def _draw(font, text, direction=None):
        from PIL import Image, ImageDraw
        def render(s):
            im = Image.new("L", (2 * PAGE_PX, 2 * PAGE_PX), 0)
            ImageDraw.Draw(im).text((PAGE_PX // 2, int(PAGE_PX * 1.25)), s, font=font, fill=255, anchor="ls", direction=direction)
            return im.point(lambda v: 255 if v > 127 else 0)
        im = render(text)
        if im.getbbox() is None or im.tobytes() == render("\U0010fffd").tobytes():  # blank, or the font's missing-glyph box
            return None
        return im


def iou(a, b, shift: int = MAX_SHIFT) -> float:
    """Intersection over union of two black and white pictures, the best over shifts of up to `shift` pixels."""
    from PIL import ImageChops
    best = 0.0
    for dx in range(-shift, shift + 1):
        for dy in range(-shift, shift + 1):
            moved = ImageChops.offset(b, dx, dy)
            union = ImageChops.lighter(a, moved).histogram()[255]
            best = max(best, ImageChops.darker(a, moved).histogram()[255] / union if union else 1.0)
    return best


def has_mark(letter: str) -> bool:
    return any(unicodedata.category(ch).startswith("M") for ch in letter)


def measure(fonts: Fonts, x: str, y: str):
    """Overlap of two letters (each a base character with its marks, as the page draws it) in the reference font of
    the first one's script (then the second's), or None when neither font has both, or when a letter has combining marks
    and this Pillow can't shape them. Both letters of a pair are drawn the same way, with shaping when either has a mark."""
    shaped = has_mark(x) or has_mark(y)
    if shaped and not fonts.raqm:
        fonts.unshaped[script_of(x)] += 1
        return None
    for script in dict.fromkeys((script_of(x), script_of(y))):
        path = fonts.path(script)
        if not path:
            continue
        direction = "rtl" if script == "HEBREW" else None
        bx, by = fonts.bitmap(path, x, shaped, direction), fonts.bitmap(path, y, shaped, direction)
        if bx is not None and by is not None:
            return iou(bx, by)
    fonts.unmeasured[script_of(x)] += 1
    return None


def differing_pairs(a: str, b: str, equivalents: dict) -> list:
    """(index in a, index in b, letter of a, letter of b) for letters that differ at the same position. Readings of the
    same length (after folding) are compared position by position, so a transposition (ᎡᏒ and ᏒᎡ) gives two pairs. Otherwise
    the differing stretches of equal length that an alignment finds are paired in order; a letter one reading has and the
    other lacks is not a pair. A letter is as `build_tasks.graphemes` counts it; spellings the language lists as
    equivalents are folded first, as the task diff does."""
    ua, ub = bt.units(a, equivalents), bt.units(b, equivalents)
    if len(ua) == len(ub):
        return [(ua[k][1], ub[k][1], ua[k][0], ub[k][0]) for k in range(len(ua)) if ua[k][0] != ub[k][0]]
    sm = difflib.SequenceMatcher(None, [u for u, _ in ua], [u for u, _ in ub], autojunk=False)
    out = []
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "replace" and i2 - i1 == j2 - j1:
            out.extend((ua[i1 + k][1], ub[j1 + k][1], ua[i1 + k][0], ub[j1 + k][0]) for k in range(i2 - i1))
    return out


def flags(a: str, b: str, equivalents: dict, fonts: Fonts, threshold: float = THRESHOLD) -> list:
    """The letter pairs of the two readings the page draws almost alike: dicts with `a` and `b` (letter indexes, as
    a_diff and b_diff), `a_label`, `b_label` and `iou`. Keys are in ascending order."""
    out = []
    for ia, ib, x, y in differing_pairs(a, b, equivalents):
        score = measure(fonts, x, y)
        if score is not None and score >= threshold:
            out.append({"a": ia, "a_label": label(x), "b": ib, "b_label": label(y), "iou": round(score, 3)})
    return out


def parse_fonts(items: list) -> dict:
    out = {}
    for item in items or []:
        script, _, path = item.partition("=")
        if not script or not path:
            raise SystemExit(f"--font wants SCRIPT=PATH (for example Cherokee=font.ttf), got {item!r}")
        out[script.upper()] = path
    return out


def language_equivalents(lang: str, languages_dir: str) -> dict:
    path = os.path.join(languages_dir, f"{lang}.json")
    if not os.path.exists(path):
        return {}
    cfg = json.load(open(path, encoding="utf-8"))
    return bt.parse_equivalents([f"{k}={v}" for k, v in cfg.get("equivalents", {}).items()])


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--export", action="append", required=True, help="an export (`/api/export?lang=...`) or tasks.jsonl; repeatable")
    ap.add_argument("--font", action="append", help="SCRIPT=PATH: the reference font for a script (default: the bundled "
                    "Cherokee font, installed fonts for the rest); repeatable")
    ap.add_argument("--threshold", type=float, default=THRESHOLD, help="flag pairs whose overlap is at least this (0-1)")
    ap.add_argument("--languages-dir", default=os.path.join(HERE, "..", "languages"), help="where <lang>.json (equivalents) are")
    ap.add_argument("--ids", action="store_true", help="also print each affected task id")
    ap.add_argument("--json", action="store_true", help="print the result as JSON")
    a = ap.parse_args()

    fonts = Fonts(parse_fonts(a.font))
    cache = {}
    result = {}
    for path in a.export:
        for raw in open(path, encoding="utf-8"):
            if not raw.strip():
                continue
            t = json.loads(raw)
            lang = t["id"].split("/")[0]
            eq = cache.setdefault(lang, language_equivalents(lang, a.languages_dir))
            r = result.setdefault(lang, {"tasks": 0, "tasks_flagged": 0, "pairs": collections.Counter(), "ids": []})
            r["tasks"] += 1
            found = flags(t["a"], t["b"], eq, fonts, a.threshold)
            if found:
                r["tasks_flagged"] += 1
                r["ids"].append(t["id"])
                for f in found:
                    x = bt.graphemes(t["a"])[f["a"]]
                    y = bt.graphemes(t["b"])[f["b"]]
                    r["pairs"][f"{x}/{y}"] += 1
    report = {
        "fonts": {s: p for s, p in sorted(fonts.paths.items())},
        "threshold": a.threshold,
        "unmeasured_pairs": dict(fonts.unmeasured),
        "unshaped_pairs": dict(fonts.unshaped),
        "languages": {l: {"tasks": r["tasks"], "tasks_flagged": r["tasks_flagged"], "flagged_pairs": sum(r["pairs"].values()),
                          "pairs": dict(r["pairs"].most_common()), **({"ids": r["ids"]} if a.ids else {})}
                      for l, r in sorted(result.items())},
    }
    if a.json:
        print(json.dumps(report, ensure_ascii=False, indent=1))
        return
    print(f"threshold {a.threshold}; fonts: " + ", ".join(f"{s or 'other'}={os.path.basename(p) if p else 'none found'}" for s, p in report["fonts"].items()))
    if fonts.unmeasured:
        print("pairs not measured (no font or no glyph): " + ", ".join(f"{s or 'other'} {n}" for s, n in fonts.unmeasured.items()))
    if fonts.unshaped:
        print("pairs not measured (a combining mark, and this Pillow has no Raqm to shape it): " + ", ".join(f"{s or 'other'} {n}" for s, n in fonts.unshaped.items()))
    for lang, r in report["languages"].items():
        print(f"{lang}: {r['tasks_flagged']} of {r['tasks']} tasks flagged, {r['flagged_pairs']} letter pairs")
        for pair, n in r["pairs"].items():
            print(f"  {pair} {n}")
        for i in r.get("ids", []):
            print(f"  {i}")


if __name__ == "__main__":
    main()
