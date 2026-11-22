#!/usr/bin/env python3
"""Draw the reader-comparison report's figures as SVG, standard library only.

Sources, all in report.md: "Tesseract by set" (Tesseract PSM 6 CER and 95% limits),
"Xiaomi-OCR-0 and Qwen 8B against Tesseract" (reader CERs and paired differences from
Tesseract PSM 6 with 95% limits), and "Recommended reader per language". Run from this
directory."""

from pathlib import Path

HERE = Path(__file__).resolve().parent
FONT = "font-family='Helvetica Neue, Helvetica, Arial, sans-serif'"
INK, MUTED, LINE = "#222", "#666", "#ccc"
TESS, XIAOMI, QWEN, PURPLE = "#5b7fb4", "#d9822b", "#4f7a35", "#6b4c94"

# (set, Tesseract PSM 6 CER, low, high, Xiaomi CER, 8B CER)   -- Tesseract by set; Xiaomi/8B table
CER = [
    ("en-op", 4.06, 2.96, 5.17, 2.12, 3.22),
    ("fr-new", 11.52, 6.31, 17.17, 6.08, 6.05),
    ("fr-52", 5.14, 3.38, 7.73, 2.62, 2.42),
    ("fi-new", 10.46, 9.34, 11.42, 5.40, 3.30),
    ("fi-52", 7.88, 4.12, 10.11, 4.22, 2.33),
    ("sv-new", 8.89, 6.93, 10.48, 2.53, 2.09),
    ("sv-52", 11.01, 10.60, 11.41, 3.03, 1.71),
    ("de-rz81", 4.90, 4.02, 5.89, 5.15, 4.80),
    ("de-nzz", 13.36, 10.43, 16.76, 7.99, 8.91),
]
# (set, Xiaomi minus PSM 6, low, high, 8B minus PSM 6, low, high)   -- Xiaomi/8B table
DIFF = [
    ("en-op", -1.94, -2.49, -1.33, -0.84, -2.21, 0.92),
    ("fr-new", -5.43, -7.26, -3.70, -5.47, -7.28, -3.70),
    ("fr-52", -2.53, -4.50, -1.37, -2.73, -4.70, -1.53),
    ("fi-new", -5.06, -6.23, -3.71, -7.16, -7.95, -6.27),
    ("fi-52", -3.66, -5.02, -1.67, -5.55, -7.26, -2.43),
    ("sv-new", -6.36, -7.75, -4.71, -6.79, -8.27, -5.03),
    ("sv-52", -7.98, -9.50, -6.18, -9.30, -9.81, -8.20),
    ("de-rz81", 0.25, -0.69, 1.07, -0.10, -0.99, 0.74),
    ("de-nzz", -5.38, -8.24, -2.72, -4.46, -6.34, -2.77),
]
# (language, set, reader, margin vs Tesseract, low, high, note)   -- Recommended reader table
PICK = [
    ("French", "fr-new", "Xiaomi-OCR-0", -5.43, -7.26, -3.70, XIAOMI, ""),
    ("Finnish", "fi-new", "Qwen3-VL 8B", -7.16, -7.95, -6.27, QWEN, ""),
    ("Swedish", "sv-new", "Qwen3-VL 8B", -6.79, -8.27, -5.03, QWEN, ""),
    ("German", "de-rz81", "Tesseract stays (best: 8B)", -0.10, -0.99, 0.74, MUTED, "not separated; provisional"),
    ("English", "en-op", "Xiaomi-OCR-0", -1.94, -2.49, -1.33, XIAOMI, "provisional"),
]


def svg(w, h, body):
    return (f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w} {h}' width='{w}' height='{h}' {FONT}>"
            f"<rect width='{w}' height='{h}' fill='white'/>{body}</svg>\n")


def text(x, y, s, size=12, anchor="start", color=INK, weight="normal", rot=None):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    t = f" transform='rotate({rot} {x} {y})'" if rot else ""
    return (f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' "
            f"font-weight='{weight}'{t}>{s}</text>")


def line(x1, y1, x2, y2, color=INK, w=1, dash=""):
    d = f" stroke-dasharray='{dash}'" if dash else ""
    return f"<line x1='{x1:.1f}' y1='{y1:.1f}' x2='{x2:.1f}' y2='{y2:.1f}' stroke='{color}' stroke-width='{w}'{d}/>"


def cer_bars():
    w, h = 760, 380
    l, r, t, b = 60, 16, 40, 60
    ymax = 18.0
    pw, ph = w - l - r, h - t - b

    def y(v):
        return t + ph * (1 - v / ymax)

    o = []
    for v in range(0, 19, 3):
        o.append(line(l, y(v), w - r, y(v), LINE))
        o.append(text(l - 6, y(v) + 4, f"{v}%", 11, "end", MUTED))
    o.append(text(16, t + ph / 2, "CER (share of characters wrong)", 12, "middle", MUTED, rot=-90))
    gw = pw / len(CER)
    bw = gw / 4
    for i, (name, tc, lo, hi, xc, qc) in enumerate(CER):
        gx = l + gw * i + bw / 2
        for j, (v, col) in enumerate(((tc, TESS), (xc, XIAOMI), (qc, QWEN))):
            x = gx + bw * j
            o.append(f"<rect x='{x:.1f}' y='{y(v):.1f}' width='{bw - 2:.1f}' height='{y(0) - y(v):.1f}' fill='{col}'/>")
        cx = gx + bw / 2 - 1
        o.append(line(cx, y(lo), cx, y(hi), INK, 1.5))
        for v in (lo, hi):
            o.append(line(cx - 3, y(v), cx + 3, y(v), INK, 1.5))
        o.append(text(gx + 1.5 * bw, h - b + 16, name, 11, "middle"))
    o.append(line(l, y(0), w - r, y(0), INK))
    for k, (lab, col) in enumerate((("Tesseract PSM 6 (bars: 95% limits)", TESS), ("Xiaomi-OCR-0", XIAOMI), ("Qwen3-VL 8B", QWEN))):
        x = l + [0, 250, 380][k]
        o.append(f"<rect x='{x}' y='12' width='12' height='12' fill='{col}'/>")
        o.append(text(x + 17, 23, lab, 12))
    o.append(text(l + pw / 2, h - 10, "Frozen set (the -new sets are held out; others are second use)", 11, "middle", MUTED))
    return svg(w, h, "".join(o))


def diff_dots():
    w, h = 760, 400
    l, r, t, b = 90, 20, 36, 50
    xmin, xmax = -10.5, 2.0
    pw, ph = w - l - r, h - t - b

    def x(v):
        return l + pw * (v - xmin) / (xmax - xmin)

    o = []
    for v in range(-10, 3, 2):
        o.append(line(x(v), t, x(v), t + ph, LINE))
        o.append(text(x(v), t + ph + 16, str(v), 11, "middle", MUTED))
    o.append(line(x(0), t, x(0), t + ph, INK, 1.5))
    o.append(text(x(-5), h - 10, "CER difference from Tesseract PSM 6, points (left of zero = reader better)", 12, "middle", MUTED))
    rh = ph / len(DIFF)
    for i, row in enumerate(DIFF):
        cy = t + rh * (i + 0.5)
        o.append(text(l - 8, cy + 4, row[0], 12, "end"))
        for dy, (m, lo, hi), col in ((-5, row[1:4], XIAOMI), (5, row[4:7], QWEN)):
            o.append(line(x(lo), cy + dy, x(hi), cy + dy, col, 2))
            o.append(f"<circle cx='{x(m):.1f}' cy='{cy + dy:.1f}' r='3.8' fill='{col}'/>")
    for k, (lab, col) in enumerate((("Xiaomi-OCR-0", XIAOMI), ("Qwen3-VL 8B", QWEN))):
        o.append(f"<circle cx='{l + 160 * k + 6}' cy='17' r='4' fill='{col}'/>")
        o.append(text(l + 160 * k + 16, 21, lab, 12))
    return svg(w, h, "".join(o))


def picks():
    w, h = 760, 290
    l, r, t, b = 330, 20, 36, 50
    xmin, xmax = -9.0, 2.0
    pw = w - l - r
    ph = h - t - b

    def x(v):
        return l + pw * (v - xmin) / (xmax - xmin)

    o = []
    for v in range(-8, 3, 2):
        o.append(line(x(v), t, x(v), t + ph, LINE))
        o.append(text(x(v), t + ph + 16, str(v), 11, "middle", MUTED))
    o.append(line(x(0), t, x(0), t + ph, INK, 1.5))
    o.append(text(l + pw / 2, h - 10, "Winner's CER minus Tesseract PSM 6, points, 95% limits", 12, "middle", MUTED))
    rh = ph / len(PICK)
    for i, (lang, s, reader, m, lo, hi, col, note) in enumerate(PICK):
        cy = t + rh * (i + 0.5)
        o.append(text(10, cy - 1, f"{lang} ({s})", 12, weight="bold"))
        o.append(text(10, cy + 13, reader + (f"; {note}" if note else ""), 11, color=MUTED))
        o.append(line(x(lo), cy, x(hi), cy, col, 3))
        o.append(f"<circle cx='{x(m):.1f}' cy='{cy:.1f}' r='5' fill='{col}'/>")
        o.append(text(x(hi) + 8, cy + 4, f"{m:+.2f}".replace("-", "−"), 11, color=MUTED))
    return svg(w, h, "".join(o))


if __name__ == "__main__":
    for name, fn in (("cer-by-reader", cer_bars), ("difference-from-tesseract", diff_dots), ("recommended-readers", picks)):
        (HERE / f"{name}.svg").write_text(fn())
