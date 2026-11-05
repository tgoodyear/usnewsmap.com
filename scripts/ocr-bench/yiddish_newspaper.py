#!/usr/bin/env python3
"""Yiddish newspaper line test set (#1479, plan steps 3 and 4).

Draws held LoC Yiddish newspaper scans by title and decade, with seeded anchor points that
decide which lines a transcriber reads, so nobody picks the easy lines. Standard library only.

  inventory  list one language's held scans under a BOINC campaign's units root (read-only)
  select     draw pages per title x decade, with reserves and anchors: manifest.json
  template   one empty transcription record per drawn page: template.jsonl
  export     copy only the drawn scans, checked against the manifest, for a transcriber
  check      validate transcriptions against the manifest and diplomatic-v1

See ocr/bench/yiddish-newspaper/README.md for the procedure.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import unicodedata
from collections import Counter, defaultdict
from pathlib import Path

MANIFEST_FORMAT = "usnm-yiddish-newspaper-selection/1"
RECORD_FORMAT = "usnm-yiddish-newspaper-lines/1"
POLICY = "diplomatic-v1"
ANCHOR_MARGIN = 0.05
DOC_ID = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
STATUSES = ("reserve", "todo", "transcribed", "reviewed", "rejected")
ACTIVE = ("todo", "transcribed", "reviewed")
REJECT_REASONS = ("no-yiddish-text", "blank-or-non-text", "image-mismatch", "other")
SKIP_REASONS = ("non-text", "already-used", "not-yiddish", "unreadable")
REGIONS = ("body", "headline", "advertisement", "caption", "table", "other")
# Bidi controls change how text displays without being printed: LRM, RLM, ALM, the embeddings and
# overrides, and the isolates. References hold logical order only.
BIDI_CONTROLS = frozenset([0x200E, 0x200F, 0x061C, *range(0x202A, 0x202F), *range(0x2066, 0x206A)])
SOF_MARKERS = frozenset([*range(0xC0, 0xC4), *range(0xC5, 0xC8), *range(0xC9, 0xCC), *range(0xCD, 0xD0)])


def jpeg_size(data: bytes) -> tuple[int, int]:
    """Width and height from a JPEG's first start-of-frame segment."""
    if data[:2] != b"\xff\xd8":
        raise ValueError("not a JPEG (no SOI marker)")
    i = 2
    while i + 4 <= len(data):
        if data[i] != 0xFF:
            raise ValueError(f"bad JPEG marker at byte {i}")
        while i < len(data) and data[i] == 0xFF:
            i += 1
        if i >= len(data):
            break
        marker = data[i]
        i += 1
        if marker == 0x01 or 0xD0 <= marker <= 0xD7:
            continue
        if marker == 0xD8:
            raise ValueError(f"second SOI marker at byte {i - 1}")
        if marker in (0xD9, 0xDA):
            break
        length = int.from_bytes(data[i : i + 2], "big")
        if marker in SOF_MARKERS:
            if length < 7 or i + 7 > len(data):
                raise ValueError("truncated JPEG frame header")
            height = int.from_bytes(data[i + 3 : i + 5], "big")
            width = int.from_bytes(data[i + 5 : i + 7], "big")
            if not width or not height:
                raise ValueError("JPEG frame has no size")
            return width, height
        i += length
    raise ValueError("no JPEG start-of-frame segment")


def scan_facts(path: Path, chunk: int = 1 << 20) -> tuple[str, int, int, int]:
    """sha256, byte count, width and height of a JPEG, read in chunks.

    Only the head up to the frame header is kept in memory.
    """
    digest = hashlib.sha256()
    head = b""
    size = 0
    dims = None
    with path.open("rb") as f:
        while block := f.read(chunk):
            digest.update(block)
            size += len(block)
            if dims is None:
                head += block
                try:
                    dims = jpeg_size(head)
                except ValueError:
                    pass
    if dims is None:
        dims = jpeg_size(head)
    return digest.hexdigest(), size, *dims


def parse_doc_id(doc_id: str) -> dict:
    m = DOC_ID.match(doc_id)
    if not m:
        raise ValueError(f"unexpected page id {doc_id!r}")
    year = int(m["date"][:4])
    return {
        "lccn": m["lccn"],
        "date": m["date"],
        "edition": int(m["ed"]),
        "seq": int(m["seq"]),
        "decade": year - year % 10,
    }


def cmd_inventory(args) -> int:
    root = Path(args.units)
    counts: Counter = Counter()
    rows = []
    for name in sorted(os.listdir(root)):
        unit = root / name
        try:
            sample = json.loads((unit / "sample.json").read_text(encoding="utf-8"))
        except FileNotFoundError:
            counts["no_sample"] += 1
            continue
        if sample.get("lang") != args.lang:
            continue
        counts["language"] += 1
        page = unit / "page.jpg"
        if not page.is_file():
            counts["no_image"] += 1
            continue
        meta = parse_doc_id(name)
        if sample.get("decade") not in (None, meta["decade"]):
            print(f"warning: {name}: sample decade {sample['decade']} != page date decade", file=sys.stderr)
        sha256, size, width, height = scan_facts(page)
        rows.append(
            {
                "doc_id": name,
                **meta,
                "damage": sample.get("damage"),
                "sha256": sha256,
                "bytes": size,
                "width": width,
                "height": height,
            }
        )
    out = open(args.out, "w", encoding="utf-8") if args.out else sys.stdout
    try:
        for row in rows:
            out.write(json.dumps(row, sort_keys=True) + "\n")
    finally:
        if args.out:
            out.close()
    counts["held"] = len(rows)
    print(json.dumps(dict(sorted(counts.items()))), file=sys.stderr)
    return 0


def unit_interval(*parts) -> float:
    """A uniform number in [0, 1) from sha256, identical on every platform and Python version."""
    digest = hashlib.sha256("|".join(str(p) for p in parts).encode("utf-8")).digest()
    return int.from_bytes(digest[:8], "big") / 2**64


def anchors_for(seed: str, doc_id: str, width: int, height: int, count: int) -> list[list[int]]:
    span = 1 - 2 * ANCHOR_MARGIN
    return [
        [
            int(width * (ANCHOR_MARGIN + span * unit_interval(seed, doc_id, "anchor", i, "x"))),
            int(height * (ANCHOR_MARGIN + span * unit_interval(seed, doc_id, "anchor", i, "y"))),
        ]
        for i in range(count)
    ]


def rank_key(seed: str, doc_id: str) -> str:
    return hashlib.sha256(f"{seed}|{doc_id}".encode("utf-8")).hexdigest()


def order_cell(seed: str, rows: list[dict]) -> list[dict]:
    """Seeded order, taking one page per issue before a second page of any issue."""
    ranked = sorted(rows, key=lambda r: rank_key(seed, r["doc_id"]))
    seen, first, rest = set(), [], []
    for row in ranked:
        issue = (row["lccn"], row["date"], row["edition"])
        (rest if issue in seen else first).append(row)
        seen.add(issue)
    return first + rest


def load_titles(path: Path, lang: str) -> tuple[str | None, list[dict]]:
    doc = json.loads(path.read_text(encoding="utf-8"))
    titles = doc["titles"] if isinstance(doc, dict) else doc
    version = doc.get("index_version") if isinstance(doc, dict) else None
    keep = [t for t in titles if lang in (t.get("languages") or [])]
    if not keep:
        raise SystemExit(f"no titles list language {lang!r}")
    return version, sorted(keep, key=lambda t: t["lccn"])


def title_decades(title: dict) -> list[int]:
    first, last = int(title["first_page_date"][:4]), int(title["last_page_date"][:4])
    return list(range(first - first % 10, last - last % 10 + 1, 10))


def build_manifest(inventory: bytes, titles_doc: tuple[str | None, list[dict]], args) -> dict:
    index_version, titles = titles_doc
    rows = [json.loads(line) for line in inventory.decode("utf-8").splitlines() if line.strip()]
    lccns = {t["lccn"] for t in titles}
    held: dict[tuple[str, int], list[dict]] = defaultdict(list)
    outside = 0
    for row in rows:
        if row["lccn"] in lccns:
            held[(row["lccn"], row["decade"])].append(row)
        else:
            outside += 1
    expected = {(t["lccn"], d) for t in titles for d in title_decades(t)}
    cells, pages = [], []
    for lccn, decade in sorted(expected | set(held)):
        pool = held.get((lccn, decade), [])
        chosen = order_cell(args.seed, pool)[: args.pages_per_cell + args.reserves_per_cell]
        selected = min(len(chosen), args.pages_per_cell)
        status = "gap" if not pool else "ok" if len(pool) >= args.pages_per_cell else "short"
        cells.append(
            {
                "cell": f"{lccn}/{decade}",
                "lccn": lccn,
                "decade": decade,
                "in_title_range": (lccn, decade) in expected,
                "held": len(pool),
                "selected": selected,
                "reserves": len(chosen) - selected,
                "status": status,
            }
        )
        for rank, row in enumerate(chosen, 1):
            pages.append(
                {
                    "doc_id": row["doc_id"],
                    "cell": f"{lccn}/{decade}",
                    "rank": rank,
                    "role": "selected" if rank <= selected else "reserve",
                    "lccn": lccn,
                    "date": row["date"],
                    "edition": row["edition"],
                    "seq": row["seq"],
                    "damage": row.get("damage"),
                    "sha256": row["sha256"],
                    "bytes": row["bytes"],
                    "width": row["width"],
                    "height": row["height"],
                    "anchors": anchors_for(args.seed, row["doc_id"], row["width"], row["height"], args.anchors_per_page),
                }
            )
    n_selected = sum(c["selected"] for c in cells)
    return {
        "format": MANIFEST_FORMAT,
        "issue": 1479,
        "policy": POLICY,
        "language": args.lang,
        "seed": args.seed,
        "params": {
            "pages_per_cell": args.pages_per_cell,
            "reserves_per_cell": args.reserves_per_cell,
            "anchors_per_page": args.anchors_per_page,
            "blocks_per_page": args.blocks_per_page,
            "lines_per_block": args.lines_per_block,
            "anchor_margin": ANCHOR_MARGIN,
        },
        "source": {
            "campaign": args.campaign,
            "image_path": args.image_path,
            "inventory_sha256": hashlib.sha256(inventory).hexdigest(),
            "inventory_pages": len(rows),
            "pages_outside_titles": outside,
            "titles_index_version": index_version,
        },
        "titles": [
            {k: t.get(k) for k in ("lccn", "title", "place", "state", "first_page_date", "last_page_date", "pages")}
            for t in titles
        ],
        "cells": cells,
        "pages": pages,
        "totals": {
            "cells_expected": len(expected),
            "cells_held": sum(1 for c in cells if c["held"]),
            "gap_cells": [c["cell"] for c in cells if not c["held"]],
            "pages_selected": n_selected,
            "pages_reserve": len(pages) - n_selected,
            "target_lines": n_selected * args.blocks_per_page * args.lines_per_block,
        },
    }


def write_json(path: str, doc) -> None:
    Path(path).write_text(json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")


def cmd_select(args) -> int:
    inventory = Path(args.inventory).read_bytes()
    manifest = build_manifest(inventory, load_titles(Path(args.titles), args.lang), args)
    write_json(args.out, manifest)
    t = manifest["totals"]
    print(
        f"{t['pages_selected']} pages and {t['pages_reserve']} reserves in {t['cells_held']} of "
        f"{t['cells_expected']} cells; gaps {t['gap_cells']}; {t['target_lines']} target lines",
        file=sys.stderr,
    )
    return 0


def template_records(manifest: dict) -> list[dict]:
    return [
        {
            "format": RECORD_FORMAT,
            "doc_id": p["doc_id"],
            "cell": p["cell"],
            "rank": p["rank"],
            "role": p["role"],
            "image": {"sha256": p["sha256"], "width": p["width"], "height": p["height"]},
            "anchors": p["anchors"],
            "status": "todo" if p["role"] == "selected" else "reserve",
            "reject_reason": None,
            "policy": POLICY,
            "prefill": "none",
            "transcribed_by": None,
            "reviewed_by": None,
            "evidence": None,
            "blocks": [],
            "skipped_anchors": [],
            "short_reason": None,
            "notes": None,
        }
        for p in manifest["pages"]
    ]


def dump_records(records: list[dict]) -> str:
    return "".join(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n" for r in records)


def cmd_template(args) -> int:
    manifest = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    Path(args.out).write_text(dump_records(template_records(manifest)), encoding="utf-8")
    return 0


def cmd_export(args) -> int:
    manifest = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    for p in manifest["pages"]:
        data = (Path(args.units) / p["doc_id"] / "page.jpg").read_bytes()
        if hashlib.sha256(data).hexdigest() != p["sha256"]:
            raise SystemExit(f"{p['doc_id']}: held scan differs from the manifest's sha256")
        dest = out / f"{p['doc_id']}.jpg"
        if dest.exists() and hashlib.sha256(dest.read_bytes()).hexdigest() != p["sha256"]:
            raise SystemExit(f"{dest} exists with other content")
        tmp = dest.with_suffix(".part")
        tmp.write_bytes(data)
        os.replace(tmp, dest)
    shutil.copyfile(args.manifest, out / "manifest.json")
    print(f"{len(manifest['pages'])} scans in {out}", file=sys.stderr)
    return 0


def is_hebrew_letter(ch: str) -> bool:
    cp = ord(ch)
    return (0x05D0 <= cp <= 0x05F2 or 0xFB1D <= cp <= 0xFB4F) and unicodedata.category(ch).startswith("L")


def valid_box(box, width: int, height: int) -> bool:
    return (
        isinstance(box, list)
        and len(box) == 4
        and all(isinstance(v, int) and not isinstance(v, bool) for v in box)
        and 0 <= box[0] < box[2] <= width
        and 0 <= box[1] < box[3] <= height
    )


def overlap_share(a, b) -> float:
    w = min(a[2], b[2]) - max(a[0], b[0])
    h = min(a[3], b[3]) - max(a[1], b[1])
    if w <= 0 or h <= 0:
        return 0.0
    smaller = min((a[2] - a[0]) * (a[3] - a[1]), (b[2] - b[0]) * (b[3] - b[1]))
    return w * h / smaller


def nonempty(value) -> bool:
    return isinstance(value, str) and bool(value.strip())


def check_line(where: str, line: dict, width: int, height: int, errors: list, warnings: list) -> int:
    """Validate one line; return its visible (unmasked) code points."""
    if not valid_box(line.get("box"), width, height):
        errors.append(f"{where}: box must be four integers [x0, y0, x1, y1] inside the {width}x{height} image")
    text = line.get("text")
    if not nonempty(text):
        errors.append(f"{where}: text is empty")
        return 0
    bad = sorted({f"U+{ord(c):04X}" for c in text if ord(c) in BIDI_CONTROLS or unicodedata.category(c) == "Cc"})
    if bad:
        errors.append(f"{where}: text has control characters {', '.join(bad)}; store logical order only, one line")
    if not any(is_hebrew_letter(c) for c in text):
        warnings.append(f"{where}: no Hebrew-script letter; confirm the line is part of the Yiddish block")
    previous_end = masked = 0
    for k, span in enumerate(line.get("illegible_spans") or []):
        ok = (
            isinstance(span, dict)
            and all(isinstance(span.get(f), int) and not isinstance(span.get(f), bool) for f in ("start", "end"))
            and previous_end <= span["start"] < span["end"] <= len(text)
            and nonempty(span.get("reason"))
        )
        if not ok:
            errors.append(f"{where}: illegible span {k} must be sorted, nonoverlapping code-point offsets with a reason")
            return len(text)
        masked += span["end"] - span["start"]
        previous_end = span["end"]
    return len(text) - masked


def valid_column(column, width: int) -> bool:
    return (
        isinstance(column, list)
        and len(column) == 2
        and all(isinstance(v, int) and not isinstance(v, bool) for v in column)
        and 0 <= column[0] < column[1] <= width
    )


def check_blocks(where: str, rec: dict, page: dict, params: dict, errors: list, warnings: list) -> Counter:
    """Validate a record's blocks against the manifest page's geometry and anchors."""
    stats: Counter = Counter()
    width, height = page["width"], page["height"]
    anchors = page["anchors"]
    blocks = rec.get("blocks") or []
    skipped = rec.get("skipped_anchors") or []
    if not isinstance(blocks, list) or not isinstance(skipped, list):
        errors.append(f"{where}: blocks and skipped_anchors must be lists")
        return stats
    used: list[int] = []
    for s in skipped:
        if not isinstance(s, dict) or s.get("reason") not in SKIP_REASONS or not isinstance(s.get("anchor"), int):
            errors.append(f"{where}: skipped anchor {s!r} needs an anchor index and a reason in {SKIP_REASONS}")
            continue
        used.append(s["anchor"])
        stats[f"skip:{s['reason']}"] += 1
    block_anchors = [b.get("anchor") for b in blocks if isinstance(b, dict)]
    if len(block_anchors) != len(blocks) or not all(isinstance(a, int) for a in block_anchors):
        errors.append(f"{where}: every block needs an integer anchor index")
        return stats
    if block_anchors != sorted(set(block_anchors)):
        errors.append(f"{where}: blocks must follow their anchors in order")
    used += block_anchors
    if sorted(used) != list(range(len(used))) or any(a >= len(anchors) for a in used):
        errors.append(f"{where}: anchors must be taken in order from 0; each is either a block or skipped, once")
    elif blocks and len(blocks) == params["blocks_per_page"] and max(used) > block_anchors[-1]:
        errors.append(f"{where}: after the last block, leave later anchors unused; don't list them in skipped_anchors")
    if rec.get("status") in ("transcribed", "reviewed") and len(blocks) < params["blocks_per_page"]:
        if not (len(used) == len(anchors) and nonempty(rec.get("short_reason"))):
            errors.append(f"{where}: needs {params['blocks_per_page']} blocks, or every anchor used and a short_reason")
    if len(blocks) > params["blocks_per_page"]:
        errors.append(f"{where}: more than {params['blocks_per_page']} blocks")
    earlier: list[list[int]] = []
    for b_i, block in enumerate(blocks):
        bw = f"{where} block {b_i}"
        if block.get("region") not in REGIONS:
            errors.append(f"{bw}: region must be one of {REGIONS}")
        lines = block.get("lines") or []
        if not isinstance(lines, list) or not lines:
            errors.append(f"{bw}: no lines")
            continue
        if len(lines) > params["lines_per_block"]:
            errors.append(f"{bw}: more than {params['lines_per_block']} lines")
        elif len(lines) < params["lines_per_block"] and not nonempty(block.get("short_reason")):
            errors.append(f"{bw}: fewer than {params['lines_per_block']} lines needs a short_reason")
        for l_i, line in enumerate(lines):
            if not isinstance(line, dict):
                errors.append(f"{bw} line {l_i}: must be an object with box and text")
                continue
            stats["characters"] += check_line(f"{bw} line {l_i}", line, width, height, errors, warnings)
            stats["lines"] += 1
        column = block.get("column")
        if not valid_column(column, width):
            errors.append(f"{bw}: column must be two integers [x0, x1]: the column or region's extent, inside the scan")
        boxes = [ln.get("box") if isinstance(ln, dict) else None for ln in lines]
        if not all(valid_box(bx, width, height) for bx in boxes) or not valid_column(column, width):
            continue
        if not 0 <= block["anchor"] < len(anchors):
            continue
        ax, ay = anchors[block["anchor"]]
        if not column[0] <= ax <= column[1]:
            errors.append(f"{bw}: column {column} must contain anchor {block['anchor']} ({ax}, {ay})")
        if any(b[0] < column[0] or b[2] > column[1] for b in boxes):
            errors.append(f"{bw}: every line must lie within the block's column {column}")
        first = boxes[0]
        on_anchor = first[0] <= ax <= first[2] and first[1] <= ay <= first[3]
        if ay > first[3]:
            errors.append(f"{bw}: the first line must be the line at anchor {block['anchor']} ({ax}, {ay}) or the next below it")
        # A gap over a line's height (to the first line, or between lines) may hide a skipped line.
        gaps = [] if on_anchor else [first[1] - ay > first[3] - first[1]]
        gaps += [b[1] - a[3] > max(a[3] - a[1], b[3] - b[1]) for a, b in zip(boxes, boxes[1:])]
        if any(gaps):
            if nonempty(block.get("gap_reason")):
                stats["gaps"] += 1
                warnings.append(f"{bw}: gap over a line's height, given as: {block['gap_reason'].strip()}")
            else:
                errors.append(f"{bw}: a gap over a line's height after anchor {block['anchor']} or between lines needs a gap_reason")
        if any(b[1] <= a[1] for a, b in zip(boxes, boxes[1:])):
            errors.append(f"{bw}: lines must run top to bottom")
        if any(e[0] <= ax <= e[2] and e[1] <= ay <= e[3] for e in earlier):
            errors.append(f"{bw}: anchor {block['anchor']} falls on an earlier block's line; skip it as already-used")
        if any(overlap_share(bx, e) > 0.5 for i, bx in enumerate(boxes) for e in earlier + boxes[:i]):
            errors.append(f"{bw}: repeats a line")
        earlier += boxes
    stats["blocks"] += len(blocks)
    return stats


def check_records(manifest: dict, records: list[dict], images: Path | None = None, complete: bool = False):
    """Return (errors, warnings, per-cell summary) for transcription records against the manifest."""
    errors: list[str] = []
    warnings: list[str] = []
    params = manifest["params"]
    pages = {p["doc_id"]: p for p in manifest["pages"]}
    by_doc: dict[str, dict] = {}
    for n, rec in enumerate(records, 1):
        doc = rec.get("doc_id") if isinstance(rec, dict) else None
        if doc not in pages:
            errors.append(f"record {n}: {doc!r} is not a drawn page")
        elif doc in by_doc:
            errors.append(f"record {n}: {doc} appears twice")
        else:
            by_doc[doc] = rec
    for doc in pages:
        if doc not in by_doc:
            errors.append(f"{doc}: no record")
    summary: dict[str, Counter] = defaultdict(Counter)
    for doc, rec in by_doc.items():
        page = pages[doc]
        where = doc
        expected = template_records({"pages": [page]})[0]
        for field in ("format", "cell", "rank", "role", "image", "anchors", "policy"):
            if rec.get(field) != expected[field]:
                errors.append(f"{where}: {field} differs from the manifest")
        status = rec.get("status")
        if status not in STATUSES:
            errors.append(f"{where}: status must be one of {STATUSES}")
            continue
        cell = summary[page["cell"]]
        cell[f"status:{status}"] += 1
        if status == "reserve":
            if page["role"] != "reserve":
                errors.append(f"{where}: a selected page cannot be put back in reserve; reject it with a reason")
            if rec.get("blocks") or rec.get("skipped_anchors"):
                errors.append(f"{where}: a reserve page has no transcription")
            continue
        if status == "rejected":
            if rec.get("reject_reason") not in REJECT_REASONS:
                errors.append(f"{where}: reject_reason must be one of {REJECT_REASONS}")
            elif rec["reject_reason"] == "other" and not nonempty(rec.get("notes")):
                errors.append(f"{where}: reject_reason other needs notes")
            continue
        prefill = rec.get("prefill")
        if not nonempty(prefill):
            errors.append(f"{where}: prefill must be 'none' or name the engine whose output was corrected")
        elif prefill != "none":
            warnings.append(f"{where}: prefilled from {prefill}; its score is biased toward {prefill}")
            cell["prefilled"] += 1
        if status in ("transcribed", "reviewed") and not nonempty(rec.get("transcribed_by")):
            errors.append(f"{where}: transcribed_by is required")
        if status == "reviewed":
            if not nonempty(rec.get("reviewed_by")) or not nonempty(rec.get("evidence")):
                errors.append(f"{where}: reviewed needs reviewed_by and evidence")
            elif rec["reviewed_by"].strip() == (rec.get("transcribed_by") or "").strip():
                errors.append(f"{where}: the reviewer must not be the transcriber")
        cell.update(check_blocks(where, rec, page, params, errors, warnings))
        if images is not None:
            path = images / f"{doc}.jpg"
            if not path.is_file():
                errors.append(f"{where}: no image {path}")
            else:
                try:
                    sha256, _, width, height = scan_facts(path)
                except ValueError:
                    sha256 = width = height = None
                if (sha256, width, height) != (page["sha256"], page["width"], page["height"]):
                    errors.append(f"{where}: {path} is not the drawn scan")
    for c in manifest["cells"]:
        ranked = sorted((p for p in manifest["pages"] if p["cell"] == c["cell"]), key=lambda p: p["rank"])
        statuses = [by_doc[p["doc_id"]].get("status") for p in ranked if p["doc_id"] in by_doc]
        if "reserve" in statuses and any(s != "reserve" for s in statuses[statuses.index("reserve") :]):
            errors.append(f"{c['cell']}: reserves must be used in rank order")
        active = sum(s in ACTIVE for s in statuses)
        if active > params["pages_per_cell"]:
            errors.append(f"{c['cell']}: {active} active pages; at most {params['pages_per_cell']}, a reserve only replaces a rejected page")
        if complete:
            if active < params["pages_per_cell"] and "reserve" in statuses:
                errors.append(f"{c['cell']}: activate the next reserve to replace a rejected page")
            if any(s in ("todo", "transcribed") for s in statuses):
                errors.append(f"{c['cell']}: every active page must be reviewed")
    return errors, warnings, summary


def cmd_check(args) -> int:
    manifest = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    errors: list[str] = []
    records = []
    for n, line in enumerate(Path(args.records).read_text(encoding="utf-8").splitlines(), 1):
        if line.strip():
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError as e:
                errors.append(f"{args.records}:{n}: {e}")
    more, warnings, summary = check_records(manifest, records, Path(args.images) if args.images else None, args.require_complete)
    errors += more
    cols = ("status:todo", "status:transcribed", "status:reviewed", "status:rejected", "blocks", "lines", "characters", "gaps", "prefilled")
    print("cell\t" + "\t".join(c.split(":")[-1] for c in cols) + "\tskipped anchors")
    total: Counter = Counter()
    for cell in sorted(summary):
        s = summary[cell]
        total.update(s)
        skips = ",".join(f"{k[5:]}={v}" for k, v in sorted(s.items()) if k.startswith("skip:")) or "-"
        print(cell + "\t" + "\t".join(str(s[c]) for c in cols) + "\t" + skips)
    print("total\t" + "\t".join(str(total[c]) for c in cols))
    for w in warnings:
        print(f"warning: {w}", file=sys.stderr)
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    return 1 if errors else 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("inventory", help="list held scans (JSONL on stdout or --out)")
    p.add_argument("--units", required=True, help="campaign units root holding <doc_id>/sample.json and page.jpg")
    p.add_argument("--lang", default="yid")
    p.add_argument("--out")
    p.set_defaults(func=cmd_inventory)
    p = sub.add_parser("select", help="draw pages and anchors")
    p.add_argument("--inventory", required=True)
    p.add_argument("--titles", required=True, help="GET /api/v1/titles response")
    p.add_argument("--seed", required=True)
    p.add_argument("--lang", default="yid")
    p.add_argument("--campaign", default="qwen-nonenglish-v1")
    p.add_argument("--image-path", default="project/qwen/qwen-nonenglish-v1/units/<doc_id>/page.jpg")
    p.add_argument("--pages-per-cell", type=int, default=5)
    p.add_argument("--reserves-per-cell", type=int, default=3)
    p.add_argument("--anchors-per-page", type=int, default=12)
    p.add_argument("--blocks-per-page", type=int, default=3)
    p.add_argument("--lines-per-block", type=int, default=3)
    p.add_argument("--out", required=True)
    p.set_defaults(func=cmd_select)
    p = sub.add_parser("template", help="write empty transcription records")
    p.add_argument("--manifest", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(func=cmd_template)
    p = sub.add_parser("export", help="copy the drawn scans for a transcriber")
    p.add_argument("--units", required=True)
    p.add_argument("--manifest", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(func=cmd_export)
    p = sub.add_parser("check", help="validate transcription records")
    p.add_argument("--manifest", required=True)
    p.add_argument("--records", required=True)
    p.add_argument("--images", help="folder of <doc_id>.jpg to check against the manifest")
    p.add_argument("--require-complete", action="store_true", help="fail unless every cell is fully reviewed")
    p.set_defaults(func=cmd_check)
    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
