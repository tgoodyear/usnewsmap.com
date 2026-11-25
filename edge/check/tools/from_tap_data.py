"""Turn a glyph-check data file (the #1620 tap page's data.json) into check.usnewsmap.com tasks.

Writes OUT_DIR/clips/<lang>/<set>/... (JPEG/PNG files, served at /clips/<same path>), OUT_DIR/tasks.jsonl
for POST /admin/tasks, and OUT_DIR/language.json for PUT /admin/languages/<lang>. Needs Pillow for the look-alike check
(lookalike.py: a task's `lookalikes`, the letter pairs the site's Cherokee font draws almost alike); --no-lookalikes skips it.

  python3 from_tap_data.py --data data.json --lang chr --set phoenix-nt-v1 --language-config chr.json --out-dir OUT

The tap page's word pictures are LoC's word box with 6 pixels around it, cut from the page scan, so a word box that reaches
into the next printed line shows two lines. With --clip-band (#2122's rule, line_clip.py) such a word is cut again from the
scan (--scans) to its printed line, taken from its TextLine's word boxes in LoC's ALTO (--loc-alto), when that makes the
picture more than --clip-shrink shorter; those tasks get new word clip keys (--clip-suffix), since served clips are cached
as immutable. Ids, readings and the other pictures are unchanged.
"""
import argparse
import base64
import collections
import datetime
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # `python3 -I` leaves the script directory off the path
import line_clip  # noqa: E402

SYL = re.compile("[\u13a0-\u13f5]")
WORD_PAD = 6  # pixels around LoC's word box in the tap page's word pictures
MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]


def save(uri: str, path: str) -> str:
    head, data = uri.split(",", 1)
    ext = "png" if "image/png" in head else "jpg"
    path = f"{path}.{ext}"
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(base64.b64decode(data))
    return path


def label(page: str, title: str) -> str:
    """`sn83020866/1828-07-23/ed-1/seq-4` -> `Cherokee Phoenix, 23 July 1828, page 4`."""
    _, date, _, seq = page.split("/")
    d = datetime.date.fromisoformat(date)
    return f"{title}, {d.day} {MONTHS[d.month - 1]} {d.year}, page {seq.split('-')[1]}"


def clipped_word(d: dict, page: str, alto: dict, scans: str, shrink: float):
    """The word picture of decision `d` cut to its printed line from the page scan (Pillow image), or None when the
    rule leaves it as it is. The word is the decision's index among its TextLine's syllabary words, as the tap page counted.
    `alto` caches the parsed ALTO and the opened scans; alto["dir"] is --loc-alto."""
    import phoenix_lines
    from PIL import Image
    lccn, date, ed, seq = page.split("/")
    path = os.path.join(alto["dir"], lccn, *date.split("-"), ed, seq, "ocr.xml")
    if path not in alto:
        alto[path] = phoenix_lines.alto_lines(path)
    pw, ph, tls = alto[path]
    line_id = d["id"].split("~")[0].split("_", 4)[4]
    tl = next(t for t in tls if t["id"] == line_id)
    words = [w["box"] for w in tl["words"] if SYL.search(w["text"])]
    x, y, w, h = words[int(d["id"].rsplit("~", 1)[1])]
    if page not in alto:
        alto[page] = Image.open(os.path.join(scans, page.replace("/", "_") + ".jpg"))
    scan = alto[page]
    sx, sy = scan.size[0] / pw, scan.size[1] / ph
    top, bottom = y * sy - WORD_PAD, (y + h) * sy + WORD_PAD
    band = line_clip.printed_band(words, [x, y, w, h])
    band = band and (band[0] * sy, band[1] * sy, band[2] * sy)
    top, bottom, cut = line_clip.clip_span(top, bottom, band, shrink)
    if not cut:
        return None
    x0, x1 = max(0, int(x * sx) - WORD_PAD), min(scan.size[0], int((x + w) * sx) + WORD_PAD)
    return scan.convert("L").crop((x0, max(0, int(top)), x1, min(scan.size[1], math.ceil(bottom))))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--lang", required=True)
    ap.add_argument("--set", required=True)
    ap.add_argument("--language-config", required=True, help="name, intro, notes, title, drop_examples")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--examples-set", help="where the example pictures go (default: --set); a new name keeps cached old pictures from being served")
    ap.add_argument("--no-lookalikes", action="store_true", help="don't check for look-alike letters (no Pillow needed)")
    ap.add_argument("--font", action="append", help="SCRIPT=PATH: reference font for the look-alike check (default: the site's Cherokee font)")
    ap.add_argument("--clip-band", action="store_true", help="cut a word picture that reaches into another printed line to "
                    "its line (needs --loc-alto and --scans); such tasks get new word clip keys")
    ap.add_argument("--loc-alto", help="LoC's ALTO, unpacked: <lccn>/<yyyy>/<mm>/<dd>/ed-N/seq-N/ocr.xml")
    ap.add_argument("--scans", help="the page scans the tap data was cut from: <lccn>_<yyyy-mm-dd>_ed-N_seq-N.jpg")
    ap.add_argument("--clip-shrink", type=float, default=line_clip.SHRINK)
    ap.add_argument("--clip-suffix", default="_b", help="added to the word clip key of a cut task")
    a = ap.parse_args()
    if a.clip_band and not (a.loc_alto and a.scans):
        ap.error("--clip-band needs --loc-alto and --scans")
    alto = {"dir": a.loc_alto}
    clipped = 0

    lookalike = fonts = None
    if not a.no_lookalikes:
        import lookalike
        fonts = lookalike.Fonts(lookalike.parse_fonts(a.font))
    flagged = marked = 0

    data = json.load(open(a.data, encoding="utf-8"))
    os.makedirs(a.out_dir, exist_ok=True)
    cfg = json.load(open(a.language_config, encoding="utf-8"))
    root = os.path.join(a.out_dir, "clips")
    prefix = f"{a.lang}/{a.set}"
    rel = lambda p: os.path.relpath(p, root)

    n = 0
    with open(os.path.join(a.out_dir, "tasks.jsonl"), "w", encoding="utf-8") as out:
        for line in data["lines"]:
            stem = line["crop"].rsplit(".", 1)[0]
            line_key = rel(save(line["line"], os.path.join(root, prefix, "lines", stem)))
            for d in line["decisions"]:
                word_path = os.path.join(root, prefix, "words", d["id"].replace("~", "_w"))
                cut = clipped_word(d, line["page"], alto, a.scans, a.clip_shrink) if a.clip_band else None
                if cut is None:
                    word_key = rel(save(d["word"], word_path))
                else:
                    os.makedirs(os.path.dirname(word_path), exist_ok=True)
                    cut.save(f"{word_path}{a.clip_suffix}.jpg", quality=85)
                    word_key = rel(f"{word_path}{a.clip_suffix}.jpg")
                    clipped += 1
                alike, marks = ([], []) if fonts is None else lookalike.analyse(d["a"], d["b"], {}, fonts)
                flagged += bool(alike)
                marked += bool(marks)
                out.write(json.dumps({
                    "id": f"{prefix}/{d['id']}", "lang": a.lang, "set": a.set,
                    "page": line["page"], "source": f"https://chroniclingamerica.loc.gov/lccn/{line['page']}/",
                    "label": label(line["page"], cfg["title"]),
                    "line": line_key, "word": word_key, "box": d["box"],
                    "a": d["a"], "b": d["b"], "a_diff": d["a_diff"], "b_diff": d["b_diff"],
                    "a_is": "reference", "b_is": "loc",
                    **({"lookalikes": alike} if alike else {}),
                    **({"mark_only": marks} if marks else {}),
                }, ensure_ascii=False) + "\n")
                n += 1

    # Example clips of each letter, except letters whose clips were found to be wrong.
    drop = set(cfg.get("drop_examples", ""))
    examples = collections.defaultdict(list)
    for ch, uris in data["examples"].items():
        if ch in drop or len(uris) < cfg.get("min_examples", 3):
            continue
        for i, u in enumerate(uris[:3]):
            examples[ch].append(rel(save(u, os.path.join(root, a.lang, a.examples_set or a.set, "examples", f"{ord(ch):04x}_{i}"))))
    language = {k: v for k, v in cfg.items() if k not in ("drop_examples", "min_examples")}
    language["examples"] = examples
    json.dump(language, open(os.path.join(a.out_dir, "language.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"{n} tasks, {len(examples)} letters with examples, {flagged} with look-alike letters, {marked} differing only in marks")
    if a.clip_band:
        print(f"{clipped} word pictures cut to their printed line (new keys ending {a.clip_suffix})")
    if fonts is not None and lookalike.unshaped_warning(fonts):
        print(lookalike.unshaped_warning(fonts), file=sys.stderr)


if __name__ == "__main__":
    main()
