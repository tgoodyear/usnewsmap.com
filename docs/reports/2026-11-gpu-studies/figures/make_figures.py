#!/usr/bin/env python3
"""Draw the report's figures as SVG, standard library only. Every number is copied
from report.md: Figure 1 from Study 9 "Crop accuracy" (612 crops) and Study 11
"Regions" (`de-rz` row); Figure 2 from Study 11 "Regions" (difference column) and
"Where Xiaomi-OCR-0 loses" (fitted vs native size). Run from this directory."""

from pathlib import Path

HERE = Path(__file__).resolve().parent
FONT = "font-family='Helvetica Neue, Helvetica, Arial, sans-serif'"
INK, MUTED, LINE = "#222", "#666", "#ccc"
BLUE, ORANGE, GREEN, PURPLE, GREY = "#5b7fb4", "#d9822b", "#4f7a35", "#6b4c94", "#bbbbbb"


def svg(w, h, body):
    return (f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w} {h}' width='{w}' height='{h}' {FONT}>"
            f"<rect width='{w}' height='{h}' fill='white'/>{body}</svg>\n")


def text(x, y, s, size=12, anchor="start", color=INK, weight="normal"):
    s = s.replace("&", "&amp;").replace("<", "&lt;")
    return f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' font-weight='{weight}'>{s}</text>"


def bars(x0, y0, w, h, ymax, step, rows, heading):
    """Vertical bars with optional 95% limits. rows: (label, value, lo, hi, colour)."""
    b = [text(x0, y0 - 14, heading, 13, weight="bold")]
    for t in range(0, ymax + 1, step):
        y = y0 + h - h * t / ymax
        b.append(f"<line x1='{x0}' y1='{y:.1f}' x2='{x0 + w}' y2='{y:.1f}' stroke='{LINE}'/>")
        b.append(text(x0 - 6, y + 4, str(t), 11, "end", MUTED))
    b.append(text(x0 - 36, y0 + h / 2, "", 11))
    slot = w / len(rows)
    for i, (label, v, lo, hi, col) in enumerate(rows):
        cx = x0 + slot * (i + 0.5)
        bw = slot * 0.55
        yv = y0 + h - h * v / ymax
        b.append(f"<rect x='{cx - bw / 2:.1f}' y='{yv:.1f}' width='{bw:.1f}' height='{y0 + h - yv:.1f}' fill='{col}'/>")
        top = yv
        if lo is not None:
            ylo, yhi = y0 + h - h * lo / ymax, y0 + h - h * hi / ymax
            b.append(f"<line x1='{cx:.1f}' y1='{ylo:.1f}' x2='{cx:.1f}' y2='{yhi:.1f}' stroke='{INK}' stroke-width='1.5'/>")
            for yy in (ylo, yhi):
                b.append(f"<line x1='{cx - 5:.1f}' y1='{yy:.1f}' x2='{cx + 5:.1f}' y2='{yy:.1f}' stroke='{INK}' stroke-width='1.5'/>")
            top = yhi
        b.append(text(cx, top - 5, f"{v:g}", 11, "middle"))
        b.append(text(cx, y0 + h + 16, label, 11, "middle"))
    return "".join(b)


def figure1():
    """CER by reader: 612 human-transcribed crops (Study 9), then held-out `de-rz` regions (Study 11)."""
    w, h = 760, 330
    # Study 9 "Crop accuracy", All (612) row; the report gives limits only for differences
    crops = [("Xiaomi", 4.0, None, None, ORANGE), ("Qwen 8B", 4.5, None, None, BLUE),
             ("CHURRO", 6.4, None, None, PURPLE), ("Tesseract", 9.0, None, None, GREEN)]
    # Study 11 "Regions", `de-rz` row (limits in brackets); RapidOCR and Vote have no limits there
    held = [("Xiaomi", 10.4, 7.4, 13.7, ORANGE), ("Qwen 8B", 12.4, 7.9, 18.1, BLUE),
            ("Tesseract", 6.5, 5.2, 8.2, GREEN), ("RapidOCR", 17.0, None, None, GREY), ("Vote", 7.3, None, None, GREY)]
    b = [bars(50, 50, 270, 220, 20, 5, crops, "612 crops (Study 9)"),
         bars(400, 50, 330, 220, 20, 5, held, "81 held-out pages, 2,000 regions (Study 11)"),
         text(50, 312, "CER, % of characters wrong (lower is better). Error bars are 95% limits; none are reported for the 612-crop pooled CERs.", 11, color=MUTED)]
    return svg(w, h, "".join(b))


def figure2():
    """Xiaomi-OCR-0 minus Tesseract, CER points, with 95% limits; zero line = tie."""
    w, h = 760, 270
    x0, x1 = 300, 610
    lo, hi = -2, 20

    def x(v):
        return x0 + (x1 - x0) * (v - lo) / (hi - lo)

    # Study 11 "Regions" (de-rz, de-cg, not-in-CHURRO rows) and "Where Xiaomi-OCR-0 loses" (fitted vs native table)
    rows = [("de-rz, fitted to 2,048 px (protocol)", 3.9, 0.9, 7.8, ORANGE),
            ("de-rz, native size, no guard (v5, post-hoc)", 1.3, -0.7, 3.3, GREEN),
            ("de-rz, pages not in CHURRO's training", 1.8, 0.0, 2.9, BLUE),
            ("de-cg, fitted (protocol)", 14.0, 9.3, 18.8, PURPLE)]
    b = []
    for t in range(lo, hi + 1, 2):
        b.append(f"<line x1='{x(t):.1f}' y1='20' x2='{x(t):.1f}' y2='200' stroke='{LINE}'/>")
        b.append(text(x(t), 218, str(t), 11, "middle", MUTED))
    b.append(f"<line x1='{x(0):.1f}' y1='20' x2='{x(0):.1f}' y2='200' stroke='{INK}' stroke-width='1.5'/>")
    b.append(text(x(0) + 5, 14, "0 = tie", 11, color=INK))
    b.append(text(x(hi), 14, "Tesseract better →", 11, "end", MUTED))
    for i, (label, v, l, u, col) in enumerate(rows):
        y = 50 + i * 45
        b.append(text(x0 - 12, y + 4, label, 12, "end"))
        b.append(f"<line x1='{x(l):.1f}' y1='{y}' x2='{x(u):.1f}' y2='{y}' stroke='{col}' stroke-width='3'/>")
        b.append(f"<circle cx='{x(v):.1f}' cy='{y}' r='6' fill='{col}'/>")
        b.append(text(x(u) + 8, y + 4, f"{v:+.1f} ({l:.1f} to {u:.1f})", 11, color=MUTED))
    b.append(text(x0, 250, "Xiaomi-OCR-0 CER minus Tesseract CER, percentage points, with 95% limits", 11, color=MUTED))
    return svg(w, h, "".join(b))


if __name__ == "__main__":
    (HERE / "fig1-cer-by-reader.svg").write_text(figure1())
    (HERE / "fig2-xiaomi-minus-tesseract.svg").write_text(figure2())
