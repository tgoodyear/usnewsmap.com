"""Make build_tasks.py's input for Serbian: LoC's ALTO against our BOINC Tesseract ALTO for the same page.

Serbian pages OCR'd on BOINC are held from publication until checked (#835); their ALTO is kept in
quarantine. This takes those layout files (--ours, one <doc id>.xml per page, fetched beforehand) and,
for each page in --pages, fetches LoC's ALTO and the page scan from loc.gov. It picks up to
--lines-per-page lines LoC's ALTO has as Cyrillic text (at least --min-words words), drawn at random
with a seed per page, so a rerun picks the same lines. Reading a is LoC's words with their boxes;
reading b is our words whose centres fall in the LoC line's box, left to right. A line where our text
isn't mostly Cyrillic (English text on a bilingual page) is left out.

  python3 serbian_lines.py --pages pages.jsonl --ours DIR --work DIR [--lines-per-page 4] \
    [--min-words 3] [--seed 1] [--delay 1.0]

--pages is JSON lines: {"doc_id": "sn85055101_1933-04-13_ed-1_seq-4", "page": "sn85055101/1933-04-13/ed-1/seq-4",
"alto": "<LoC ALTO URL on tile.loc.gov>", "title": "Amerikanski Srbobran", "scan": "<optional local scan>"}.
A page's "scan", relative to --scans, is read instead of fetching the scan from loc.gov (the BOINC run
already keeps them; LoC's image server takes about a minute to render a page). Writes DIR/lines.jsonl and
DIR/lines/<line id>.png, and caches downloads in DIR/loc/. Needs Pillow.
"""
import argparse
import hashlib
import json
import os
import random
import re

from yiddish_lines import alto_lines, fetch, iiif_full, label

CYRILLIC = re.compile(r"[Ѐ-ӿ]")
LETTER = re.compile(r"[^\W\d_]")


def cyrillic_share(text: str) -> float:
    letters = LETTER.findall(text)
    return sum(1 for ch in letters if CYRILLIC.match(ch)) / len(letters) if letters else 0.0


def candidates(lines: list, min_words: int) -> list:
    """Indexes of LoC lines worth checking: enough words, read as mostly Cyrillic letters."""
    return [i for i, ln in enumerate(lines)
            if len(ln["words"]) >= min_words
            and cyrillic_share(" ".join(w["text"] for w in ln["words"])) >= 0.6]


def words_in(box: list, words: list) -> list:
    """Words (in the same units as box) whose centres fall inside box, left to right."""
    x, y, w, h = box
    inside = [wd for wd in words
              if x <= wd["box"][0] + wd["box"][2] / 2 <= x + w and y <= wd["box"][1] + wd["box"][3] / 2 <= y + h]
    return sorted(inside, key=lambda wd: wd["box"][0])


def scaled_words(lines: list, sx: float, sy: float) -> list:
    return [{"text": wd["text"], "box": [wd["box"][0] * sx, wd["box"][1] * sy, wd["box"][2] * sx, wd["box"][3] * sy]}
            for ln in lines for wd in ln["words"]]


def pick(pool: list, k: int, doc: str, seed: int) -> list:
    rng = random.Random(int(hashlib.sha256(f"{seed}:{doc}".encode()).hexdigest()[:16], 16))
    return sorted(rng.sample(pool, min(k, len(pool))))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--pages", required=True)
    ap.add_argument("--ours", required=True, help="folder of our ALTO files, <doc id>.xml")
    ap.add_argument("--work", required=True, help="output and download cache folder")
    ap.add_argument("--scans", help="folder the pages' local scan paths are relative to")
    ap.add_argument("--lines-per-page", type=int, default=4)
    ap.add_argument("--min-words", type=int, default=3)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--delay", type=float, default=1.0, help="seconds to wait after each download")
    a = ap.parse_args()

    from PIL import Image

    # The largest scan in the first set is 43 million pixels; allow well above that, not unlimited.
    Image.MAX_IMAGE_PIXELS = 200_000_000
    os.makedirs(os.path.join(a.work, "lines"), exist_ok=True)
    n = 0
    with open(a.pages, encoding="utf-8") as pages, open(os.path.join(a.work, "lines.jsonl"), "w", encoding="utf-8") as out:
        for raw in pages:
            p = json.loads(raw)
            doc, page = p["doc_id"], p["page"]
            ours = os.path.join(a.ours, doc + ".xml")
            if not os.path.exists(ours):
                print(f"{doc}: no layout of ours, left out", flush=True)
                continue
            loc = os.path.join(a.work, "loc", doc + ".xml")
            tile = fetch(p["alto"], loc, a.delay)
            aw, ah, lines = alto_lines(loc)
            ow, oh, our_lines = alto_lines(ours)
            our_words = scaled_words(our_lines, aw / ow, ah / oh)
            chosen = []
            for li in pick(candidates(lines, a.min_words), a.lines_per_page, doc, a.seed):
                b_words = words_in(lines[li]["box"], our_words)
                b = " ".join(wd["text"] for wd in b_words)
                if b and cyrillic_share(b) >= 0.6:
                    chosen.append((li, b))
            if not chosen:
                print(f"{doc}: no Serbian lines to check", flush=True)
                continue
            local = os.path.join(a.scans, p["scan"]) if a.scans and p.get("scan") else None
            if not (local and os.path.exists(local)):
                local = os.path.join(a.work, "loc", doc + ".jpg")
                fetch(iiif_full(tile), local, a.delay)
            with Image.open(local) as im:
                scan = im.convert("L")
            kx, ky = scan.width / aw, scan.height / ah
            date, seq = page.split("/")[1], int(page.split("seq-")[1])
            for li, b in chosen:
                ln = lines[li]
                x, y, w, h = ln["box"]
                pad = h * 0.15
                x0, y0 = max(0, round((x - pad) * kx)), max(0, round((y - pad) * ky))
                x1, y1 = min(scan.width, round((x + w + pad) * kx)), min(scan.height, round((y + h + pad) * ky))
                line_id = f"{doc}_l{li}"
                rel = f"lines/{line_id}.png"
                scan.crop((x0, y0, x1, y1)).save(os.path.join(a.work, rel))
                words = [{"text": wd["text"], "box": [round(wd["box"][0] * kx) - x0, round(wd["box"][1] * ky) - y0,
                                                      round(wd["box"][2] * kx), round(wd["box"][3] * ky)]}
                         for wd in ln["words"]]
                out.write(json.dumps({
                    "id": line_id, "image": rel, "page": page,
                    "source": f"https://chroniclingamerica.loc.gov/lccn/{page}/",
                    "label": label(p["title"], date, seq),
                    "a": " ".join(wd["text"] for wd in words), "a_words": words, "b": b,
                }, ensure_ascii=False) + "\n")
                n += 1
            print(f"{doc}: {len(chosen)} lines", flush=True)
    print(f"{n} lines")


if __name__ == "__main__":
    main()
