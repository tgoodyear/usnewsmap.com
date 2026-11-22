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
    w, h = 760, 330
    pw, gap, x0, y0, y1 = 150, 24, 56, 50, 250
    b = []
    for i, (name, n, a, c) in enumerate(CER):
        px = x0 + i * (pw + gap)
        top = max(a, c) * 1.25
        b.append(f"<line x1='{px}' y1='{y1}' x2='{px + pw}' y2='{y1}' stroke='{LINE}'/>")
        for j, (v, col, lab) in enumerate(((a, OLD, "5.5.0"), (c, NEW, "5.5.4"))):
            bh = (y1 - y0) * v / top
            bx = px + 18 + j * 60
            b.append(f"<rect x='{bx}' y='{y1 - bh:.1f}' width='46' height='{bh:.1f}' fill='{col}'/>")
            b.append(text(bx + 23, y1 - bh - 6, f"{v:.2f}%", 12, "middle"))
            b.append(text(bx + 23, y1 + 16, lab, 11, "middle", MUTED))
        d = c - a
        b.append(text(px + pw / 2, 24, f"{d:+.2f} points", 13, "middle", GREEN if d < 0 else NEW if d > 0.1 else MUTED, "bold"))
        b.append(text(px + pw / 2, y1 + 40, name, 11, "middle"))
        b.append(text(px + pw / 2, y1 + 55, f"{n} reads", 11, "middle", MUTED))
    b.append(text(10, 14, "CER (share of characters wrong), lower is better. Each panel has its own scale.", 11, color=MUTED))
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
    w, h = 760, 380
    x0, x1, y0 = 170, 720, 40
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
        b.append(f"<circle cx='{x0 + k * 120}' cy='{h - 14}' r='5' fill='{c}'/>")
        b.append(text(x0 + k * 120 + 10, h - 10, p, 11))
    return svg(w, h, "".join(b))


for name, fn in (("cer-by-set", cer), ("cpu-ratio", timing)):
    with open(f"{name}.svg", "w") as f:
        f.write(fn())
