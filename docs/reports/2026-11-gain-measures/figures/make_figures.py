#!/usr/bin/env python3
"""Draw the report's figures as SVG, standard library only. Every number is
copied from report.md: "Results" (rho table), "Damage against CER, by language
and unit" (CER drop at 0.02) and "Production-scale snapshot". Run from anywhere."""

from pathlib import Path

HERE = Path(__file__).resolve().parent
FONT = "font-family='Helvetica Neue, Helvetica, Arial, sans-serif'"
INK, MUTED, LINE = "#222", "#666", "#ccc"
BLUE, ORANGE, GREEN, PURPLE = "#5b7fb4", "#d9822b", "#4f7a35", "#6b4c94"


def svg(w, h, body):
    return (f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w} {h}' width='{w}' height='{h}' {FONT}>"
            f"<rect width='{w}' height='{h}' fill='white'/>{body}</svg>\n")


def text(x, y, s, size=12, anchor="start", color=INK, weight="normal"):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    return f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' font-weight='{weight}'>{s}</text>"


def line(x1, y1, x2, y2, color=LINE, w=1):
    return f"<line x1='{x1:.1f}' y1='{y1:.1f}' x2='{x2:.1f}' y2='{y2:.1f}' stroke='{color}' stroke-width='{w}'/>"


def rho():
    """Results table: Spearman rho (95% limits) of the measure's drop against the CER drop."""
    # (language, damage (rho, lo, hi), lm_oov (rho, lo, hi))
    data = [
        ("Pooled (338 pages)", (0.48, 0.42, 0.54), (0.65, 0.60, 0.70)),
        ("English (28)", (0.85, 0.72, 0.93), (0.88, 0.74, 0.96)),
        ("French (20)", (0.80, 0.64, 0.91), (0.93, 0.88, 0.96)),
        ("Swedish (22)", (0.88, 0.82, 0.92), (0.93, 0.84, 0.97)),
        ("Finnish (20)", (0.94, 0.91, 0.96), (0.85, 0.70, 0.92)),
        ("German (248)", (0.28, 0.20, 0.36), (0.56, 0.49, 0.64)),
    ]
    w, h = 760, 380
    x0, x1 = 170, 730
    y0, rh = 40, 50

    def x(v):
        return x0 + (x1 - x0) * v

    b = [text(x0, 16, "Spearman rho of the drop in each measure against the real CER drop (page pairs)", 12, color=MUTED)]
    for t in (0, 0.2, 0.4, 0.6, 0.8, 1.0):
        b.append(line(x(t), y0 - 8, x(t), y0 + rh * len(data) - 6))
        b.append(text(x(t), y0 + rh * len(data) + 8, f"{t:.1f}", 11, "middle", MUTED))
    b.append(text((x0 + x1) / 2, y0 + rh * len(data) + 28, "rho (1 = drop tracks real CER drop perfectly), bars show 95% limits", 11, "middle", MUTED))
    for i, (lang, d, l) in enumerate(data):
        yc = y0 + rh * i + 14
        b.append(text(x0 - 10, yc + 8, lang, 12, "end", weight="bold" if i == 0 else "normal"))
        for off, (r, lo, hi), col in ((-7, d, BLUE), (9, l, ORANGE)):
            yy = yc + off
            b.append(line(x(lo), yy, x(hi), yy, col, 2.5))
            b.append(f"<circle cx='{x(r):.1f}' cy='{yy:.1f}' r='4.5' fill='{col}'/>")
            b.append(text(x(hi) + 6, yy + 4, f"{r:.2f}", 11, color=col))
    ly = y0 + rh * len(data) + 48
    b.append(f"<circle cx='{x0}' cy='{ly}' r='4.5' fill='{BLUE}'/>" + text(x0 + 10, ly + 4, "damage", 12))
    b.append(f"<circle cx='{x0 + 100}' cy='{ly}' r='4.5' fill='{ORANGE}'/>" + text(x0 + 110, ly + 4, "lm_oov", 12))
    return svg(w, h, "".join(b))


def implied():
    """'Damage against CER' tables: CER drop (points) implied by a 0.02 damage drop."""
    langs = ["English", "French", "Swedish", "Finnish", "German"]
    series = [
        ("crops", [0.8, 1.5, 0.8, 0.4, 0.3], "#9fb5d6"),   # text below table: crop level
        ("pages", [1.2, 2.3, 0.9, 0.7, 0.4], BLUE),        # page-level table
        ("chunks", [1.2, 2.2, 1.1, 0.7, 0.2], PURPLE),     # text below table: chunk level
    ]
    w, h = 760, 340
    x0, x1, yb, yt = 70, 740, 270, 30
    vmax = 2.5

    def y(v):
        return yb - (yb - yt) * v / vmax

    b = []
    for t in (0, 0.5, 1.0, 1.5, 2.0, 2.5):
        b.append(line(x0, y(t), x1, y(t)))
        b.append(text(x0 - 8, y(t) + 4, f"{t:.1f}", 11, "end", MUTED))
    b.append(f"<text x='16' y='{(yb + yt) / 2}' font-size='11' fill='{MUTED}' text-anchor='middle' transform='rotate(-90 16 {(yb + yt) / 2})'>CER drop implied by a 0.02 damage drop (points)</text>")
    gw = (x1 - x0) / len(langs)
    bw = 34
    for i, lang in enumerate(langs):
        gx = x0 + gw * i + gw / 2
        b.append(text(gx, yb + 20, lang, 12, "middle"))
        for j, (name, vals, col) in enumerate(series):
            bx = gx + (j - 1) * (bw + 4) - bw / 2
            b.append(f"<rect x='{bx:.1f}' y='{y(vals[i]):.1f}' width='{bw}' height='{yb - y(vals[i]):.1f}' fill='{col}'/>")
            b.append(text(bx + bw / 2, y(vals[i]) - 4, f"{vals[i]:.1f}", 11, "middle"))
    for j, (name, vals, col) in enumerate(series):
        lx = 300 + 100 * j
        b.append(f"<rect x='{lx}' y='306' width='12' height='12' fill='{col}'/>" + text(lx + 18, 317, name, 12))
    return svg(w, h, "".join(b))


def production():
    """Production-scale snapshot: mean damage drop and estimated-CER change by cell."""
    # (label, damage drop, estimated CER change or None where the report gives none)
    rows = [
        ("Worst band (18,693 pages)", 0.137, None),
        ("Moderate band (1,525 pages)", -0.002, None),
        ("German 1890 (187 pairs)", 0.147, 0.003),
        ("German 1900 (1,167 pairs)", 0.130, -0.054),
    ]
    w, h = 760, 330
    x0, x1 = 230, 730
    lo, hi = -0.08, 0.16

    def x(v):
        return x0 + (x1 - x0) * (v - lo) / (hi - lo)

    b = []
    for t in (-0.05, 0, 0.05, 0.10, 0.15):
        b.append(line(x(t), 30, x(t), 250, "#888" if t == 0 else LINE, 1.5 if t == 0 else 1))
        b.append(text(x(t), 268, f"{t:.2f}", 11, "middle", MUTED))
    b.append(text((x0 + x1) / 2, 288, "mean change per page (positive = improvement)", 11, "middle", MUTED))
    for i, (lab, dmg, cer) in enumerate(rows):
        yc = 46 + 52 * i
        b.append(text(x0 - 10, yc + 14, lab, 12, "end"))
        b.append(f"<rect x='{min(x(0), x(dmg)):.1f}' y='{yc}' width='{abs(x(dmg) - x(0)):.1f}' height='14' fill='{BLUE}'/>")
        b.append(text(x(max(dmg, 0)) + 6, yc + 12, f"{dmg:+.3f}", 11))
        if cer is None:
            b.append(text(x(0) + 6 if dmg >= 0 else x(0) + 40, yc + 31, "estimated-CER change not reported for this row", 10, color=MUTED))
        else:
            b.append(f"<rect x='{min(x(0), x(cer)):.1f}' y='{yc + 17}' width='{max(abs(x(cer) - x(0)), 1.5):.1f}' height='14' fill='{ORANGE}'/>")
            b.append(text(x(max(cer, 0)) + 6, yc + 29, f"{cer:+.3f}", 11))
    b.append(f"<rect x='{x0}' y='302' width='12' height='12' fill='{BLUE}'/>" + text(x0 + 18, 313, "damage drop", 12))
    b.append(f"<rect x='{x0 + 130}' y='302' width='12' height='12' fill='{ORANGE}'/>" + text(x0 + 148, 313, "estimated-CER gain", 12))
    return svg(w, h, "".join(b))


for name, fn in (("rho", rho), ("implied-cer", implied), ("production", production)):
    (HERE / f"{name}.svg").write_text(fn())
