#!/usr/bin/env python3
"""Draw the figure for qwen-per-crop-settlement.md, standard library only. The
numbers come from that document's "Problem" section (the 14 Nov table of the
three failed units) and its "Pair threshold" section (the 80% floor). Run from
this directory."""

FONT = "font-family='Helvetica Neue, Helvetica, Arial, sans-serif'"
INK, MUTED, LINE = "#222", "#666", "#ccc"
BLUE, ORANGE = "#5b7fb4", "#d9822b"


def text(x, y, s, size=12, anchor="start", color=INK, weight="normal"):
    return (f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' "
            f"font-weight='{weight}'>{s}</text>")


# Share of text crops agreeing. Nordic units: 95% and 97% ("Problem" text);
# ces+spa: 135 of 248 crops disagreed, so 113 of 248 = 45.6% ("Problem" table).
UNITS = [
    ("Nordic unit (95%)", "", 95.0, BLUE),
    ("Nordic unit (97%)", "", 97.0, BLUE),
    ("ces+spa", "135 of 248 crops disagree", 113 / 248 * 100, ORANGE),
]
FLOOR = 80.0  # proposed pair threshold ("Pair threshold")

w, h = 760, 250
x0, x1, y0 = 190, 700, 50


def x(v):
    return x0 + (x1 - x0) * v / 100


b = []
for v in range(0, 101, 20):
    b.append(f"<line x1='{x(v):.1f}' y1='{y0 - 10}' x2='{x(v):.1f}' y2='{y0 + 3 * 46}' stroke='{LINE}'/>")
    b.append(text(x(v), y0 + 3 * 46 + 16, f"{v}%", 11, "middle", MUTED))
for i, (name, note, v, col) in enumerate(UNITS):
    y = y0 + i * 46
    if note:
        b.append(text(x0 - 12, y + 16, name, 12, "end"))
        b.append(text(x0 - 12, y + 30, note, 10, "end", MUTED))
    else:
        b.append(text(x0 - 12, y + 23, name, 12, "end"))
    b.append(f"<rect x='{x0}' y='{y + 4}' width='{x(v) - x0:.1f}' height='28' fill='{col}'/>")
    b.append(text(x(v) + 6, y + 23, f"{v:.0f}%" if i < 2 else "46%", 12))
b.append(f"<line x1='{x(FLOOR):.1f}' y1='{y0 - 10}' x2='{x(FLOOR):.1f}' y2='{y0 + 3 * 46}' stroke='{INK}' stroke-width='2' stroke-dasharray='5 3'/>")
b.append(text(x(FLOOR), y0 - 16, "proposed 80% floor", 12, "middle", INK, "bold"))
b.append(text((x0 + x1) / 2, y0 + 3 * 46 + 38, "Share of text crops where the two readings agree", 11, "middle", MUTED))
with open("qwen-per-crop-settlement.svg", "w") as f:
    f.write(f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w} {h}' width='{w}' height='{h}' {FONT}>"
            f"<rect width='{w}' height='{h}' fill='white'/>{''.join(b)}</svg>\n")
