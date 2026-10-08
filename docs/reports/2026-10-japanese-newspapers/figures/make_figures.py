#!/usr/bin/env python3
"""Draw the report's figures as SVG. Every number comes from issues #128 and #135,
docs/notes/2026-10-05-japanese-ocr.md, and the OCR quality audit's tables in
data/ocr-quality-v2-10pct.jsonl; run from this directory."""

import json
from datetime import date
from pathlib import Path

HERE = Path(__file__).resolve().parent
AUDIT = [json.loads(line) for line in (HERE.parent / "data" / "ocr-quality-v2-10pct.jsonl").open()]


def audit(table):
    return [r for r in AUDIT if r.get("table") == table]
FONT = "font-family='Helvetica Neue, Helvetica, Arial, Hiragino Sans, sans-serif'"
INK, MUTED, LINE = "#222", "#666", "#ccc"
LOC, OURS, HOJI, CHNC, NONE = "#5b7fb4", "#d9822b", "#4f7a35", "#6b4c94", "#bbbbbb"


def svg(w, h, body):
    return (f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w} {h}' width='{w}' height='{h}' {FONT}>"
            f"<rect width='{w}' height='{h}' fill='white'/>{body}</svg>\n")


def text(x, y, s, size=12, anchor="start", color=INK, weight="normal", halo=False):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    h = " stroke='white' stroke-width='4' paint-order='stroke'" if halo else ""
    return (f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' "
            f"font-weight='{weight}'{h}>{s}</text>")


def timeline():
    """Who holds which years of the two Denver papers, and with what text."""
    w, h = 760, 300
    x0, x1 = 210, 730
    lo, hi = date(1943, 1, 1), date(1946, 1, 1)

    def x(d):
        return x0 + (x1 - x0) * (d - lo).days / (hi - lo).days

    b = []
    for yr in range(1943, 1947):
        xx = x(date(yr, 1, 1))
        b.append(f"<line x1='{xx}' y1='14' x2='{xx}' y2='262' stroke='{LINE}'/>")
        b.append(text(xx, 280, str(yr), 11, "middle", MUTED))
    rows = [
        ("Rocky Shimpo", None),
        ("Hoji Shinbun", (date(1943, 4, 12), date(1944, 4, 12), HOJI, "Japanese OCR, 404 pages", False)),
        ("CHNC", (date(1943, 4, 12), date(1944, 4, 12), CHNC, "Japanese OCR, 101 issues", False)),
        ("LoC / ours", (date(1944, 6, 2), date(1945, 12, 31), OURS, "756 of 1,012 pages lack text at LoC; our OCR", False)),
        ("Colorado Times", None),
        ("CHNC", (date(1943, 1, 1), date(1945, 12, 31), CHNC, "Japanese OCR, 1918–1969 (axis cut)", False)),
        ("LoC / ours", (date(1945, 1, 1), date(1945, 12, 31), OURS, "1945: 634 of 726 pages lack text", False)),
    ]
    y = 22
    for label, bar in rows:
        if bar is None:
            y += 10 if y > 22 else 0
            b.append(text(10, y + 4, label, 13, weight="bold"))
            y += 22
            continue
        b.append(text(24, y + 14, label, 12))
        d0, d1, color, note, open_end = bar
        xa, xb = x(d0), x(d1)
        b.append(f"<rect x='{xa:.1f}' y='{y}' width='{xb - xa:.1f}' height='20' fill='{color}' rx='2'/>")
        if open_end:
            b.append(f"<rect x='{xb - 30:.1f}' y='{y}' width='30' height='20' fill='white' opacity='0.55'/>")
            b.append(text(xb + 4, y + 14, "?", 12, color=MUTED))
        if xb - xa > 7 * len(note):
            b.append(text(xa + 6, y + 14, note, 11, color="white"))
        elif xa - x0 > 5 * len(note):
            b.append(text(xa - 6, y + 14, note, 11, "end", halo=True))
        else:
            b.append(text(xb + (20 if open_end else 6), y + 14, note, 11, halo=True))
        y += 30
    return svg(w, h, "".join(b))


def missing():
    """Japanese pages without LoC text, by title (non-English audit, #135)."""
    rows = [("Topaz Times", 1249, 2994), ("Poston Chronicle", 1191, 3502), ("Granada Pioneer", 1069, 3272),
            ("Gila News-Courier", 1058, 3519), ("Heart Mountain Sentinel (Japanese ed.)", 1032, 1035),
            ("Rocky Shimpo", 756, 1012), ("Colorado Times", 634, 726), ("Manzanar Free Press", 633, 2640),
            ("Newell Star", 390, 957), ("Rohwer Outpost", 346, 2462), ("Denson Tribune", 344, 1194)]
    w, h = 760, 34 + 26 * len(rows) + 30
    x0, x1, top = 290, 640, 14
    scale = (x1 - x0) / 3600
    b = []
    for i, (name, miss, total) in enumerate(rows):
        y = top + i * 26
        b.append(text(x0 - 8, y + 13, name, 12, "end"))
        b.append(f"<rect x='{x0}' y='{y}' width='{total * scale:.1f}' height='18' fill='#e3e8f0'/>")
        b.append(f"<rect x='{x0}' y='{y}' width='{miss * scale:.1f}' height='18' fill='{LOC}'/>")
        b.append(text(x0 + total * scale + 6, y + 13, f"{miss:,} of {total:,}", 11, color=MUTED))
    y = top + len(rows) * 26 + 16
    b.append(f"<rect x='{x0}' y='{y - 10}' width='12' height='12' fill='{LOC}'/>")
    b.append(text(x0 + 18, y, "without text", 11, color=MUTED))
    b.append(f"<rect x='{x0 + 110}' y='{y - 10}' width='12' height='12' fill='#e3e8f0'/>")
    b.append(text(x0 + 128, y, "all pages LoC lists", 11, color=MUTED))
    return svg(w, h, "".join(b))


def engines():
    """Bigram F1 on 23 reference crops, typeset and camp (#128)."""
    rows = [("NDLOCR-Lite", 83, 72), ("NDNP-Open-OCR + NDLOCR-Lite (our prototype)", 74, 60),
            ("Azure Document Intelligence Read", 72, 49), ("Tesseract jpn_vert (crop)", 40, 11),
            ("NDNP-Open-OCR, jpn_vert", 37, 9), ("NDNP-Open-OCR as shipped (English)", 0, 0)]
    w, h = 760, 24 + 40 * len(rows) + 30
    x0, x1, top = 300, 700, 10
    sc = (x1 - x0) / 100
    b = []
    for i, (name, typ, camp) in enumerate(rows):
        y = top + i * 40
        b.append(text(x0 - 8, y + 17, name, 12, "end"))
        b.append(f"<rect x='{x0}' y='{y}' width='{typ * sc:.1f}' height='14' fill='{LOC}'/>")
        b.append(text(x0 + typ * sc + 5, y + 12, f"{typ}%", 10, color=MUTED))
        b.append(f"<rect x='{x0}' y='{y + 16}' width='{camp * sc:.1f}' height='14' fill='{OURS}'/>")
        b.append(text(x0 + camp * sc + 5, y + 28, f"{camp}%", 10, color=MUTED))
    y = top + len(rows) * 40 + 12
    b.append(f"<rect x='{x0}' y='{y - 10}' width='12' height='12' fill='{LOC}'/>")
    b.append(text(x0 + 18, y, "typeset (Denver papers)", 11, color=MUTED))
    b.append(f"<rect x='{x0 + 170}' y='{y - 10}' width='12' height='12' fill='{OURS}'/>")
    b.append(text(x0 + 188, y, "camp papers (hand-lettered mimeograph)", 11, color=MUTED))
    return svg(w, h, "".join(b))


def pipeline():
    """How a page without text becomes searchable."""
    steps = [("LoC page", "full-size IIIF image;", "empty ALTO, no ocr.txt"),
             ("NDLOCR-Lite", "OCR job on Azure,", "paced to loc.gov's limits"),
             ("Our output", "ALTO v3 in LoC's units,", "Parquet text per page"),
             ("Japanese index", "one character per token,", "old forms folded to modern"),
             ("Search", "a query in Japanese", "searches only this index")]
    w, h, bw, gap = 760, 120, 132, 15
    b = []
    for i, (title, a, c) in enumerate(steps):
        x = 10 + i * (bw + gap)
        b.append(f"<rect x='{x}' y='20' width='{bw}' height='80' rx='6' fill='#f4f6fa' stroke='{LOC}'/>")
        b.append(text(x + bw / 2, 44, title, 13, "middle", weight="bold"))
        b.append(text(x + bw / 2, 66, a, 10.5, "middle", MUTED))
        b.append(text(x + bw / 2, 82, c, 10.5, "middle", MUTED))
        if i < len(steps) - 1:
            ax = x + bw
            b.append(f"<path d='M{ax + 2} 60 L{ax + gap - 3} 60' stroke='{INK}' stroke-width='1.5'/>")
            b.append(f"<path d='M{ax + gap - 7} 56 L{ax + gap - 2} 60 L{ax + gap - 7} 64' fill='none' stroke='{INK}' stroke-width='1.5'/>")
    return svg(w, h, "".join(b))


NAMES = {"eng": "English", "ger": "German", "spa": "Spanish", "fre": "French", "pol": "Polish",
         "ita": "Italian", "dan": "Danish", "cze": "Czech", "nor": "Norwegian", "yid": "Yiddish",
         "fin": "Finnish", "swe": "Swedish", "hun": "Hungarian", "slv": "Slovenian"}


def agreement():
    """Sampled pages' detected language against the title's first catalog language."""
    summ = {r["scope"]: r for r in audit("agreement_summary")}
    rows = [("Single-language titles", summ["single_language_titles"]),
            ("Multilingual titles", summ["multilingual_titles"]), None]
    langs = sorted((r for k, r in summ.items() if k.startswith("title_language:") and r["text_pages"] >= 1500),
                   key=lambda r: -r["text_pages"])
    rows += [(f"{NAMES.get(r['scope'][15:], r['scope'][15:])} titles", r) for r in langs]
    parts = [("same_share", LOC, "same language"), ("other_language_share", OURS, "another language"),
             ("mixed_share", CHNC, "mixed"), ("und_share", NONE, "undetermined")]
    w, x0, x1, top = 760, 190, 570, 10
    h = top + 24 * len(rows) + 40
    b, y = [], top
    for row in rows:
        if row is None:
            y += 10
            continue
        label, r = row
        b.append(text(x0 - 8, y + 13, label, 12, "end"))
        x = x0
        for key, color, _ in parts:
            wd = (r[key] or 0) * (x1 - x0)
            b.append(f"<rect x='{x:.1f}' y='{y}' width='{wd:.1f}' height='18' fill='{color}'/>")
            x += wd
        b.append(text(x1 + 6, y + 13, f"{r['same_share']:.0%} same, {r['text_pages']:,} pages", 11, color=MUTED))
        y += 24
    y += 16
    x = x0
    for _, color, name in parts:
        b.append(f"<rect x='{x}' y='{y - 10}' width='12' height='12' fill='{color}'/>")
        b.append(text(x + 18, y, name, 11, color=MUTED))
        x += 26 + 6.5 * len(name)
    return svg(w, h, "".join(b))


def decades():
    """Median damage rate and share badly damaged, by decade, for English, German, Spanish and French."""
    series = [("eng", LOC), ("ger", OURS), ("spa", HOJI), ("fre", CHNC)]
    lo, hi = 1800, 1960
    w, h = 760, 270
    panels = [("damage_rate_median", "Median damage rate", 0.25, 70, 360),
              ("badly_damaged_share", "Pages badly damaged (rate over 0.25)", 0.35, 430, 720)]
    top, bottom = 30, 210
    b = []
    for key, title, ymax, x0, x1 in panels:
        def px(d):
            return x0 + (x1 - x0) * (d - lo) / (hi - lo)

        def py(v):
            return bottom - (bottom - top) * v / ymax
        b.append(text(x0, 16, title, 12, weight="bold"))
        for v in [i * 0.05 for i in range(int(ymax / 0.05) + 1)]:
            b.append(f"<line x1='{x0}' y1='{py(v):.1f}' x2='{x1}' y2='{py(v):.1f}' stroke='{LINE}' stroke-width='0.6'/>")
            b.append(text(x0 - 6, py(v) + 4, f"{v:.2f}", 10, "end", MUTED))
        for d in range(lo, hi + 1, 40):
            b.append(text(px(d), bottom + 16, f"{d}s", 10, "middle", MUTED))
        for lang, color in series:
            pts = sorted((r["decade"], r[key]) for r in audit("by_language_decade")
                         if r["language"] == lang and r["scored_pages"] >= 100 and lo <= r["decade"] <= hi)
            path = " ".join(f"{'M' if i == 0 else 'L'}{px(d):.1f} {py(v):.1f}" for i, (d, v) in enumerate(pts))
            b.append(f"<path d='{path}' fill='none' stroke='{color}' stroke-width='2'/>")
            b.extend(f"<circle cx='{px(d):.1f}' cy='{py(v):.1f}' r='2.2' fill='{color}'/>" for d, v in pts)
    x = 70
    for lang, color in series:
        b.append(f"<rect x='{x}' y='{h - 22}' width='12' height='12' fill='{color}'/>")
        b.append(text(x + 18, h - 12, NAMES[lang], 11, color=MUTED))
        x += 90
    b.append(text(x + 10, h - 12, "Decades with at least 100 scored pages in the sample.", 11, color=MUTED))
    return svg(w, h, "".join(b))


for name, fn in [("coverage-timeline", timeline), ("missing-text", missing), ("engines", engines),
                 ("pipeline", pipeline), ("language-agreement", agreement), ("damage-by-decade", decades)]:
    (HERE / f"{name}.svg").write_text(fn())
    print(f"wrote {name}.svg")
