#!/usr/bin/env python3
"""The OCR gold set: select pages, cut crops, validate transcriptions, score CER (#296).

Design and rules are in docs/ocr-gold-set.md. Every option is a named flag.

    gold.py select   --scores docs/reports/2026-10-ocr-engine-survey/data/scores.jsonl --out selection.jsonl
    gold.py select   --manifest SAMPLE/manifest-final.jsonl --out selection.jsonl [--target 60] [--seed 296]
    gold.py cut      --selection selection.jsonl --sample SAMPLE --out DIR [--work WORK]   (on a node)
    gold.py fetch    --manifest DIR/manifest.jsonl --out DIR/images                          (on a node)
    gold.py validate --crops DIR/crops.jsonl [--manifest DIR/manifest.jsonl --images DIR/images]
    gold.py score    --crops DIR/crops.jsonl [--hyp ENGINE=FILE.jsonl ...] [--hints] [--out DIR]

select  Seeded stratified choice of pages, one crop per page: the target is shared equally between the
        survey's groups (a group with fewer pages gives all of them and the rest goes to the others),
        and inside a group the picks cycle through the decades in a seeded order. With --scores the
        page's date, LCCN, edition and sequence come from its page_id; --manifest also carries the
        image and ALTO URLs and the image checksum, which `cut` and `fetch` need.
cut     Cuts one crop per selected page (the box is crops.pick_box on LoC's ALTO, seeded per page),
        writes the manifest (URLs, checksums, box) and a transcription template, crops.jsonl, with
        `transcription` null. Each engine's text for the box goes in `hints`, marked as not truth.
fetch   Re-creates the crops from the manifest: downloads each page image (paced as locfetch does),
        checks its sha256, cuts the box.
validate  Checks a filled-in crops.jsonl. Exit status 1 on any error.
score   CER and WER of hypotheses against the transcriptions (normalization: see NORMALIZATION).
        Hypotheses are `--hyp ENGINE=FILE.jsonl` (rows {"crop_id", "text"}), and/or `--hints`, which uses
        the engines' texts stored in crops.jsonl. Crops that fail validation are left out and counted.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import random
import re
import sys
import unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

PAGE_ID = re.compile(r"^(?P<lccn>[^_]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-?(?P<ed>\d+)_seq-?(?P<seq>\d+)$")
DEFAULT_TARGET = 60
DEFAULT_SEED = 296
HINT_NOTE = "HINT, NOT TRUTH: each engine's current text for this box. Do not copy it; read the image."
UNKNOWN = "[?]"
GUESS = re.compile(r"\[\?([^\[\]\n]+)\]")
MAX_UNKNOWN_SHARE = 0.02


def read_jsonl(p: Path) -> list[dict]:
    return [json.loads(x) for x in p.read_text(encoding="utf-8").splitlines() if x.strip()]


def write_jsonl(p: Path, rows: list[dict]) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")


# ---------------------------------------------------------------- select


def parse_page_id(pid: str) -> dict:
    m = PAGE_ID.match(pid)
    if not m:
        raise ValueError(f"unexpected page_id {pid!r}")
    return {"lccn": m["lccn"], "date": m["date"], "edition": int(m["ed"]), "seq": int(m["seq"])}


def loc_page_url(r: dict) -> str:
    return f"https://www.loc.gov/resource/{r['lccn']}/{r['date']}/ed-{r['edition']}/?sp={r['seq']}"


def sample_pages(scores: Path | None, manifest: Path | None) -> list[dict]:
    """One row per survey page: page_id, group, lang, lccn, date, edition, seq (+ URLs and checksum with a manifest)."""
    pages: dict[str, dict] = {}
    if manifest:
        for r in read_jsonl(manifest):
            if r.get("in_sample", True):
                pages[r["page_id"]] = {**parse_page_id(r["page_id"]), **{k: r.get(k) for k in (
                    "page_id", "group", "lang", "title", "source_label", "image_url", "image_sha256", "alto_url")}}
    else:
        for r in read_jsonl(scores):
            pid = r["page_id"]
            pages.setdefault(pid, {**parse_page_id(pid), "page_id": pid, "group": r["group"], "lang": r["lang"]})
    return sorted(pages.values(), key=lambda p: p["page_id"])


def quotas(sizes: dict[str, int], target: int) -> dict[str, int]:
    """Equal shares; a group smaller than its share gives all its pages and the rest is shared by the others."""
    left, todo, out = target, sorted(sizes), {}
    while todo:
        share = left // len(todo)
        small = [g for g in todo if sizes[g] <= share]
        if not small:
            extra = left - share * len(todo)
            for i, g in enumerate(todo):  # groups in name order take the remainder
                out[g] = share + (1 if i < extra else 0)
            break
        for g in small:
            out[g] = sizes[g]
            left -= sizes[g]
            todo.remove(g)
    return out


def decade(date: str) -> str:
    return f"{int(date[:4]) // 10 * 10}s"


def select(pages: list[dict], target: int, seed: int) -> list[dict]:
    by_group: dict[str, list[dict]] = {}
    for p in pages:
        by_group.setdefault(p["group"], []).append(p)
    q = quotas({g: len(ps) for g, ps in by_group.items()}, target)
    out = []
    for g in sorted(by_group):
        rnd = random.Random(f"{seed}:gold:{g}")
        eras: dict[str, list[dict]] = {}
        for p in by_group[g]:
            eras.setdefault(decade(p["date"]), []).append(p)
        for ps in eras.values():
            ps.sort(key=lambda p: p["page_id"])
            rnd.shuffle(ps)
        order = sorted(eras)
        rnd.shuffle(order)
        picked: list[dict] = []
        while len(picked) < q[g]:  # cycle through the eras, one page each time round
            for e in order:
                if eras[e] and len(picked) < q[g]:
                    picked.append(eras[e].pop())
        for p in sorted(picked, key=lambda p: p["page_id"]):
            out.append({**p, "era": decade(p["date"]), "stratum": f"{g}/{decade(p['date'])}"})
    return out


def cmd_select(a: argparse.Namespace) -> int:
    if bool(a.scores) == bool(a.manifest):
        print("give exactly one of --scores and --manifest", file=sys.stderr)
        return 2
    rows = select(sample_pages(a.scores, a.manifest), a.target, a.seed)
    write_jsonl(a.out, rows)
    tally: dict[str, int] = {}
    for r in rows:
        tally[r["group"]] = tally.get(r["group"], 0) + 1
    print(f"{len(rows)} pages selected (seed {a.seed}, target {a.target}): {tally}")
    return 0


# ---------------------------------------------------------------- cut and fetch


def sha256(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def encode_crop(img, box) -> bytes:
    import io

    buf = io.BytesIO()
    img.crop(tuple(box)).convert("L").save(buf, format="JPEG", quality=85)
    return buf.getvalue()


def cmd_cut(a: argparse.Namespace) -> int:
    import PIL
    from PIL import Image

    import crops

    Image.MAX_IMAGE_PIXELS = None
    sel = read_jsonl(a.selection)
    sample = {r["page_id"]: r for r in read_jsonl(a.sample / "manifest-final.jsonl")}
    results = []
    if a.work:
        for res in sorted((a.work / "results").glob("*.jsonl")):
            results += read_jsonl(res)
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "images").mkdir(exist_ok=True)
    manifest, template, counters = [], [], {}
    for s in sel:
        pid = s["page_id"]
        sm = sample.get(pid)
        if sm is None:
            print(f"skipped, not in the sample: {pid}", file=sys.stderr)
            continue
        img = Image.open(a.sample / "images" / f"{pid}.jpg")
        loc = crops.alto_words(a.sample / "alto" / f"{pid}.xml", img.width)
        box = crops.pick_box(loc, img.size, random.Random(f"{a.seed}:gold-box:{pid}"))
        if box is None:
            print(f"skipped, no text region of 600+ pixels: {pid}", file=sys.stderr)
            continue
        counters[s["group"]] = counters.get(s["group"], 0) + 1
        crop_id = f"{s['group']}-{counters[s['group']]:02d}"
        data = encode_crop(img, box)
        (a.out / "images" / f"{crop_id}.jpg").write_bytes(data)
        page_bytes = (a.sample / "images" / f"{pid}.jpg").read_bytes()
        row = {
            "crop_id": crop_id, "page_id": pid, "group": s["group"], "lang": s["lang"], "era": s["era"],
            "stratum": s["stratum"], "lccn": s["lccn"], "date": s["date"], "edition": s["edition"], "seq": s["seq"],
            "title": sm.get("title"), "box": list(box), "image": f"images/{crop_id}.jpg",
            "crop_sha256": sha256(data), "crop_bytes": len(data), "encoder": f"Pillow {PIL.__version__}",
            "page_url": loc_page_url(s), "page_image_url": sm["image_url"],
            "page_image_sha256": sm.get("image_sha256") or sha256(page_bytes), "alto_url": sm["alto_url"],
        }
        manifest.append(row)
        texts = crops.engine_texts(a.work, results, pid, loc, box, img.width)
        template.append({
            "crop_id": crop_id, "group": s["group"], "lang": s["lang"], "date": s["date"], "title": sm.get("title"),
            "image": row["image"],
            "hints": {"is_truth": False, "note": HINT_NOTE, "texts": dict(sorted(texts.items()))},
            "transcription": None, "transcriber": None, "reviewer": None, "hint_copy_reviewed": False,
            "notes": "",
        })
    write_jsonl(a.out / "manifest.jsonl", manifest)
    write_jsonl(a.out / "crops.jsonl", template)
    total = sum(r["crop_bytes"] for r in manifest)
    print(f"{len(manifest)} crops, {total / 1e6:.1f} MB "
          f"({'under' if total < 10e6 else 'over'} the 10 MB limit for committing the images)")
    return 0


def cmd_fetch(a: argparse.Namespace) -> int:
    import PIL
    from PIL import Image

    import locfetch

    Image.MAX_IMAGE_PIXELS = None
    import io

    a.out.mkdir(parents=True, exist_ok=True)
    bad = 0
    pages: dict[str, bytes] = {}
    for r in read_jsonl(a.manifest):
        dest = a.out / Path(r["image"]).name
        if dest.exists() and sha256(dest.read_bytes()) == r["crop_sha256"]:
            continue
        data = pages.get(r["page_id"]) or locfetch.fetch(r["page_image_url"], locfetch.TILE)
        pages = {r["page_id"]: data}  # one page at a time
        if sha256(data) != r["page_image_sha256"]:
            print(f"page image checksum differs: {r['page_id']} (LoC changed the file?)", file=sys.stderr)
            bad += 1
            continue
        crop = encode_crop(Image.open(io.BytesIO(data)), r["box"])
        dest.write_bytes(crop)
        if sha256(crop) != r["crop_sha256"]:
            # JPEG bytes can differ between Pillow versions; the page checksum above is what pins the pixels.
            print(f"crop bytes differ from the manifest's: {r['crop_id']} (encoder {PIL.__version__}, "
                  f"manifest {r.get('encoder')})", file=sys.stderr)
    return 1 if bad else 0


# ---------------------------------------------------------------- normalization and CER

# NORMALIZATION, applied to the transcription and to every hypothesis before comparing:
#  1. Unicode NFC. Nothing else is folded: case, punctuation, diacritics, long s and ligatures all count.
#  2. A hyphen at a line's end followed by a line break is removed and the line joined (the survey's
#     text does the same, altotext.py), so a word broken across lines counts as one word.
#  3. Line breaks and runs of whitespace become one space; the ends are stripped.
# In the transcription only: `[?x]` (a guessed character or characters) counts as x, and `[?]` (an
# unreadable character) is dropped. Crops with more than MAX_UNKNOWN_SHARE unknown characters fail validation.


def normalize(text: str, truth: bool = False) -> str:
    t = unicodedata.normalize("NFC", text)
    if truth:
        t = GUESS.sub(lambda m: m.group(1), t).replace(UNKNOWN, "")
    t = re.sub(r"-[ \t]*\r?\n[ \t]*", "", t)
    return re.sub(r"\s+", " ", t).strip()


def edit_distance(a: list | str, b: list | str) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]


def cer_wer(truth: str, hyp: str) -> dict:
    t, h = normalize(truth, truth=True), normalize(hyp)
    tw, hw = t.split(), h.split()
    return {"ref_chars": len(t), "char_edits": edit_distance(t, h), "ref_words": len(tw),
            "word_edits": edit_distance(tw, hw)}


# ---------------------------------------------------------------- validate


def unknown_share(text: str) -> float:
    n = len(text.replace(UNKNOWN, "").replace("\n", "")) or 1
    return (text.count(UNKNOWN) + len(GUESS.findall(text))) / n


def validate_rows(rows: list[dict], manifest: list[dict] | None = None, images: Path | None = None,
                  require_review: bool = True) -> tuple[list[str], list[str]]:
    errors, warnings = [], []
    seen: set[str] = set()
    by_id = {m["crop_id"]: m for m in manifest or []}
    for r in rows:
        cid = r.get("crop_id", "?")
        if cid in seen:
            errors.append(f"{cid}: duplicate crop_id")
        seen.add(cid)
        t = r.get("transcription")
        if not isinstance(t, str) or not t.strip():
            errors.append(f"{cid}: transcription is empty")
            continue
        if not r.get("transcriber"):
            errors.append(f"{cid}: no transcriber")
        if require_review and (not r.get("reviewer") or r.get("reviewer") == r.get("transcriber")):
            errors.append(f"{cid}: needs a second-pass reviewer other than the transcriber")
        stripped = GUESS.sub("", t).replace(UNKNOWN, "")
        if "[?" in stripped:
            errors.append(f"{cid}: unbalanced uncertain-character marker")
        if unknown_share(t) > MAX_UNKNOWN_SHARE:
            errors.append(f"{cid}: more than {MAX_UNKNOWN_SHARE:.0%} unknown characters; drop the crop")
        nt = normalize(t, truth=True)
        for engine, text in sorted(((r.get("hints") or {}).get("texts") or {}).items()):
            if text and normalize(text) == nt and not r.get("hint_copy_reviewed"):
                errors.append(f"{cid}: transcription equals {engine!r} hint verbatim; set hint_copy_reviewed "
                              "after checking the image, or re-read it")
            elif text and len(nt) >= 30 and not r.get("hint_copy_reviewed") \
                    and edit_distance(nt, normalize(text)) / len(nt) < 0.01:
                warnings.append(f"{cid}: within 1% of {engine!r} hint; check it was read from the image")
        m = by_id.get(cid)
        if manifest is not None and m is None:
            errors.append(f"{cid}: not in the manifest")
        if m is not None and images is not None:
            p = images / Path(m["image"]).name
            if not p.exists():
                errors.append(f"{cid}: image missing ({p.name})")
            elif sha256(p.read_bytes()) != m["crop_sha256"]:
                warnings.append(f"{cid}: image bytes differ from the manifest's checksum")
    if manifest is not None:
        for cid in sorted(set(by_id) - seen):
            errors.append(f"{cid}: in the manifest, not in crops.jsonl")
    return errors, warnings


def cmd_validate(a: argparse.Namespace) -> int:
    manifest = read_jsonl(a.manifest) if a.manifest else None
    errors, warnings = validate_rows(read_jsonl(a.crops), manifest, a.images, not a.allow_unreviewed)
    for w in warnings:
        print(f"warning: {w}")
    for e in errors:
        print(f"error: {e}")
    print(f"{len(errors)} errors, {len(warnings)} warnings")
    return 1 if errors else 0


# ---------------------------------------------------------------- score


def cmd_score(a: argparse.Namespace) -> int:
    rows = read_jsonl(a.crops)
    errors, _ = validate_rows(rows, require_review=not a.allow_unreviewed)
    bad = {e.split(":", 1)[0] for e in errors}
    hyps: dict[str, dict[str, str]] = {}
    for spec in a.hyp:
        name, _, path = spec.partition("=")
        if not path:
            print(f"--hyp wants ENGINE=FILE.jsonl, got {spec!r}", file=sys.stderr)
            return 2
        hyps[name] = {r["crop_id"]: r["text"] for r in read_jsonl(Path(path))}
    if a.hints:
        for r in rows:
            for engine, text in ((r.get("hints") or {}).get("texts") or {}).items():
                hyps.setdefault(engine, {})[r["crop_id"]] = text
    used = [r for r in rows if r["crop_id"] not in bad]
    print(f"{len(used)} of {len(rows)} crops usable; {len(rows) - len(used)} left out (failed validation)",
          file=sys.stderr)
    out_rows = []
    for engine in sorted(hyps):
        for r in used:
            if r["crop_id"] in hyps[engine]:
                out_rows.append({"engine": engine, "crop_id": r["crop_id"], "group": r["group"],
                                 **cer_wer(r["transcription"], hyps[engine][r["crop_id"]])})
    summary = []
    for engine in sorted(hyps):
        for group in [None, *sorted({r["group"] for r in used})]:
            rs = [x for x in out_rows if x["engine"] == engine and (group is None or x["group"] == group)]
            if not rs:
                continue
            c, w = sum(x["ref_chars"] for x in rs), sum(x["ref_words"] for x in rs)
            summary.append({"engine": engine, "group": group or "all", "crops": len(rs), "ref_chars": c,
                            "cer": round(sum(x["char_edits"] for x in rs) / c, 4) if c else None,
                            "wer": round(sum(x["word_edits"] for x in rs) / w, 4) if w else None})
    if a.out:
        a.out.mkdir(parents=True, exist_ok=True)
        for name, data in (("cer-by-crop.csv", out_rows), ("cer-summary.csv", summary)):
            if data:
                with (a.out / name).open("w", newline="") as f:
                    w = csv.DictWriter(f, fieldnames=list(data[0]))
                    w.writeheader()
                    w.writerows(data)
    print("| engine | group | crops | reference chars | CER | WER |\n|---|---|---|---|---|---|")
    for s in summary:
        print(f"| {s['engine']} | {s['group']} | {s['crops']} | {s['ref_chars']} | {s['cer']} | {s['wer']} |")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("select")
    s.add_argument("--scores", type=Path, help="the survey's data/scores.jsonl (page_id, group, lang)")
    s.add_argument("--manifest", type=Path, help="the sample's manifest-final.jsonl")
    s.add_argument("--out", type=Path, required=True)
    s.add_argument("--target", type=int, default=DEFAULT_TARGET)
    s.add_argument("--seed", type=int, default=DEFAULT_SEED)
    s.set_defaults(fn=cmd_select)

    c = sub.add_parser("cut")
    c.add_argument("--selection", type=Path, required=True)
    c.add_argument("--sample", type=Path, required=True)
    c.add_argument("--work", type=Path, help="survey work dir, for the engines' hint texts")
    c.add_argument("--out", type=Path, required=True)
    c.add_argument("--seed", type=int, default=DEFAULT_SEED)
    c.set_defaults(fn=cmd_cut)

    f = sub.add_parser("fetch")
    f.add_argument("--manifest", type=Path, required=True)
    f.add_argument("--out", type=Path, required=True)
    f.set_defaults(fn=cmd_fetch)

    v = sub.add_parser("validate")
    v.add_argument("--crops", type=Path, required=True)
    v.add_argument("--manifest", type=Path)
    v.add_argument("--images", type=Path)
    v.add_argument("--allow-unreviewed", action="store_true", help="skip the second-pass reviewer check")
    v.set_defaults(fn=cmd_validate)

    k = sub.add_parser("score")
    k.add_argument("--crops", type=Path, required=True)
    k.add_argument("--hyp", action="append", default=[], metavar="ENGINE=FILE.jsonl")
    k.add_argument("--hints", action="store_true", help="score the engine texts stored in crops.jsonl")
    k.add_argument("--allow-unreviewed", action="store_true")
    k.add_argument("--out", type=Path)
    k.set_defaults(fn=cmd_score)

    a = ap.parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
