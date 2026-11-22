#!/usr/bin/env python3
"""Draw the report's figures as SVG, standard library only. Every number comes
from the report itself: Figure 1 from the table in "Text and accuracy", Figure 2
from the first table in "Timing". Run from this directory."""

FONT = "font-family='Helvetica Neue, Helvetica, Arial, sans-serif'"
INK, MUTED, LINE = "#222", "#666", "#ccc"
OLD, NEW, GREEN, PURPLE = "#5b7fb4", "#d9822b", "#4f7a35", "#6b4c94"


def svg(w, h, body):
    return (f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w} {h}' width='{w}' height='{h}' {FONT}>"
            f"<rect width='{w}' height='{h}' fill='white'/>{body}</svg>\n")


def text(x, y, s, size=12, anchor="start", color=INK, weight="normal"):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    return (f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' "
            f"font-weight='{weight}'>{s}</text>")


# Table "Text and accuracy": (set, reads, CER 5.5.0 %, CER 5.5.4 %)
CER = [
    ("English challenge crops", 30, 7.16, 6.81),
    ("English pages (American Stories)", 6, 10.78, 12.13),
    ("German pages, frak2021", 13, 58.15, 58.14),
    ("German pages, deu_latf+deu", 8, 85.84, 85.91),
]


def cer():
    """Diverging bars: CER of 5.5.4 minus CER of 5.5.0, percentage points; positive = worse."""
    w, rh = 760, 62
    h = 90 + rh * len(CER)
    x0, x1, lo, hi = 330, 730, -1.0, 2.0
    top, bot = 50, 50 + rh * len(CER)

    def x(v):
        return x0 + (x1 - x0) * (v - lo) / (hi - lo)

    b = [text(10, 18, "Change in CER, 5.5.4 minus 5.5.0, percentage points", 12, color=MUTED, weight="bold")]
    for v in (-1, -0.5, 0, 0.5, 1, 1.5, 2):
        b.append(f"<line x1='{x(v):.1f}' y1='{top - 8}' x2='{x(v):.1f}' y2='{bot}' stroke='{INK if v == 0 else LINE}' stroke-width='{1.5 if v == 0 else 1}'/>")
        b.append(text(x(v), bot + 16, f"{v:+g}" if v else "0", 11, "middle", MUTED))
    b.append(text(x(0) - 6, top - 14, "← better", 11, "end", MUTED))
    b.append(text(x(0) + 6, top - 14, "worse →", 11, "start", MUTED))
    for i, (name, n, a, c) in enumerate(CER):
        y = top + i * rh
        d = round(c - a, 2)
        col = NEW if d > 0 else GREEN
        b.append(text(x0 - 14, y + 26, f"{name}, {n} reads", 12, "end"))
        b.append(text(x0 - 14, y + 43, f"{a:.2f}% → {c:.2f}%", 12, "end", MUTED))
        x_a, x_b = sorted((x(0), x(d)))
        b.append(f"<rect x='{x_a:.1f}' y='{y + 14}' width='{max(x_b - x_a, 1.5):.1f}' height='30' fill='{col}'/>")
        lab = f"{d:+.2f}"
        if d < -0.1:
            b.append(text(x_a - 6, y + 34, lab, 12, "end", weight="bold"))
        else:
            b.append(text(x_b + 6, y + 34, lab, 12, weight="bold"))
    return svg(w, h, "".join(b))


# First table in "Timing": (profile, page, 5.5.4 / 5.5.0 user CPU ratio)
TIME = [
    ("frak2021", "cg-008-frak", 0.62), ("eng", "asgold-006", 0.64), ("frak2021", "cg-011-frak", 0.64),
    ("eng", "carson-05", 0.68), ("frak2021", "cg-005-frak", 0.74), ("eng", "asgold-001", 0.83),
    ("frak2021", "cg-001-frak", 0.89), ("deu_latf+deu", "cg-001-latf", 0.90),
    ("frak2021", "cg-002-frak", 0.93), ("eng", "asgold-002", 0.95), ("deu_latf+deu", "cg-005-latf", 1.05),
]
COL = {"eng": OLD, "frak2021": NEW, "deu_latf+deu": PURPLE}


def timing():
    w, h = 760, 400
    x0, x1, y0 = 170, 720, 62
    lo, hi = 0.5, 1.1

    def x(v):
        return x0 + (x1 - x0) * (v - lo) / (hi - lo)

    b = []
    for v in (0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1):
        b.append(f"<line x1='{x(v):.1f}' y1='{y0 - 8}' x2='{x(v):.1f}' y2='{y0 + 11 * 26}' stroke='{'#888' if v == 1.0 else LINE}'/>")
        b.append(text(x(v), y0 + 11 * 26 + 16, f"{v:.1f}", 11, "middle", MUTED))
    for i, (prof, page, r) in enumerate(TIME):
        y = y0 + i * 26 + 10
        b.append(text(x0 - 12, y + 4, page, 12, "end"))
        b.append(f"<circle cx='{x(r):.1f}' cy='{y}' r='6' fill='{COL[prof]}'/>")
        b.append(text(x(r) + 11, y + 4, f"{r:.2f}", 11, color=MUTED))
    b.append(text((x0 + x1) / 2, y0 + 11 * 26 + 38, "5.5.4 CPU time divided by 5.5.0 CPU time (median of 3 runs); below 1.0 means 5.5.4 is faster", 11, "middle", MUTED))
    b.append(text(x(1.0) + 4, y0 - 14, "no change", 11, color=MUTED))
    for k, (p, c) in enumerate(COL.items()):
        b.append(f"<circle cx='{x0 + k * 120}' cy='{y0 - 22}' r='5' fill='{c}'/>")
        b.append(text(x0 + k * 120 + 10, y0 - 18, p, 11))
    return svg(w, h, "".join(b))


for name, fn in (("cer-by-set", cer), ("cpu-ratio", timing)):
    with open(f"{name}.svg", "w") as f:
        f.write(fn())
