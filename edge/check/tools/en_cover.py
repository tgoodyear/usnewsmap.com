"""Cover tasks for check.usnewsmap.com: does our OCR of a Chronicling America English page hold all of a block's print?

The completeness rule (crates/usnm-ocr-local/src/rule.rs) keeps LoC's text when ours is much shorter or shares too few
of LoC's words, because a short reading may have missed a column. LoC's own noise makes it long, so the rule holds many
pages that lost nothing, and tests on word bags or box areas can't tell the two apart (rule v2 and v3 both failed their
checks, 22 Nov 2026). These tasks ask people, block by block, whether our reading has the printed text, so a next rule
can be fitted and tested on labels.

Input (--pages) is JSON lines, one page each:

  {"page": "<lccn>/<date>/ed-N/seq-N", "source": "<page URL>", "date": "YYYY-MM-DD", "label": "<shown above the page>",
   "image": "<page image>", "loc_alto": "<LoC's ALTO>", "ours_alto": "<our ALTO of the same page>",
   "seed": "<why the page is in the set, e.g. known-miss, v3-held, random>"}

Paths are relative to --root (default: the folder of --pages). Each ALTO's page extent is mapped onto the image by
proportion, as en_pick.parse_alto does. Our ALTO must have a box per word: a page whose ALTO declares line, block or
page granularity (processingStepSettings) is left out and counted, since its words carry their block's box.

Blocks come from LoC's ALTO: each TextBlock, cut into chunks of at most --max-lines lines, is one candidate. A chunk
with fewer than --min-words LoC words is left out, and so is one that mostly overlaps a chunk already taken (LoC
sometimes stacks two readings of one region). A chunk's text is our words whose box centre falls in it, in our
reading order, a line break between our lines. `ratio` is our letters over LoC's letters in the chunk.

Per page it keeps every chunk with ratio below --low (the candidates for a missed block), then fills up to --per-page
with other chunks drawn with --seed, so most pages also carry chunks that should be answered Yes. Low chunks past
--per-page are kept too, up to --max-low.

A chunk where our reading has at most 3 words and LoC has at least 20 is missing by rule: no volunteer is asked, it is not a task, and it goes to OUT_DIR/rule_labelled.jsonl with outcome "missing" and reason "ours<=3 words, loc>=20". It does not count as a low chunk, so the page is filled from the others instead.
The page's `image` must be the held full-resolution scan, never a reduced derivative or a reduced decode (`im.reduce`): the block clips are cut from it at 1:1 (the page view alone is scaled to 800 px wide).
--clip-tag TAG writes clips as pages-TAG/ and blocks-TAG/, new keys for a rebuild (clips are cached as immutable); --only-ids FILE keeps only the chunks whose ids are listed, so a rebuild covers the loaded tasks and nothing else.

Writes OUT_DIR/clips/<lang>/<set>/{pages,blocks}/... (lang en-cover by default, its own queue and page, /en-cover), OUT_DIR/tasks.jsonl, OUT_DIR/language.json and OUT_DIR/report.json.
A task is {"kind": "cover", "text": <our text>, "block": <clip>, "pageview": <clip>, "box": <the block on the page,
as fractions>, "block_box": <the chunk in image pixels>} plus the usual id, page, source and label. The page answers
"a" for Yes (all there), "b" for No (some is missing), "notext" for no text in the box, "unsure" for can't tell, and
"badcrop" as on every task; the Worker refuses "neither".
`private.strata` is `decade` only, since the Worker exports strata before a task settles. `private.measure` holds
what would tell a volunteer which chunks the build thought were short (`ratio`, `ratio_band`, `seed`, `loc_words`,
`ours_words`): the Worker never serves or exports it, so the analysis joins it from this tasks.jsonl on the build host.

  python3 en_cover.py --pages pages.jsonl --set cover-v1 --language-config languages/en-cover.json --out-dir OUT \
    [--seed 1] [--per-page 16] [--low 0.5] [--max-low 24] [--max-lines 8]
"""
import argparse
import hashlib
import importlib.util
import json
import os
import re
import xml.etree.ElementTree as ET
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("build_tasks", os.path.join(HERE, "build_tasks.py"))
bt = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bt)

RULE_OURS_MAX = 3  # our reading of a chunk with at most this many words ...
RULE_LOC_MIN = 20  # ... where LoC has at least this many is missing by rule: no volunteer is asked
RULE_REASON = f"ours<={RULE_OURS_MAX} words, loc>={RULE_LOC_MIN}"
PAGEVIEW_WIDTH = 800
LETTER = re.compile(r"\w", re.UNICODE)


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _scale(root, image_size: tuple) -> tuple:
    page = next(e for e in root.iter() if _local(e.tag) == "Page")
    return image_size[0] / float(page.get("WIDTH")), image_size[1] / float(page.get("HEIGHT"))


def _line_words(line, sx: float, sy: float) -> list:
    words = []
    for el in line:
        tag = _local(el.tag)
        if tag == "String" and el.get("CONTENT"):
            x, y, w, h = (float(el.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
            words.append({"text": el.get("CONTENT"), "box": [x * sx, y * sy, w * sx, h * sy]})
        elif tag == "HYP" and words and el.get("CONTENT"):
            words[-1]["text"] += el.get("CONTENT")
    return words


def blocks(xml: bytes, image_size: tuple) -> list:
    """LoC's text blocks in ALTO order: a list of lines per block, each line a list of {text, box}."""
    root = ET.fromstring(xml)
    sx, sy = _scale(root, image_size)
    out = []
    for block in root.iter():
        if _local(block.tag) != "TextBlock":
            continue
        lines = [w for w in (_line_words(l, sx, sy) for l in block if _local(l.tag) == "TextLine") if w]
        if lines:
            out.append(lines)
    return out


class NotWordGeometry(ValueError):
    """Our ALTO gives its words the box of their line, block or page, so a word's centre says nothing about where it is."""


def granularity(root) -> str:
    """The geometry level stored ALTO declares (`granularity=` in processingStepSettings, as usnm-alto writes it), or
    "word" when none is declared (Tesseract's ALTO has a box per word)."""
    for el in root.iter():
        if _local(el.tag) == "processingStepSettings" and el.text:
            for line in el.text.splitlines():
                if line.strip().startswith("granularity="):
                    return line.strip().split("=", 1)[1].strip()
    return "word"


def words(xml: bytes, image_size: tuple) -> list:
    """Our words in reading order: {text, box, line}. Raises NotWordGeometry for ALTO whose word boxes are inherited
    from a line, block or page (crates/usnm-alto/src/boxes.rs): placing those words by centre would empty whole chunks."""
    root = ET.fromstring(xml)
    g = granularity(root)
    if g != "word":
        raise NotWordGeometry(f"granularity={g}")
    sx, sy = _scale(root, image_size)
    out, n = [], -1
    for line in root.iter():
        if _local(line.tag) != "TextLine":
            continue
        n += 1
        for w in _line_words(line, sx, sy):
            w["line"] = n
            out.append(w)
    return out


def union(boxes: list) -> list:
    """[x0, y0, x1, y1] around [x, y, w, h] boxes."""
    return [min(b[0] for b in boxes), min(b[1] for b in boxes),
            max(b[0] + b[2] for b in boxes), max(b[1] + b[3] for b in boxes)]


def iou(a: list, b: list) -> float:
    ix = max(0.0, min(a[2], b[2]) - max(a[0], b[0]))
    iy = max(0.0, min(a[3], b[3]) - max(a[1], b[1]))
    inter = ix * iy
    if inter == 0:
        return 0.0
    area = lambda r: (r[2] - r[0]) * (r[3] - r[1])  # noqa: E731
    return inter / (area(a) + area(b) - inter)


def letters(text: str) -> int:
    return len(LETTER.findall(text))


def text_in(ours: list, box: list) -> tuple:
    """Our words with their centre in box, as text with a line break between our lines, and how many there are."""
    x0, y0, x1, y1 = box
    inside = [w for w in ours if x0 <= w["box"][0] + w["box"][2] / 2 < x1 and y0 <= w["box"][1] + w["box"][3] / 2 < y1]
    lines, last = [], None
    for w in inside:
        if w["line"] != last:
            lines.append([])
            last = w["line"]
        lines[-1].append(w["text"])
    return "\n".join(" ".join(l) for l in lines), len(inside)


def ratio_band(r: float) -> str:
    return "none" if r == 0 else "low" if r < 0.5 else "mid" if r < 0.85 else "full"


def rank(seed: str, task_id: str) -> str:
    return hashlib.sha256(f"{seed}\n{task_id}".encode()).hexdigest()


def chunks(page: dict, loc_blocks: list, ours: list, args, stats: Counter) -> list:
    out, taken = [], []
    for bi, lines in enumerate(loc_blocks):
        for ci in range(0, len(lines), args.max_lines):
            part = [w for l in lines[ci:ci + args.max_lines] for w in l]
            stats["chunks"] += 1
            if len(part) < args.min_words:
                stats["left_out_few_words"] += 1
                continue
            box = union([w["box"] for w in part])
            if any(iou(box, t) > args.max_overlap for t in taken):
                stats["left_out_overlap"] += 1
                continue
            taken.append(box)
            loc_letters = sum(letters(w["text"]) for w in part)
            text, n = text_in(ours, box)
            r = round(letters(text) / loc_letters, 4) if loc_letters else 0.0
            out.append({"id": f"{args.lang}/{args.set}/{page['page']}~b{bi}c{ci // args.max_lines}", "box": box,
                        "text": text, "ratio": r, "loc_words": len(part), "ours_words": n})
    return out


def by_rule(c: dict) -> bool:
    """A chunk where we read almost nothing and LoC reads a lot: missing without asking anyone."""
    return c["ours_words"] <= RULE_OURS_MAX and c["loc_words"] >= RULE_LOC_MIN


def pick(cands: list, args) -> list:
    low = sorted((c for c in cands if c["ratio"] < args.low), key=lambda c: rank(args.seed, c["id"]))[:args.max_low]
    rest = sorted((c for c in cands if c["ratio"] >= args.low), key=lambda c: rank(args.seed, c["id"]))
    return sorted(low + rest[:max(0, args.per_page - len(low))], key=lambda c: c["id"])


def decade(date: str) -> int:
    return int(date[:4]) // 10 * 10


def write_page(page: dict, chosen: list, img, args, out_dir: str, f) -> None:
    W, H = img.size
    prefix = f"{args.lang}/{args.set}"
    stem = page["page"].replace("/", "_")
    tag = f"-{args.clip_tag}" if args.clip_tag else ""  # clips are cached as immutable, so a rebuilt clip needs a new key
    pv_key = f"{prefix}/pages{tag}/{stem}.jpg"
    os.makedirs(os.path.dirname(os.path.join(out_dir, "clips", pv_key)), exist_ok=True)
    pv = img.resize((PAGEVIEW_WIDTH, max(1, round(H * PAGEVIEW_WIDTH / W))))
    pv.save(os.path.join(out_dir, "clips", pv_key), quality=args.quality)
    for c in chosen:
        x0, y0, x1, y1 = c["box"]
        pad = max(6, int((y1 - y0) * 0.03))
        crop = img.crop((max(0, int(x0 - pad)), max(0, int(y0 - pad)), min(W, int(x1 + pad)), min(H, int(y1 + pad))))
        # No downscale: a block clip is the scan's own pixels (a cap at 1200 px once shrank full-width blocks by up to 4.6x).
        key = f"{prefix}/blocks{tag}/{c['id'].split('/', 2)[2].replace('/', '_').replace('~', '_')}.jpg"
        os.makedirs(os.path.dirname(os.path.join(out_dir, "clips", key)), exist_ok=True)
        crop.save(os.path.join(out_dir, "clips", key), quality=args.quality)
        f.write(json.dumps({
            "id": c["id"], "lang": args.lang, "set": args.set, "kind": "cover", "page": page["page"],
            "source": page.get("source"), "label": page.get("label"), "pageview": pv_key, "block": key,
            "box": [round(x0 / W, 4), round(y0 / H, 4), round((x1 - x0) / W, 4), round((y1 - y0) / H, 4)],
            "block_box": [round(v) for v in c["box"]], "text": c["text"],
            "private": {"strata": {"decade": decade(page["date"])},
                        "measure": {"ratio": c["ratio"], "ratio_band": ratio_band(c["ratio"]), "seed": page.get("seed"),
                                    "loc_words": c["loc_words"], "ours_words": c["ours_words"]}},
        }, ensure_ascii=False) + "\n")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--pages", required=True)
    ap.add_argument("--root", help="folder the pages' paths are relative to (default: the folder of --pages)")
    ap.add_argument("--lang", default="en-cover")
    ap.add_argument("--set", required=True)
    ap.add_argument("--language-config", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--seed", default="1", help="seeds the sampling (public)")
    ap.add_argument("--per-page", type=int, default=16)
    ap.add_argument("--low", type=float, default=0.5, help="a chunk whose ratio is below this is always kept")
    ap.add_argument("--max-low", type=int, default=24, help="at most this many low chunks per page")
    ap.add_argument("--max-lines", type=int, default=8)
    ap.add_argument("--min-words", type=int, default=6)
    ap.add_argument("--max-overlap", type=float, default=0.6, help="a chunk overlapping a taken one more than this is left out")
    ap.add_argument("--no-clips", action="store_true", help="count only: write the report and no tasks or images")
    ap.add_argument("--quality", type=int, default=85)
    ap.add_argument("--clip-tag", default="", help="clips go in pages-TAG/ and blocks-TAG/ (new keys for a rebuild)")
    ap.add_argument("--only-ids", help="a file of task ids, one per line: keep only those chunks (a rebuild of a loaded set)")
    args = ap.parse_args()

    from PIL import Image
    Image.MAX_IMAGE_PIXELS = 400_000_000  # newspaper scans are large
    root = args.root or os.path.dirname(os.path.abspath(args.pages))
    os.makedirs(args.out_dir, exist_ok=True)
    stats, bands, per_page = Counter(), Counter(), {}
    pages = [json.loads(l) for l in open(args.pages, encoding="utf-8") if l.strip()]
    only = set(l.strip() for l in open(args.only_ids, encoding="utf-8") if l.strip()) if args.only_ids is not None else None
    f = None if args.no_clips else open(os.path.join(args.out_dir, "tasks.jsonl"), "w", encoding="utf-8")
    rl = None if args.no_clips else open(os.path.join(args.out_dir, "rule_labelled.jsonl"), "w", encoding="utf-8")
    try:
        for page in pages:
            stats["pages"] += 1
            img = Image.open(os.path.join(root, page["image"])).convert("L")
            loc = blocks(open(os.path.join(root, page["loc_alto"]), "rb").read(), img.size)
            try:
                ours = words(open(os.path.join(root, page["ours_alto"]), "rb").read(), img.size)
            except NotWordGeometry as e:
                stats["left_out_pages_not_word_geometry"] += 1
                per_page[page["page"]] = {"seed": page.get("seed"), "left_out": str(e)}
                continue
            cands = chunks(page, loc, ours, args, stats)
            ruled = [c for c in cands if by_rule(c)]
            cands = [c for c in cands if not by_rule(c)]
            if only is not None:
                ruled = [c for c in ruled if c["id"] in only]
            stats["rule_labelled_missing"] += len(ruled)
            if f:
                for c in ruled:
                    rl.write(json.dumps({"id": c["id"], "page": page["page"], "outcome": "missing", "reason": RULE_REASON,
                                         "block_box": [round(v) for v in c["box"]], "loc_words": c["loc_words"],
                                         "ours_words": c["ours_words"], "text": c["text"]}, ensure_ascii=False) + "\n")
            chosen = [c for c in cands if c["id"] in only] if only is not None else pick(cands, args)
            stats["tasks"] += len(chosen)
            bands.update(ratio_band(c["ratio"]) for c in chosen)
            per_page[page["page"]] = {"seed": page.get("seed"), "chunks": len(cands), "tasks": len(chosen),
                                      "low": sum(c["ratio"] < args.low for c in cands)}
            if f:
                write_page(page, chosen, img, args, args.out_dir, f)
    finally:
        if f:
            f.close()
            rl.close()
    if not args.no_clips:
        cfg = json.load(open(args.language_config, encoding="utf-8"))
        language = {k: v for k, v in cfg.items() if k not in bt.BUILD_ONLY}
        language["examples"] = {}
        with open(os.path.join(args.out_dir, "language.json"), "w", encoding="utf-8") as lf:
            json.dump(language, lf, ensure_ascii=False, indent=1)
    report = dict(sorted(stats.items()))
    report["tasks_by_ratio_band"] = dict(sorted(bands.items()))
    report["pages_detail"] = per_page
    with open(os.path.join(args.out_dir, "report.json"), "w", encoding="utf-8") as rf:
        json.dump(report, rf, indent=1)
    print(json.dumps({k: v for k, v in report.items() if k != "pages_detail"}, indent=1))


if __name__ == "__main__":
    main()
