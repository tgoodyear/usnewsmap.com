#!/usr/bin/env python3
"""The GHN silver lines of #2128, and the R4 candidate lines: S0's Cherokee lines aligned to the 1860 New
Testament by `cherokee_testset.py`'s `align_block`, with its drop rules unchanged.

`cherokee_testset.py align` takes the Cherokee blocks of LoC's ALTO. GHN pages have no ALTO, so a block
here is a run of consecutive Cherokee lines in Tesseract's reading order (block, paragraph, line numbers
of S0) inside one Tesseract block (a PSM 3 region), with a Latin line or a new region ending the run.
Each reading (stock `chr`, `chr_phoenix`) is grouped and aligned on its own; a line's letters are the
syllabary letters of that reading.

    silver.py align --classified classified.jsonl --pages pages.txt --reads NAME=reads.jsonl [--reads ...]
                    --nt-text cnts.raw.txt --out aligned.jsonl
    silver.py build --aligned aligned.jsonl --reads NAME=reads.jsonl [--reads ...] --prefer NAME
                    --crops-dir DIR --nt-text cnts.raw.txt --out-dir DIR [--copy-crops]

`build` keeps a line when the builder accepts it (drop is null) under any of the readings. Its reference
is the one from the --prefer reading when that reading accepts it, otherwise from the first reading
(in --reads order) that does; both readings' references are kept in the row. It writes:
  lines.jsonl      one row per kept line: line_id, page, tid, box, pixel_box, crop, crop_sha256,
                   reference, reference_from, accepted_by, references, nt_span [a, b) (letter offsets of
                   `nt_stream`), verses (ids from nt_verses.py)
  line-ids.txt     the sorted line ids; summary.json has its sha256
  verses.txt       the verse ids the kept lines cover, sorted
  summary.json     counts (lines, pages, letters, verses, per reading) and hashes
  crops/           the kept lines' crops (with --copy-crops)
"""
import argparse
import collections
import json
import shutil
from pathlib import Path

import common
import cherokee_testset as ct  # noqa: E402
import nt_verses  # noqa: E402


def tid_key(tid):
    return tuple(int(x) for x in tid.split("."))


def blocks_of(rec, text_of):
    """Runs of consecutive Cherokee lines in reading order inside one Tesseract block:
    (block id, [(line id, box, letters, text)])."""
    lines = sorted(rec["lines"], key=lambda l: tid_key(l["tid"]))
    out, run, run_block = [], [], None
    for l in lines:
        blk = l["tid"].split(".")[0]
        if not l.get("chr") or blk != run_block:
            if run:
                out.append(run)
            run, run_block = [], blk
        if l.get("chr"):
            lid = common.line_id(rec["page"], l["tid"])
            t = text_of.get(lid, "")
            run.append((lid, l["box"], "".join(ct.SYLLABARY.findall(t)), t))
    if run:
        out.append(run)
    return [(f"{rec['page']}#run{k}:{r[0][0].split('#')[1]}", r) for k, r in enumerate(out)]


def parse_reads(specs):
    out = {}
    for s in specs:
        name, _, path = s.partition("=")
        out[name] = {r["line_id"]: r for r in common.read_jsonl(path)}
    return out


def cmd_align(a):
    nt, starts = ct.nt_stream(open(a.nt_text, encoding="utf-8").read())
    index = ct.gram_index(nt)
    pages = set(common.read_pages(a.pages))
    reads = parse_reads(a.reads)
    recs = [r for r in common.read_jsonl(a.classified) if r["page"] in pages]
    counts = collections.Counter()
    with open(a.out, "w", encoding="utf-8") as out:
        for name, rd in reads.items():
            text_of = {k: v["text"] for k, v in rd.items()}
            for rec in recs:
                for block_id, lines in blocks_of(rec, text_of):
                    counts[name, "blocks"] += 1
                    found = ct.align_block(lines, nt, starts, index)
                    counts[name, "aligned_blocks"] += bool(found)
                    for row in found:
                        counts[name, "kept" if row["drop"] is None else "dropped"] += 1
                        row["nt_end"] = row["nt_offset"] + len(row["reference"].replace(" ", ""))
                        row["text"] = row.pop("loc_text")
                        row["letters"] = row.pop("loc_letters")
                        out.write(json.dumps({"reading": name, "page": rec["page"], "block_id": block_id, **row},
                                             ensure_ascii=False, sort_keys=True) + "\n")
    print(json.dumps({f"{k[0]}:{k[1]}": v for k, v in sorted(counts.items())}))


def cmd_build(a):
    text = open(a.nt_text, encoding="utf-8").read()
    vm = nt_verses.VerseMap(text)
    reads = parse_reads(a.reads)
    order = list(reads)
    rows = collections.defaultdict(dict)
    for r in common.read_jsonl(a.aligned):
        rows[r["line_id"]][r["reading"]] = r
    out_dir = Path(a.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    kept = []
    for lid, by in sorted(rows.items()):
        acc = [n for n in order if n in by and by[n]["drop"] is None]
        if not acc:
            continue
        src = a.prefer if a.prefer in acc else acc[0]
        r = by[src]
        base = next(reads[n][lid] for n in order if lid in reads[n])
        kept.append({
            "line_id": lid, "page": r["page"], "tid": lid.split("#")[1], "box": r["box"],
            "pixel_box": base["pixel_box"], "crop": base["crop"], "crop_sha256": base["crop_sha256"],
            "reference": r["reference"], "reference_from": src, "accepted_by": acc,
            "references": {n: by[n]["reference"] for n in order if n in by},
            "drops": {n: by[n]["drop"] for n in order if n in by},
            "nt_span": [r["nt_offset"], r["nt_end"]], "verses": vm.verses(r["nt_offset"], r["nt_end"]),
            "block_id": r["block_id"], "reference_source": "nt-1860-aligned",
        })
    common.write_jsonl(out_dir / "lines.jsonl", kept)
    ids = sorted(k["line_id"] for k in kept)
    (out_dir / "line-ids.txt").write_text("\n".join(ids) + "\n" if ids else "")
    verses = sorted({v for k in kept for v in k["verses"]}, key=lambda v: tuple(int(x) for x in v.split(".")))
    (out_dir / "verses.txt").write_text("\n".join(verses) + "\n" if verses else "")
    if a.copy_crops:
        for k in kept:
            dst = out_dir / "crops" / k["crop"]
            dst.parent.mkdir(parents=True, exist_ok=True)
            if not dst.exists():
                shutil.copy2(Path(a.crops_dir) / k["crop"], dst)
            assert common.sha256_file(dst) == k["crop_sha256"], k["crop"]
    letters = sum(len(k["reference"].replace(" ", "")) for k in kept)
    summary = {
        "lines": len(kept), "pages": len({k["page"] for k in kept}), "reference_letters": letters,
        "verses": len(verses), "line_ids_sha256": common.sha256_list(ids),
        "accepted_by": dict(collections.Counter("+".join(k["accepted_by"]) for k in kept)),
        "reference_from": dict(collections.Counter(k["reference_from"] for k in kept)),
        "references_differ": sum(len(set(v for n, v in k["references"].items() if k["drops"][n] is None)) > 1 for k in kept),
        "readings": order, "prefer": a.prefer,
        "nt_text_sha256": common.sha256_file(a.nt_text),
        "verse_map": {"verses_identified": sum(1 for v in vm.ids if not v.endswith(".0"))},
    }
    json.dump(summary, open(out_dir / "summary.json", "w"), indent=1, sort_keys=True)
    print(json.dumps(summary, indent=1, sort_keys=True))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    al = sub.add_parser("align")
    al.add_argument("--classified", required=True)
    al.add_argument("--pages", required=True)
    al.add_argument("--reads", action="append", required=True, help="NAME=reads.jsonl from read_lines.py")
    al.add_argument("--nt-text", required=True)
    al.add_argument("--out", required=True)
    al.set_defaults(fn=cmd_align)
    b = sub.add_parser("build")
    b.add_argument("--aligned", required=True)
    b.add_argument("--reads", action="append", required=True)
    b.add_argument("--prefer", required=True)
    b.add_argument("--crops-dir", required=True)
    b.add_argument("--nt-text", required=True)
    b.add_argument("--out-dir", required=True)
    b.add_argument("--copy-crops", action="store_true")
    b.set_defaults(fn=cmd_build)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
