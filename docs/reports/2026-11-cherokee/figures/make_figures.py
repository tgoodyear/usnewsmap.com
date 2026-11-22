#!/usr/bin/env python3
"""Draw the figures of report.md and letter-similarity.md as SVG (standard library only).

Every number is copied from a table or sentence in the reports; run from this directory.

report.md
  cer-by-reading.svg   section 5, step 3, the table of scores on test set v2 (PSM 13).
  finder-recall.svg    section 5, step 4, "Finding lines without ALTO", the finder table.
  ghn-pages-yield.svg  section 5, step 4, the inventory table (pages by year) and
                       "GHN scans" (yield estimate), plus the volume table in section 1.
letter-similarity.md
  letter-1nn.svg       "Letter k-NN accuracy (page-grouped folds)" table.
  letter-cells.svg     the Verdict table (best coverage at 98% look-alike accuracy).
"""

from pathlib import Path

HERE = Path(__file__).resolve().parent
FONT = "font-family='Helvetica Neue, Helvetica, Arial, sans-serif'"
INK, MUTED, LINE = "#222", "#666", "#ccc"
BLUE, ORANGE, GREEN, PURPLE = "#5b7fb4", "#d9822b", "#4f7a35", "#6b4c94"


def svg(w, h, body):
    return (f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w} {h}' width='{w}' height='{h}' {FONT}>"
            f"<rect width='{w}' height='{h}' fill='white'/>{body}</svg>\n")


def text(x, y, s, size=12, anchor="start", color=INK, weight="normal", halo=False):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    h = " stroke='white' stroke-width='4' paint-order='stroke'" if halo else ""
    return (f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' "
            f"font-weight='{weight}'{h}>{s}</text>")


def line(x1, y1, x2, y2, color=LINE, width=1, dash=None):
    d = f" stroke-dasharray='{dash}'" if dash else ""
    return f"<line x1='{x1:.1f}' y1='{y1:.1f}' x2='{x2:.1f}' y2='{y2:.1f}' stroke='{color}' stroke-width='{width}'{d}/>"


def rect(x, y, w, h, color):
    return f"<rect x='{x:.1f}' y='{y:.1f}' width='{max(w, 0.5):.1f}' height='{h:.1f}' fill='{color}' rx='1'/>"


def legend(x, y, items):
    b = []
    for label, color in items:
        b.append(rect(x, y - 9, 12, 12, color))
        b.append(text(x + 17, y + 2, label, 12))
        x += 17 + 7 * len(label) + 18
    return "".join(b)


def vbars(w, h, groups, series, ymin, ymax, step, unit, ylabel, fmt="{:.1f}", ints=None):
    """Grouped vertical bars. groups: labels; series: [(name, colour, [values], [(lo, hi)|None])]."""
    x0, x1, y0, y1 = 64, w - 16, 52, h - 62
    sy = lambda v: y1 - (y1 - y0) * (v - ymin) / (ymax - ymin)
    b = [legend(x0, 20, [(n, c) for n, c, _, _ in series])]
    v = ymin
    while v <= ymax + 1e-9:
        b.append(line(x0, sy(v), x1, sy(v)))
        b.append(text(x0 - 8, sy(v) + 4, f"{v:g}{unit}", 11, "end", MUTED))
        v += step
    b.append(text(14, (y0 + y1) / 2, ylabel, 11, "middle", MUTED).replace("<text ", f"<text transform='rotate(-90 14 {(y0 + y1) / 2})' "))
    gw = (x1 - x0) / len(groups)
    bw = min(46, gw * 0.8 / len(series))
    for gi, g in enumerate(groups):
        cx = x0 + gw * (gi + 0.5)
        b.append(text(cx, y1 + 18, g, 12, "middle"))
        for si, (_, color, vals, ci) in enumerate(series):
            xx = cx + (si - (len(series) - 1) / 2) * (bw + 3) - bw / 2
            val = vals[gi]
            b.append(rect(xx, sy(val), bw, sy(ymin) - sy(val), color))
            if ci and ci[gi]:
                lo, hi = ci[gi]
                m = xx + bw / 2
                b.append(line(m, sy(lo), m, sy(hi), INK, 1.5))
                b.append(line(m - 4, sy(lo), m + 4, sy(lo), INK, 1.5))
                b.append(line(m - 4, sy(hi), m + 4, sy(hi), INK, 1.5))
                top = sy(hi) - 5
            else:
                top = sy(val) - 5
            b.append(text(m if ci and ci[gi] else xx + bw / 2, top, fmt.format(val), 10.5, "middle", halo=True))
    return svg(w, h, "".join(b))


def cer_by_reading():
    # Step 3 table, test set v2 at PSM 13: character error, share of syllabary letters wrong.
    groups = ["All lines (2,160)", "Silver lines (700)", "Hand-checked lines (70)"]
    series = [
        ("LoC OCR", BLUE, [34.5, 18.0, 8.4], None),
        ("Tesseract stock", "#bbbbbb", [35.8, 19.7, 12.2], None),
        ("Round one", PURPLE, [30.5, 14.1, 9.7], None),
        ("chr_phoenix", ORANGE, [27.6, 10.2, 6.6], None),
    ]
    return vbars(760, 380, groups, series, 0, 40, 10, "%", "Character error (lower is better)", "{:.1f}%")


def finder_recall():
    # Step 4 finder table (30 LoC pages): line recall, line precision, Cherokee line recall.
    rows = [
        ("Tesseract layout, raw scan (live jobs)", 81.3, 69.5, 79.4),
        ("Same, short and rule-like boxes dropped", 81.3, 70.9, 79.4),
        ("Layout on Sauvola-binarised scan", 89.8, 81.6, 84.0),
        ("Same, with clean-up (chosen)", 89.7, 83.8, 84.0),
        ("Binarised, half size", 77.0, 68.6, 79.2),
        ("Binarised plus raw-scan boxes", 89.8, 81.2, 84.1),
        ("Kraken blla (2 pages only)", 1.2, 3.5, 0.7),
    ]
    cols = [("Line recall", BLUE), ("Line precision", ORANGE), ("Cherokee line recall", GREEN)]
    w, rh = 760, 52
    h = 70 + rh * len(rows) + 30
    x0, x1 = 270, 730
    sx = lambda v: x0 + (x1 - x0) * v / 100
    b = [legend(x0, 20, cols)]
    for v in range(0, 101, 20):
        b.append(line(sx(v), 40, sx(v), h - 34))
        b.append(text(sx(v), h - 18, f"{v}%", 11, "middle", MUTED))
    for i, (name, *vals) in enumerate(rows):
        y = 50 + rh * i
        if "chosen" in name:
            b.append(f"<rect x='8' y='{y - 4}' width='{w - 16}' height='{rh - 2}' fill='#f3f6fb'/>")
        b.append(text(x0 - 10, y + 24, name, 12, "end", weight="bold" if "chosen" in name else "normal"))
        for j, val in enumerate(vals):
            yy = y + j * 14
            b.append(rect(x0, yy, sx(val) - x0, 12, cols[j][1]))
            b.append(text(sx(val) + 5, yy + 10, f"{val:.1f}%", 10.5))
    return svg(w, h, "".join(b))


def ghn_pages_yield():
    # Left: inventory table, "By year, 1828 to 1834": 88, 76, 125, 120, 152, 82, 56 (sum 699).
    years = list(range(1828, 1835))
    pages = [88, 76, 125, 120, 152, 82, 56]
    # Right: searchable Cherokee words. Now: 85,000 (Summary, section 1). The plan assumed
    # "250,000 or more" (step 4). The sample estimate is about 65,000 new words (13,000
    # Cherokee lines, standard error 8,000, at 5 words a line), so about 150,000 in all.
    w, h = 760, 330
    b = [text(14, 22, "GHN pages to read, by year (699 pages)", 13, weight="bold")]
    x0, x1, y0, y1 = 50, 350, 50, 280
    sy = lambda v: y1 - (y1 - y0) * v / 160
    for v in range(0, 161, 40):
        b.append(line(x0, sy(v), x1, sy(v)))
        b.append(text(x0 - 6, sy(v) + 4, str(v), 11, "end", MUTED))
    gw = (x1 - x0) / len(years)
    for i, (yr, n) in enumerate(zip(years, pages)):
        cx = x0 + gw * (i + 0.5)
        b.append(rect(cx - 15, sy(n), 30, sy(0) - sy(n), BLUE))
        b.append(text(cx, sy(n) - 5, str(n), 11, "middle"))
        b.append(text(cx, y1 + 18, str(yr), 11, "middle", MUTED))
    b.append(text(x0 + 4, y1 + 38, "Pages", 11, color=MUTED))
    b.append(text(420, 22, "Searchable Cherokee words, thousands", 13, weight="bold"))
    rx0, rx1 = 420, 730
    sx = lambda v: rx0 + 125 + (rx1 - rx0 - 135) * v / 300
    rows = [
        ("Today (LoC pages)", 85, 85, None, BLUE),
        ("Plan assumed", 250, 250, None, "#bbbbbb"),
        ("Sample estimate", 150, 150, (110, 190), ORANGE),
    ]
    for v in range(0, 301, 100):
        b.append(line(sx(v), 44, sx(v), 232))
        b.append(text(sx(v), 250, str(v), 11, "middle", MUTED))
    for i, (name, val, _, ci, color) in enumerate(rows):
        y = 62 + 58 * i
        b.append(text(rx0, y + 20, name, 12))
        b.append(rect(sx(0), y, sx(val) - sx(0), 30, color))
        b.append(text(sx(val) + 6 if not ci else sx(ci[1]) + 6, y + 20, ("250+" if val == 250 else f"{val}") + ("" if not ci else " (110 to 190)"), 11))
        if ci:
            b.append(line(sx(ci[0]), y + 15, sx(ci[1]), y + 15, INK, 1.5))
            b.append(line(sx(ci[0]), y + 8, sx(ci[0]), y + 22, INK, 1.5))
            b.append(line(sx(ci[1]), y + 8, sx(ci[1]), y + 22, INK, 1.5))
    b.append(text(rx0, 276, "Estimate: 85 now plus 65 (+/- 40, one standard error)", 11, color=MUTED))
    b.append(text(rx0, 292, "from 13,000 Cherokee lines at 5 words a line. Its error", 11, color=MUTED))
    b.append(text(rx0, 308, "bar covers only the line count, not the 12-page sample.", 11, color=MUTED))
    return svg(w, h, "".join(b))


def letter_1nn():
    # "Letter k-NN accuracy (page-grouped folds)": 1-NN accuracy and its 95% interval.
    groups = ["All letters (2,148)", "Tier A (321)", "Look-alike (260)", "Look-alike, tier A (38)"]
    hog = [92.6, 97.2, 92.7, 92.1]
    hog_ci = [(91.4, 93.6), (94.8, 98.5), (88.9, 95.3), (79.2, 97.3)]
    dino = [79.0, 85.0, 79.6, 86.8]
    dino_ci = [(77.2, 80.7), (80.7, 88.5), (74.3, 84.1), (72.7, 94.2)]
    series = [("Pixels + HOG", BLUE, hog, hog_ci), ("DINOv2-small", ORANGE, dino, dino_ci)]
    return vbars(760, 380, groups, series, 60, 100, 10, "%", "1-NN accuracy (95% interval)", "{:.1f}")


def letter_cells():
    # Verdict table: best coverage at 98% look-alike accuracy, with the look-alike letters covered.
    cells = [
        ("DINOv2-small, tier A", 63.2, "22 of 22 correct"),
        ("Pixels + HOG, all letters", 29.5, "71 of 72 correct"),
        ("Pixels + HOG, tier A", 28.7, "8 of 8 correct"),
        ("DINOv2-small, all letters", 14.8, "26 of 26 correct"),
    ]
    w, rh = 760, 62
    h = 80 + rh * len(cells)
    x0, x1 = 210, 735
    sx = lambda v: x0 + (x1 - x0) * v / 70
    b = [text(x0, 20, "Share of letters covered at 98% look-alike accuracy", 12, color=MUTED)]
    for v in range(0, 71, 10):
        b.append(line(sx(v), 34, sx(v), h - 34))
        b.append(text(sx(v), h - 18, f"{v}%", 11, "middle", MUTED))
    for i, (name, cov, note) in enumerate(cells):
        y = 46 + rh * i
        met = cov >= 30
        b.append(text(x0 - 10, y + 22, name, 12, "end"))
        b.append(rect(x0, y, sx(cov) - x0, 32, GREEN if met else "#bbbbbb"))
        inside = sx(cov) + 120 > w
        tx, anchor = (sx(cov) - 6, "end") if inside else (max(sx(cov), sx(30)) + 10, "start")
        b.append(text(tx, y + 15, f"{cov:.1f}%" + (" (met)" if met else " (not met)"), 12, anchor, "white" if inside else INK, "bold"))
        b.append(text(tx, y + 30, note, 11, anchor, "white" if inside else MUTED))
    b.append(line(sx(30), 34, sx(30), h - 34, INK, 1.5, "5 4"))
    b.append(text(sx(30) + 4, h - 38, "Rule: at least 30%", 11, color=INK, halo=True))
    return svg(w, h, "".join(b))


if __name__ == "__main__":
    for name, fn in [
        ("cer-by-reading", cer_by_reading),
        ("finder-recall", finder_recall),
        ("ghn-pages-yield", ghn_pages_yield),
        ("letter-1nn", letter_1nn),
        ("letter-cells", letter_cells),
    ]:
        (HERE / f"{name}.svg").write_text(fn())
