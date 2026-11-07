#!/usr/bin/env python3
"""Build the frozen Cherokee OCR test set (#1620): Phoenix lines that reprint the New Testament.

The Cherokee Phoenix (1828-1834) printed scripture in the same translation tradition as the 1860
New Testament (Worcester and Boudinot). A Cherokee text block of LoC's OCR that reprints a passage
aligns to the NT text, and each of its lines then has a reference: the NT letters its own letters
align to. Aligning whole blocks, not single lines, keeps badly read lines inside a matched passage,
so the set isn't limited to lines LoC already reads well.

The references are NT-derived, not transcriptions of the page: where the 1828 printing differs from
the 1860 text, the reference is wrong for that line. A line is dropped as a likely wording difference
when it has a run of 3 or more inserted letters, or when an NT word in its span is missing entirely,
has under half its letters matched, or lacks its final letter (the suffixes -Ꭹ, -Ꮙ and -Ᏹ that the
two printings often differ by). Other single-letter differences stay, and the glyph-checked subset
settles them.

    cherokee_testset.py align --nt-text cnts.raw.txt --tar LOC_BATCH.tar.bz2 [--tar ...] --out aligned.jsonl
    cherokee_testset.py images --aligned aligned.jsonl --out-dir images [--reuse-dir DIR ...]
    cherokee_testset.py crops --aligned aligned.jsonl --images images --out-dir crops
    cherokee_testset.py sample --lines crops/lines.jsonl --per-tier 100 --seed 1620 --out glyph-check.jsonl
    cherokee_testset.py score --lines crops/lines.jsonl --pred NAME=preds.jsonl [--pred ...] [--tier silver]

Scores are character error on syllabary letters only (spaces, punctuation and digits ignored), summed
over lines: edit distance / reference letters. `--pred loc` scores LoC's own text.

All text here is syllabary letters (U+13A0-U+13F5) and single spaces; punctuation, digits and Latin
are not part of a reference.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import hashlib
import json
import os
import re
import shutil
import sys
import tarfile
import xml.etree.ElementTree as ET

SYLLABARY = re.compile("[Ꭰ-Ᏽ]+")
GRAM = 4
MAX_GRAM_HITS = 50  # a 4-gram found more often than this in the NT doesn't vote
BUCKET = 32  # diagonal votes are pooled in bands this many letters wide
WINDOW = 300  # NT letters either side of the voted diagonal given to the alignment
MATCH, MISMATCH, GAP = 2, -1, -2
MIN_LINE_LETTERS = 8
MAX_GAP_RUN = 2  # a longer run of inserted or deleted letters drops the line
MAX_LINE_DIFF = 0.5  # (substitutions + insertions + deletions) / line letters
MIN_WORD_MATCH = 0.5  # an NT word in the span with fewer of its letters matched is a wording difference


def nt_stream(text: str) -> tuple[str, set[int]]:
    """The NT's syllabary letters in reading order, and the letter offsets where a word starts.
    `pdftotext -raw` keeps the reading order; a word hyphenated across a line break is rejoined."""
    text = re.sub(r"-\s*\n\s*", "", text)
    letters, starts = [], set()
    n = 0
    for word in SYLLABARY.findall(text):
        starts.add(n)
        letters.append(word)
        n += len(word)
    return "".join(letters), starts


def spaced(nt: str, starts: set[int], a: int, b: int) -> str:
    """NT letters [a, b) with a space before each word start inside the span."""
    return "".join((" " if i in starts and i != a else "") + nt[i] for i in range(a, b))


def gram_index(nt: str) -> dict[str, list[int]]:
    index: dict[str, list[int]] = collections.defaultdict(list)
    for i in range(len(nt) - GRAM + 1):
        index[nt[i:i + GRAM]].append(i)
    return {g: p for g, p in index.items() if len(p) <= MAX_GRAM_HITS}


def best_diagonal(q: str, index: dict[str, list[int]]) -> tuple[int, int]:
    """The NT offset where `q` most likely starts, and the votes for it."""
    votes = collections.Counter()
    for i in range(len(q) - GRAM + 1):
        for j in index.get(q[i:i + GRAM], ()):
            votes[(j - i) // BUCKET] += 1
    if not votes:
        return 0, 0
    band, n = votes.most_common(1)[0]
    n += votes.get(band - 1, 0) + votes.get(band + 1, 0)
    return band * BUCKET, n


def local_align(q: str, t: str) -> list[tuple[int | None, int | None]]:
    """Smith-Waterman alignment of `q` against `t`: the aligned pairs (q index, t index), None for a
    gap, in order, covering the best-scoring local region only."""
    rows, cols = len(q) + 1, len(t) + 1
    h = [[0] * cols for _ in range(rows)]
    best, bi, bj = 0, 0, 0
    for i in range(1, rows):
        hi, hp, qc = h[i], h[i - 1], q[i - 1]
        for j in range(1, cols):
            s = max(0, hp[j - 1] + (MATCH if qc == t[j - 1] else MISMATCH), hp[j] + GAP, hi[j - 1] + GAP)
            hi[j] = s
            if s > best:
                best, bi, bj = s, i, j
    pairs = []
    i, j = bi, bj
    while i > 0 and j > 0 and h[i][j] > 0:
        s = h[i][j]
        if s == h[i - 1][j - 1] + (MATCH if q[i - 1] == t[j - 1] else MISMATCH):
            pairs.append((i - 1, j - 1))
            i, j = i - 1, j - 1
        elif s == h[i - 1][j] + GAP:
            pairs.append((i - 1, None))
            i -= 1
        else:
            pairs.append((None, j - 1))
            j -= 1
    pairs.reverse()
    return pairs


def page_blocks(xml: bytes):
    """(block id, [(line id, box, syllabary letters, LoC text)]) for each language="chr" block, and
    the ALTO page size."""
    root = ET.fromstring(xml)
    ns = root.tag.split("}")[0] + "}" if root.tag.startswith("{") else ""
    page = root.find(f".//{ns}Page")
    size = (int(float(page.get("WIDTH"))), int(float(page.get("HEIGHT")))) if page is not None else None
    blocks = []
    for block in root.iter(f"{ns}TextBlock"):
        if block.get("language") != "chr":
            continue
        lines = []
        for line in block.iter(f"{ns}TextLine"):
            words = [s.get("CONTENT") or "" for s in line.iter(f"{ns}String")]
            box = {k: int(float(line.get(k))) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")}
            lines.append((line.get("ID"), box, "".join(SYLLABARY.findall(" ".join(words))), " ".join(words)))
        blocks.append((block.get("ID"), lines))
    return blocks, size


def align_block(lines, nt, starts, index):
    """The candidate test lines of one block that reprints the NT, or [] if it doesn't."""
    q = "".join(letters for _, _, letters, _ in lines)
    if len(q) < 40:
        return []
    d, votes = best_diagonal(q, index)
    if votes < max(10, len(q) // 20):
        return []
    lo = max(0, d - WINDOW)
    pairs = local_align(q, nt[lo:d + len(q) + WINDOW])
    # For each block letter: its aligned NT offset (None if inserted), and the deletions before it.
    mapped: dict[int, int | None] = {}
    deleted_before: dict[int, int] = collections.Counter()
    nt_status: dict[int, str] = {}  # NT offset -> "match", "sub" or "del"
    pending = 0
    for qi, ti in pairs:
        if ti is not None:
            nt_status[lo + ti] = "del" if qi is None else ("match" if q[qi] == nt[lo + ti] else "sub")
        if qi is None:
            pending += 1
            continue
        mapped[qi] = None if ti is None else lo + ti
        deleted_before[qi] = pending
        pending = 0
    out, pos = [], 0
    for line_id, box, letters, loc_text in lines:
        s, e = pos, pos + len(letters)
        pos = e
        if len(letters) < MIN_LINE_LETTERS or any(i not in mapped for i in range(s, e)):
            continue
        subs = ins = dels = run = longest = 0
        for i in range(s, e):
            if i > s and deleted_before[i]:
                dels += deleted_before[i]
                longest = max(longest, deleted_before[i])
            if mapped[i] is None:
                ins += 1
                run += 1
                longest = max(longest, run)
            else:
                run = 0
                subs += nt[mapped[i]] != letters[i - s]
        targets = [mapped[i] for i in range(s, e) if mapped[i] is not None]
        if not targets:
            continue
        a, b = targets[0], targets[-1] + 1
        # NT letters deleted between this line and the next belong to this line's reference, so a
        # word the printing lacks at a line break is counted and checked, not lost from both lines.
        after = next((mapped[i] for i in range(e, len(q)) if mapped.get(i) is not None), None)
        if after is not None and after > b:
            dels += after - b
            longest = max(longest, after - b)
            b = after
        if longest > MAX_GAP_RUN:
            drop = "gap_run"
        elif (subs + ins + dels) / len(letters) > MAX_LINE_DIFF:
            drop = "too_different"
        elif wording_differs(nt, starts, nt_status, a, b):
            drop = "wording"
        else:
            drop = None
        out.append({"drop": drop,
            "line_id": line_id, "box": box, "loc_text": loc_text,
            "loc_letters": letters, "reference": spaced(nt, starts, a, b), "nt_offset": a,
            "substitutions": subs, "insertions": ins, "deletions": dels,
        })
    return out


_ORDERED: dict[int, list[int]] = {}


def _sorted_starts(starts: set[int]) -> list[int]:
    if id(starts) not in _ORDERED:
        _ORDERED.clear()
        _ORDERED[id(starts)] = sorted(starts)
    return _ORDERED[id(starts)]


def wording_differs(nt, starts, status, a, b) -> bool:
    """Whether an NT word overlapping [a, b) looks like a different wording rather than misread letters:
    missing entirely, mostly unmatched, or without its final letter. A word split by a line break is
    judged whole, from the letters the block alignment covers, so a misread letter in a short
    fragment doesn't count as a wording difference."""
    ordered = _sorted_starts(starts)
    i = a
    while i < b:
        k = bisect.bisect_right(ordered, i) - 1
        ws = ordered[k] if k >= 0 else 0
        we = ordered[k + 1] if k + 1 < len(ordered) else len(nt)
        st = [status[j] for j in range(ws, we) if j in status]
        matched = sum(x == "match" for x in st)
        if matched == 0 or matched / len(st) < MIN_WORD_MATCH:
            return True
        if we - 1 in status and len(st) > 1 and st[-1] == "del":
            return True
        i = we
    return False


def page_id(member: str) -> str:
    """`sn83020866/1828/03/06/ed-1/seq-1/ocr.xml` -> `sn83020866/1828-03-06/ed-1/seq-1`."""
    lccn, y, m, d, ed, seq = member.split("/")[:6]
    return f"{lccn}/{y}-{m}-{d}/{ed}/{seq}"


# LoC's batch for each title, as in the IIIF image paths.
BATCHES = {"sn83020866": "batch_dlc_misctopsn83020866_ver03", "sn83020874": "batch_dlc_misctopsn83020874_ver02"}


def image_url(page: str) -> str:
    """The full-resolution IIIF JPEG of a page id (`sn83020866/1828-03-06/ed-1/seq-4`)."""
    lccn, date, ed, seq = page.split("/")
    issue = date.replace("-", "") + f"{int(ed.split('-')[1]):02d}"
    return ("https://tile.loc.gov/image-services/iiif/service:ndnp:dlc:"
            f"{BATCHES[lccn]}:data:{lccn}:print:{issue}:{int(seq.split('-')[1]):04d}/full/full/0/default.jpg")


def image_name(page: str) -> str:
    return page.replace("/", "_") + ".jpg"


def sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def cmd_images(a) -> None:
    """Each page's image: copied from an earlier run's files when one has the same URL, fetched otherwise."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from locfetch import TILE, fetch  # noqa: E402  (paced, stops on a 403)

    pages = sorted({json.loads(line)["page"] for line in open(a.aligned, encoding="utf-8")})
    reuse = {}
    for d in a.reuse_dir or []:
        manifest = os.path.join(d, "manifest.json")
        if os.path.exists(manifest):
            for p in json.load(open(manifest, encoding="utf-8")).get("pages", []):
                if p.get("image_url"):
                    reuse[p["image_url"]] = os.path.join(d, "..", "images", p["image_file"])
    os.makedirs(a.out_dir, exist_ok=True)
    with open(os.path.join(a.out_dir, "images.jsonl"), "w", encoding="utf-8") as out:
        for page in pages:
            url, path = image_url(page), os.path.join(a.out_dir, image_name(page))
            if not os.path.exists(path):
                src = reuse.get(url)
                if src and os.path.exists(src):
                    shutil.copyfile(src, path)
                    how = "reused"
                else:
                    data = fetch(url, TILE)
                    with open(path + ".part", "wb") as f:
                        f.write(data)
                    os.replace(path + ".part", path)
                    how = "fetched"
            else:
                how = "kept"
            out.write(json.dumps({"page": page, "url": url, "file": image_name(page), "sha256": sha256(path),
                                  "bytes": os.path.getsize(path)}) + "\n")
            print(f"{how} {page}", file=sys.stderr)


def cmd_crops(a) -> None:
    """A PNG crop of each aligned line, with the padding and scaling cherokee_hybrid.py reads lines with,
    and lines.jsonl: one row per crop with its tier (`silver` for kept lines, `dropped-<reason>`)."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from PIL import Image  # noqa: E402

    import cherokee_hybrid  # noqa: E402

    images = {r["page"]: r for r in map(json.loads, open(os.path.join(a.images, "images.jsonl"), encoding="utf-8"))}
    rows = [json.loads(line) for line in open(a.aligned, encoding="utf-8")]
    os.makedirs(a.out_dir, exist_ok=True)
    by_page = collections.defaultdict(list)
    for r in rows:
        by_page[r["page"]].append(r)
    with open(os.path.join(a.out_dir, "lines.jsonl"), "w", encoding="utf-8") as out:
        for page, page_rows in sorted(by_page.items()):
            img = Image.open(os.path.join(a.images, images[page]["file"]))
            img.load()
            for r in page_rows:
                el = ET.Element("TextLine", {"ID": r["line_id"], **{k: str(v) for k, v in r["box"].items()}})
                box = cherokee_hybrid._pixel_box(el, tuple(r["alto_page_size"]), img.size,
                                                 cherokee_hybrid.PADDING_X, cherokee_hybrid.PADDING_Y)
                name = f"{page.replace('/', '_')}_{r['line_id']}.png"
                path = os.path.join(a.out_dir, name)
                img.crop(box).save(path)
                out.write(json.dumps({
                    "crop": name, "sha256": sha256(path), "page": page, "line_id": r["line_id"],
                    "pixel_box": box, "image_sha256": images[page]["sha256"],
                    "tier": "silver" if r["drop"] is None else f"dropped-{r['drop'].replace('_', '-')}",
                    "reference": r["reference"], "reference_source": "nt-1860-aligned",
                    "loc_text": r["loc_text"], "nt_offset": r["nt_offset"],
                }, ensure_ascii=False) + "\n")
    print(f"{len(rows):,} crops from {len(by_page)} pages", file=sys.stderr)


def cmd_sample(a) -> None:
    """The lines to check glyph by glyph: --per-tier from the silver lines and --per-tier from the
    dropped ones (split across drop reasons by their share), at most 4 per page per tier so no page
    dominates, chosen with --seed."""
    import random

    rows = [json.loads(line) for line in open(a.lines, encoding="utf-8")]
    rng = random.Random(a.seed)
    picked = []
    for tier_rows in ([r for r in rows if r["tier"] == "silver"], [r for r in rows if r["tier"] != "silver"]):
        rng.shuffle(tier_rows)
        per_page = collections.Counter()
        for r in tier_rows:
            if per_page[r["page"]] < 4:
                picked.append(r)
                per_page[r["page"]] += 1
            if sum(per_page.values()) == a.per_tier:
                break
    with open(a.out, "w", encoding="utf-8") as out:
        for r in sorted(picked, key=lambda r: (r["page"], r["line_id"])):
            out.write(json.dumps({"crop": r["crop"], "tier": r["tier"], "reference": r["reference"],
                                  "loc_text": r["loc_text"], "checked_text": None}, ensure_ascii=False) + "\n")
    print(f"{len(picked)} lines: {dict(collections.Counter(r['tier'] for r in picked))}", file=sys.stderr)


def letter_edit_distance(a: str, b: str) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def cmd_score(a) -> None:
    rows = [json.loads(line) for line in open(a.lines, encoding="utf-8")]
    if a.tier:
        rows = [r for r in rows if r["tier"] == a.tier]
    result = {"tier": a.tier or "all", "lines": len(rows)}
    for spec in a.pred:
        name, _, path = spec.partition("=")
        if name == "loc" and not path:
            preds = {r["crop"]: r["loc_text"] for r in rows}
        else:
            preds = {p["crop"]: p["text"] for p in map(json.loads, open(path, encoding="utf-8"))}
        err = ref = missing = 0
        for r in rows:
            gold = "".join(SYLLABARY.findall(r["reference"]))
            hyp = preds.get(r["crop"])
            missing += hyp is None
            err += letter_edit_distance(gold, "".join(SYLLABARY.findall(hyp or "")))
            ref += len(gold)
        result[name] = {"cer": round(err / ref, 4), "letters": ref, "missing": missing}
    print(json.dumps(result, ensure_ascii=False))


def cmd_align(a) -> None:
    nt, starts = nt_stream(open(a.nt_text, encoding="utf-8").read())
    index = gram_index(nt)
    print(f"NT: {len(nt):,} letters, {len(starts):,} words, {len(index):,} voting 4-grams", file=sys.stderr)
    n_lines = n_blocks = 0
    with open(a.out, "w", encoding="utf-8") as out:
        for t in a.tar:
            with tarfile.open(t, "r:bz2") as tf:
                for m in tf:
                    if not m.name.endswith(".xml"):
                        continue
                    blocks, size = page_blocks(tf.extractfile(m).read())
                    for block_id, lines in blocks:
                        found = align_block(lines, nt, starts, index)
                        n_blocks += bool(found)
                        for row in found:
                            n_lines += row["drop"] is None
                            out.write(json.dumps({"page": page_id(m.name), "alto_page_size": size,
                                                  "block_id": block_id, **row}, ensure_ascii=False) + "\n")
    print(f"{n_lines:,} kept lines from {n_blocks:,} blocks; dropped lines are written too, with a reason",
          file=sys.stderr)


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    al = sub.add_parser("align", help="align Cherokee blocks of LoC's ALTO to the NT text")
    al.add_argument("--nt-text", required=True, help="pdftotext -raw output of the 1860 NT")
    al.add_argument("--tar", action="append", required=True, help="a LoC OCR batch tar (.tar.bz2)")
    al.add_argument("--out", required=True, help="aligned lines, JSON lines; `drop` is null for kept lines")
    im = sub.add_parser("images", help="the page images the aligned lines need")
    im.add_argument("--aligned", required=True, help="align's output")
    im.add_argument("--out-dir", required=True)
    im.add_argument("--reuse-dir", action="append", help="an earlier run's directory with manifest.json "
                    "listing image_url and image_file (images in its ../images)")
    cr = sub.add_parser("crops", help="a crop of each aligned line, and lines.jsonl")
    cr.add_argument("--aligned", required=True)
    cr.add_argument("--images", required=True, help="images' --out-dir")
    cr.add_argument("--out-dir", required=True)
    sa = sub.add_parser("sample", help="the glyph-check sample")
    sa.add_argument("--lines", required=True, help="crops' lines.jsonl")
    sa.add_argument("--per-tier", type=int, default=100)
    sa.add_argument("--seed", type=int, default=1620)
    sa.add_argument("--out", required=True)
    sc = sub.add_parser("score", help="character error of predictions against the references")
    sc.add_argument("--lines", required=True)
    sc.add_argument("--pred", action="append", required=True,
                    help="NAME=path to JSON lines of {crop, text}; `loc` alone scores LoC's text")
    sc.add_argument("--tier", help="only this tier (silver, dropped-wording, ...)")
    a = p.parse_args(argv)
    {"align": cmd_align, "images": cmd_images, "crops": cmd_crops, "sample": cmd_sample,
     "score": cmd_score}[a.cmd](a)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
