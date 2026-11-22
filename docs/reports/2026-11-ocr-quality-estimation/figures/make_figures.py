#!/usr/bin/env python3
"""Draw the report's figures as SVG (standard library only; run from any directory).

Sources, all in report.md:
- measures.svg: "Correlation with CER" table (All column, per crop) and "Paired
  decisions" table (All column, crops). Masked-LM numbers cover a sample only.
- transfer.svg: "Transfer to a language without truth" table.
- vlm-blind-spot.svg: "Per reader pair" table, English LoC vs Qwen 8B row
  (324 pairs, first reader truly better 0.11 = 35 crops). The caught counts are
  derived: with a = accuracy on crops Qwen wins and b = accuracy on crops LoC wins,
  a + b = 2 x balanced accuracy and (289 a + 35 b) / 324 = accuracy, so
  caught = 35 b. Rounded inputs make the counts good to about +-2 crops.
"""

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
    return (f"<text x='{x}' y='{y}' font-size='{size}' text-anchor='{anchor}' fill='{color}' "
            f"font-weight='{weight}'>{s}</text>")


def measures():
    """Eleven measures ranked by Spearman rho with CER per crop (all truth sets); paired accuracy beside."""
    # (name, rho per crop "All", paired accuracy "All" or None)  -- two tables above
    rows = [("mlm_pppl (XLM-R, sample only)", 0.61, 0.88), ("lm_oov", 0.58, 0.83), ("combo_lolo", 0.52, 0.0),
            ("charlm_bpc", 0.50, 0.81), ("oov", 0.50, 0.77), ("nearmiss_any", 0.46, 0.73),
            ("damage (production)", 0.41, 0.70), ("nearmiss_conf50", 0.37, None),
            ("unknown (top 5,000)", 0.36, 0.79), ("garbage", 0.22, 0.59), ("script_mix", 0.00, None)]
    rows = [(n, r, (p or None)) for n, r, p in rows]
    w, top = 760, 34
    h = top + 26 * len(rows) + 40
    x0, x1 = 210, 450          # rho axis, 0 to 0.8
    p0, p1 = 520, 730          # paired accuracy axis, 0.5 to 1.0
    b = [text(x0, 18, "Spearman rho with true CER (per crop)", 12, weight="bold"),
         text(p0, 18, "Paired accuracy (1-point pairs)", 12, weight="bold")]
    for v in (0, 0.2, 0.4, 0.6, 0.8):
        x = x0 + (x1 - x0) * v / 0.8
        b.append(f"<line x1='{x:.1f}' y1='{top - 6}' x2='{x:.1f}' y2='{top + 26 * len(rows)}' stroke='{LINE}'/>")
        b.append(text(x, top + 26 * len(rows) + 14, f"{v:.1f}", 11, "middle", MUTED))
    for v in (0.5, 0.75, 1.0):
        x = p0 + (p1 - p0) * (v - 0.5) / 0.5
        b.append(f"<line x1='{x:.1f}' y1='{top - 6}' x2='{x:.1f}' y2='{top + 26 * len(rows)}' stroke='{LINE}'/>")
        b.append(text(x, top + 26 * len(rows) + 14, f"{v:.2f}", 11, "middle", MUTED))
    for i, (name, rho, acc) in enumerate(rows):
        y = top + i * 26
        col = ORANGE if name.startswith("lm_oov") else (MUTED if "damage" in name or "unknown" in name else BLUE)
        b.append(text(x0 - 8, y + 14, name, 12, "end", weight="bold" if name == "lm_oov" else "normal"))
        b.append(f"<rect x='{x0}' y='{y + 2}' width='{max((x1 - x0) * rho / 0.8, 1):.1f}' height='16' fill='{col}'/>")
        b.append(text(x0 + (x1 - x0) * rho / 0.8 + 5, y + 14, f"{rho:.2f}", 11, color=MUTED))
        if acc:
            x = p0 + (p1 - p0) * (acc - 0.5) / 0.5
            b.append(f"<circle cx='{x:.1f}' cy='{y + 10}' r='5' fill='{col}'/>")
            b.append(text(x + 9, y + 14, f"{acc:.2f}", 11, color=MUTED))
        else:
            b.append(text(p0 + 4, y + 14, "not reported", 11, color=MUTED))
    return svg(w, h, "".join(b))


def transfer():
    """Leave-one-language-out accuracy of lm_oov on 1-point crop pairs, with the share of pairs decided."""
    rows = [("English", 0.93, 0.73), ("French", 0.99, 0.84), ("Finnish", 0.88, 0.96),
            ("Swedish", 0.98, 0.92), ("German", 0.81, 0.79)]  # table "Transfer to a language without truth"
    w, top = 760, 34
    h = top + 34 * len(rows) + 44
    x0, x1 = 120, 520          # accuracy axis 0.5 to 1.0
    b = [text(x0, 18, "Accuracy on the held-out language (margin chosen on the others)", 12, weight="bold")]
    for v in (0.5, 0.6, 0.7, 0.8, 0.9, 1.0):
        x = x0 + (x1 - x0) * (v - 0.5) / 0.5
        b.append(f"<line x1='{x:.1f}' y1='{top - 6}' x2='{x:.1f}' y2='{top + 34 * len(rows)}' stroke='{LINE}'/>")
        b.append(text(x, top + 34 * len(rows) + 14, f"{v:.1f}", 11, "middle", MUTED))
    x90 = x0 + (x1 - x0) * 0.8
    b.append(f"<line x1='{x90:.1f}' y1='{top - 6}' x2='{x90:.1f}' y2='{top + 34 * len(rows)}' stroke='{INK}' stroke-dasharray='5 4'/>")
    b.append(text(x90, top + 34 * len(rows) + 30, "90% bar", 11, "middle", MUTED))
    for i, (name, acc, share) in enumerate(rows):
        y = top + i * 34
        col = BLUE if acc >= 0.9 else ORANGE
        b.append(text(x0 - 8, y + 18, name, 12, "end"))
        b.append(f"<rect x='{x0}' y='{y + 4}' width='{(x1 - x0) * (acc - 0.5) / 0.5:.1f}' height='20' fill='{col}'/>")
        b.append(text(x0 + (x1 - x0) * (acc - 0.5) / 0.5 + 6, y + 19, f"{acc:.2f}", 12))
        b.append(text(x1 + 70, y + 19, f"{share:.0%} of pairs decided", 11, color=MUTED))
    return svg(w, h, "".join(b))


def blind_spot():
    """Of the 35 en-op crops where LoC beats Qwen by 1 point or more, how many each measure picks LoC on."""
    # (measure, accuracy, balanced accuracy)  -- "Per reader pair", English LoC vs Qwen 8B
    rows = [("lm_oov", 0.89, 0.57), ("charlm_bpc", 0.88, 0.60), ("unknown (top 5,000)", 0.89, 0.57)]
    n, loc_wins = 324, 35
    qwen_wins = n - loc_wins
    w, top = 760, 40
    h = top + 44 * len(rows) + 50
    x0, x1 = 170, 640
    sc = (x1 - x0) / loc_wins
    b = [text(x0, 20, "The 35 crops where LoC is truly better than Qwen", 12, weight="bold")]
    for i, (name, acc, bal) in enumerate(rows):
        a = (acc * n - loc_wins * 2 * bal) / (qwen_wins - loc_wins)
        caught = loc_wins * (2 * bal - a)
        y = top + i * 44
        b.append(text(x0 - 8, y + 20, name, 12, "end"))
        b.append(f"<rect x='{x0}' y='{y + 4}' width='{loc_wins * sc:.1f}' height='24' fill='#e3e8f0'/>")
        b.append(f"<rect x='{x0}' y='{y + 4}' width='{caught * sc:.1f}' height='24' fill='{GREEN}'/>")
        b.append(text(x0 + caught * sc + 6, y + 21, f"about {caught:.0f} caught", 11, color=MUTED))
        b.append(text(x1 + 8, y + 21, f"{loc_wins - caught:.0f} missed", 11, color=MUTED))
    y = top + 44 * len(rows) + 14
    b.append(f"<rect x='{x0}' y='{y - 10}' width='12' height='12' fill='{GREEN}'/>")
    b.append(text(x0 + 18, y, "measure prefers LoC (correct)", 11, color=MUTED))
    b.append(f"<rect x='{x0 + 220}' y='{y - 10}' width='12' height='12' fill='#e3e8f0'/>")
    b.append(text(x0 + 238, y, "measure prefers Qwen (miss)", 11, color=MUTED))
    return svg(w, h, "".join(b))


if __name__ == "__main__":
    for name, fn in (("measures", measures), ("transfer", transfer), ("vlm-blind-spot", blind_spot)):
        (HERE / f"{name}.svg").write_text(fn())
