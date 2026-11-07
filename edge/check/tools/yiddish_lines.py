"""Make build_tasks.py's input for the Yiddish newspaper line set (#1479, ocr/bench/yiddish-newspaper/).

For each page drawn in the manifest, it fetches LoC's ALTO and the page scan from loc.gov, then picks lines
the way the set's transcription protocol does: at each anchor in order, the ALTO text line at that point
(or the next one below it in the same column) and the two lines after it in the same text block, until
the page has three blocks. Each line is cut from the scan, read with Tesseract (PSM 7) and the given
model, and written as one record: reading a is LoC's ALTO words with their boxes, reading b Tesseract's text.

  python3 yiddish_lines.py --manifest ocr/bench/yiddish-newspaper/manifest.json --work DIR \
    --tessdata DIR --model yidj2 [--roles selected] [--delay 1.0]

Writes DIR/lines.jsonl and DIR/lines/<line id>.png, and caches downloads in DIR/loc/. Needs Pillow and
the tesseract command.
"""
import argparse
import datetime
import json
import os
import re
import subprocess
import time
import urllib.request
import xml.etree.ElementTree as ET

MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
HEBREW = re.compile(r"[֐-׿יִ-ﭏ]")
UA = "usnewsmap-check/1.0 (+https://check.usnewsmap.com)"


def fetch(url: str, path: str, delay: float) -> str:
    """Download url to path once; return the final URL (loc.gov redirects to tile.loc.gov)."""
    meta = path + ".url"
    if os.path.exists(path) and os.path.exists(meta):
        return open(meta).read().strip()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    for attempt in range(4):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers={"user-agent": UA}), timeout=120) as r:
                data, final = r.read(), r.geturl()
            break
        except OSError:
            if attempt == 3:
                raise
            time.sleep(10 * (attempt + 1))
    with open(path + ".part", "wb") as f:
        f.write(data)
    os.replace(path + ".part", path)
    open(meta, "w").write(final)
    time.sleep(delay)
    return final


def label(title: str, date: str, seq: int) -> str:
    d = datetime.date.fromisoformat(date)
    return f"{title}, {d.day} {MONTHS[d.month - 1]} {d.year}, page {seq}"


def short_title(title: str) -> str:
    """`Der Morgen Zshurnal = the Jewish Morning Journal` -> `Der Morgen Zshurnal`."""
    return title.split(" = ")[0].split(", ")[0].strip()


def alto_lines(path: str) -> tuple:
    """(page width, page height, [lines]) in ALTO units; a line is {block, box, words: [{text, box}]}."""
    root = ET.parse(path).getroot()
    ns = root.tag.split("}")[0] + "}" if root.tag.startswith("{") else ""
    page = root.find(f".//{ns}Page")
    out = []
    for b, block in enumerate(root.iter(f"{ns}TextBlock")):
        for tl in block.iter(f"{ns}TextLine"):
            words = []
            for s in tl:
                kind = s.tag[len(ns):]
                if kind not in ("String", "HYP") or s.get("HPOS") is None:
                    continue
                box = [int(float(s.get(k, 0))) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")]  # LoC's HYP has no HEIGHT
                if kind == "String":
                    words.append({"text": s.get("CONTENT", ""), "box": box})
                elif words:  # a line-end hyphen belongs to the word before it, as printed
                    prev = words[-1]["box"]
                    box = [box[0], prev[1], box[2], prev[3]]  # within the word's own height
                    words[-1] = {"text": words[-1]["text"] + s.get("CONTENT", "-"), "box": union(prev, box)}
            box = [int(float(tl.get(k))) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")]
            out.append({"block": b, "box": box, "words": words})
    return int(float(page.get("WIDTH"))), int(float(page.get("HEIGHT"))), out


def union(p: list, q: list) -> list:
    x0, y0 = min(p[0], q[0]), min(p[1], q[1])
    return [x0, y0, max(p[0] + p[2], q[0] + q[2]) - x0, max(p[1] + p[3], q[1] + q[3]) - y0]


def pick_blocks(lines: list, anchors: list, blocks: int = 3, per_block: int = 3) -> list:
    """Indexes of the lines the protocol would transcribe, in block order (anchors in ALTO units)."""
    used, picked = set(), []
    for x, y in anchors:
        if len(picked) == blocks:
            break
        hit = None
        for i, ln in enumerate(lines):
            lx, ly, lw, lh = ln["box"]
            if lx <= x <= lx + lw and ly <= y <= ly + lh:
                hit = i
                break
        if hit is None:  # between lines: the next line below in the same column, within a line's height
            below = [(ln["box"][1], i) for i, ln in enumerate(lines)
                     if ln["box"][0] <= x <= ln["box"][0] + ln["box"][2] and 0 <= ln["box"][1] - y <= 1.5 * ln["box"][3]]
            hit = min(below)[1] if below else None
        if hit is None or hit in used or not HEBREW.search(" ".join(w["text"] for w in lines[hit]["words"])):
            continue
        block = [i for i in range(hit, len(lines)) if lines[i]["block"] == lines[hit]["block"]][:per_block]
        block = [i for i in block if i not in used]
        used.update(block)
        picked.append(block)
    return [i for b in picked for i in b]


def iiif_full(tile_url: str) -> str:
    """ALTO's tile.loc.gov URL -> the IIIF URL of the page scan at full size."""
    m = re.search(r"/storage-services/service/(.+)\.xml$", tile_url)
    if not m:
        raise ValueError(f"unexpected ALTO location {tile_url}")
    return f"https://tile.loc.gov/image-services/iiif/service:{m.group(1).replace('/', ':')}/full/full/0/default.jpg"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--work", required=True, help="output and download cache folder")
    ap.add_argument("--tessdata", required=True, help="folder holding <model>.traineddata")
    ap.add_argument("--model", required=True, help="Tesseract model name, e.g. yidj2")
    ap.add_argument("--roles", default="selected", help="comma-separated page roles to use: selected, reserve")
    ap.add_argument("--delay", type=float, default=1.0, help="seconds to wait after each download")
    a = ap.parse_args()

    from PIL import Image

    Image.MAX_IMAGE_PIXELS = None
    man = json.load(open(a.manifest, encoding="utf-8"))
    titles = {t["lccn"]: t["title"] for t in man["titles"]}
    roles = set(a.roles.split(","))
    os.makedirs(os.path.join(a.work, "lines"), exist_ok=True)
    n = 0
    with open(os.path.join(a.work, "lines.jsonl"), "w", encoding="utf-8") as out:
        for p in man["pages"]:
            if p["role"] not in roles:
                continue
            page = f"{p['lccn']}/{p['date']}/ed-{p['edition']}/seq-{p['seq']}"
            doc = p["doc_id"]
            tile = fetch(f"https://chroniclingamerica.loc.gov/lccn/{page}/ocr.xml", os.path.join(a.work, "loc", doc + ".xml"), a.delay)
            aw, ah, lines = alto_lines(os.path.join(a.work, "loc", doc + ".xml"))
            sx, sy = aw / p["width"], ah / p["height"]  # anchors are in pixels of the held (smaller) scan
            picked = pick_blocks(lines, [(x * sx, y * sy) for x, y in p["anchors"]])
            if not picked:
                print(f"{doc}: no Yiddish lines at the anchors")
                continue
            fetch(iiif_full(tile), os.path.join(a.work, "loc", doc + ".jpg"), a.delay)
            scan = Image.open(os.path.join(a.work, "loc", doc + ".jpg")).convert("L")
            kx, ky = scan.width / aw, scan.height / ah
            for li in picked:
                ln = lines[li]
                x, y, w, h = ln["box"]
                pad = h * 0.15
                x0, y0 = max(0, round((x - pad) * kx)), max(0, round((y - pad) * ky))
                x1, y1 = min(scan.width, round((x + w + pad) * kx)), min(scan.height, round((y + h + pad) * ky))
                line_id = f"{doc}_l{li}"
                rel = f"lines/{line_id}.png"
                scan.crop((x0, y0, x1, y1)).save(os.path.join(a.work, rel))
                r = subprocess.run(["tesseract", os.path.join(a.work, rel), "-", "--psm", "7", "-l", a.model, "--tessdata-dir", a.tessdata],
                                   capture_output=True, text=True)
                if r.returncode < 0:  # Tesseract 5.5 can die on a line (SIGFPE); leave that line out
                    print(f"{line_id}: tesseract died (signal {-r.returncode}), line left out", flush=True)
                    continue
                if r.returncode != 0:  # a setup error (no model, bad --tessdata) would fail every line
                    raise SystemExit(f"{line_id}: tesseract exited {r.returncode}: {r.stderr.strip()}")
                words = [{"text": wd["text"], "box": [round(wd["box"][0] * kx) - x0, round(wd["box"][1] * ky) - y0,
                                                      round(wd["box"][2] * kx), round(wd["box"][3] * ky)]}
                         for wd in ln["words"]]
                out.write(json.dumps({
                    "id": line_id, "image": rel, "page": page,
                    "source": f"https://chroniclingamerica.loc.gov/lccn/{page}/",
                    "label": label(short_title(titles.get(p["lccn"], p["lccn"])), p["date"], p["seq"]),
                    "a": " ".join(wd["text"] for wd in words), "a_words": words, "b": r.stdout.strip(),
                }, ensure_ascii=False) + "\n")
                n += 1
            print(f"{doc}: {len(picked)} lines", flush=True)
    print(f"{n} lines")


if __name__ == "__main__":
    main()
