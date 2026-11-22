#!/usr/bin/env python3
"""Draws the English pilot's pages for Xiaomi-OCR-0 page mode (#1867, #1998).

  pilot_select.py --out-dir DIR (--dsn-file FILE | --input FILE)
      [--n 10000] [--seed x08-eng-pilot-1] [--floor 30] [--chunk 500]
      [--wf1415-plan FILE ...] [--on-disk FILE]

The pages are the worst tenth of English pages by qe-v1, leaving out those Tesseract's
worst-first wave has fixed, stratified by decade and damage band (scripts/onprem/quality/
pilot_selection.sql, which docs/boinc/apps/quality.md, "Why page mode, and how the pilot was chosen", describes).
It reads the database read-only (use the quality_reader DSN, which points at the replica).

The floor of 30 pages a stratum can push the draw past --n; the largest strata are trimmed back
one page at a time (ties to the later decade and band) until it is --n, and the trim is
recorded. Pages are listed so that any prefix keeps the strata in proportion.

Files, under --out-dir:

  draw.csv       the query's output, before the trim (--input replays one, copying it here)
  draw.meta.json the seed, n, floor and query checksum that made draw.csv; a replay carries the
                 original's across, and campaign.json says "unknown" (null) without it
  pilot.csv      the pages, in staging order, with the stratum and the flags
  members-NNN.json  the doc ids, --chunk to a file, for `usnm-boinc qwen stage --members`
  strata.json    doc_id -> decade, band, tess_read, wf1415, for `qwen stage --strata`
  campaign.json  the notes: seed, sizes, the query's checksum, the strata with pages, drawn and
                 kept, the trim, and with --on-disk the split of pages already on disk

--wf1415-plan marks the pages that #1867's first draw (wf1415-eng) also had, from plan.jsonl
files (one JSON object with a doc_id a line). --on-disk is a file of doc ids, one a line,
whose scans are already on disk; the rest are fetched through the paced locfetch path, which
the Compute Coordinator queues. Neither changes who is chosen.
"""
import argparse
import csv
import hashlib
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [HERE, os.path.join(HERE, "..", "ocr-feed")]
SQL_FILE = os.path.join(HERE, "..", "..", "..", "..", "scripts", "onprem", "quality", "pilot_selection.sql")
COLUMNS = ["doc_id", "decade", "band", "lm_oov", "damage_rate", "tess_read", "stratum_pages", "take", "k"]
OUT_COLUMNS = ["doc_id", "decade", "band", "lm_oov", "damage_rate", "tess_read", "wf1415", "stratum_pages", "take",
               "k", "kept"]
TIMEOUT = 3600


def read_sql(path):
    with open(path, "rb") as f:
        raw = f.read()
    return raw.decode(), hashlib.sha256(raw).hexdigest()


def psql(dsn_file, sql, out, variables):
    """Runs `sql` read-only with the DSN in libpq's environment, its output to the file `out`."""
    import quality_campaign
    with open(dsn_file) as f:
        dsn = f.read().strip()
    env = {**os.environ, **quality_campaign.dsn_env(dsn), "PGOPTIONS": "-c default_transaction_read_only=on",
           "PGAPPNAME": "usnm-pilot-select", "LC_ALL": "C.UTF-8"}
    args = ["psql", "-X", "-q", "-v", "ON_ERROR_STOP=1"]
    for k, v in variables.items():
        args += ["-v", f"{k}={v}"]
    p = subprocess.run(args, input=sql.encode(), stdout=out, stderr=subprocess.PIPE, env=env, timeout=TIMEOUT)
    if p.returncode:
        sys.exit(f"the query failed: {p.stderr.decode(errors='replace').strip()[-300:]}")


def meta_path(draw):
    return os.path.splitext(draw)[0] + ".meta.json"


def export(dsn_file, sql_file, path, n, seed, floor):
    sql, sha = read_sql(sql_file)
    tmp = path + ".tmp"
    with open(tmp, "wb") as out:
        psql(dsn_file, sql, out, {"n": n, "seed": seed, "floor": floor})
    os.replace(tmp, path)
    meta = {"seed": seed, "n": n, "floor": floor, "query_sha256": sha}
    write_atomic(meta_path(path), (json.dumps(meta, indent=1, sort_keys=True) + "\n").encode())
    return meta


def replay(src, dest):
    """Copies a saved draw and its metadata into the output directory, so draw.csv always
    belongs to the files written beside it. Returns the metadata, or None when it has none."""
    meta = None
    if os.path.exists(meta_path(src)):
        with open(meta_path(src)) as f:
            meta = json.load(f)
    same = os.path.exists(dest) and os.path.samefile(src, dest)
    if not same:
        with open(src, "rb") as f:
            write_atomic(dest, f.read())
    if meta is not None:
        if not (os.path.exists(meta_path(dest)) and os.path.samefile(meta_path(src), meta_path(dest))):
            write_atomic(meta_path(dest), (json.dumps(meta, indent=1, sort_keys=True) + "\n").encode())
    elif os.path.exists(meta_path(dest)):
        os.unlink(meta_path(dest))
    return meta


def read_draw(path):
    with open(path, newline="") as f:
        r = csv.reader(f)
        if next(r, None) != COLUMNS:
            sys.exit(f"{path}: expected the columns {','.join(COLUMNS)}")
        for doc, decade, band, lm_oov, damage, tess, pages, take, k in r:
            yield {"doc_id": doc, "decade": int(decade), "band": band, "lm_oov": float(lm_oov),
                   "damage_rate": float(damage), "tess_read": tess == "t", "stratum_pages": int(pages),
                   "take": int(take), "k": int(k)}


def trim(takes, n):
    """The takes per stratum cut to `n` in all, the largest first, one page at a time: ties go to
    the later (decade, band). Returns the new takes and how many pages each stratum lost."""
    takes = dict(takes)
    lost = {}
    excess = sum(takes.values()) - n
    while excess > 0:
        key = max(takes, key=lambda s: (takes[s], s))
        takes[key] -= 1
        lost[key] = lost.get(key, 0) + 1
        excess -= 1
    return takes, lost


def plan_docs(paths):
    docs = set()
    for path in paths:
        with open(path) as f:
            for line in f:
                if line.strip():
                    docs.add(json.loads(line)["doc_id"])
    return docs


def lines(path):
    with open(path) as f:
        return {line.strip() for line in f if line.strip()}


def select(rows, n, wf1415=frozenset()):
    """Trims the draw to `n` and orders it so that any prefix keeps the strata in proportion:
    by how far through its stratum's take a page is, then by stratum and k."""
    takes, pages = {}, {}
    for r in rows:
        takes[(r["decade"], r["band"])] = r["take"]
        pages[(r["decade"], r["band"])] = r["stratum_pages"]
    if sum(takes.values()) != len(rows):
        sys.exit("the draw has a stratum with other than its take in rows")
    kept, lost = trim(takes, n)
    out = []
    for r in rows:
        key = (r["decade"], r["band"])
        if r["k"] <= kept[key]:
            out.append({**r, "kept": kept[key], "wf1415": r["doc_id"] in wf1415})
    out.sort(key=lambda r: ((r["k"] - 0.5) / r["kept"], r["decade"], r["band"], r["k"]))
    return out, takes, kept, lost, pages


def write_atomic(path, data):
    tmp = path + ".tmp"
    with open(tmp, "wb") as f:
        f.write(data)
    os.replace(tmp, path)


def write_outputs(out_dir, picked, takes, kept, lost, pages, a, meta, on_disk):
    buf = []
    buf.append(",".join(OUT_COLUMNS))
    for r in picked:
        buf.append(",".join([r["doc_id"], str(r["decade"]), r["band"], repr(r["lm_oov"]), repr(r["damage_rate"]),
                             "t" if r["tess_read"] else "f", "t" if r["wf1415"] else "f", str(r["stratum_pages"]),
                             str(r["take"]), str(r["k"]), str(r["kept"])]))
    write_atomic(os.path.join(out_dir, "pilot.csv"), ("\n".join(buf) + "\n").encode())
    docs = [r["doc_id"] for r in picked]
    chunks = (len(docs) + a.chunk - 1) // a.chunk
    for i in range(0, len(docs), a.chunk):
        write_atomic(os.path.join(out_dir, f"members-{i // a.chunk + 1:03d}.json"),
                     (json.dumps(docs[i:i + a.chunk]) + "\n").encode())
    # A rerun with fewer chunks must not leave the old ones for a consumer that lists the directory.
    for name in os.listdir(out_dir):
        if name.startswith("members-") and name.endswith(".json") and name[8:-5].isdigit() \
                and int(name[8:-5]) > chunks:
            os.unlink(os.path.join(out_dir, name))
    strata = {r["doc_id"]: {"decade": r["decade"], "band": r["band"], "tess_read": r["tess_read"],
                            "wf1415": r["wf1415"]} for r in picked}
    write_atomic(os.path.join(out_dir, "strata.json"), (json.dumps(strata, indent=1, sort_keys=True) + "\n").encode())
    cells = [{"decade": d, "band": b, "pages": pages[(d, b)], "drawn": takes[(d, b)], "kept": kept[(d, b)],
              "trimmed": lost.get((d, b), 0)} for (d, b) in sorted(takes)]
    # What made the draw: the export's own values, or unknown for a replay without its metadata.
    made = meta or {}
    notes = {"seed": made.get("seed"), "drawn_with_n": made.get("n"), "floor": made.get("floor"),
             "query_sha256": made.get("query_sha256"), "n": a.n, "replayed": bool(a.input),
             "drawn": sum(takes.values()),
             "pages": len(picked), "trimmed": sum(lost.values()), "strata": cells,
             "tess_read": sum(r["tess_read"] for r in picked), "wf1415": sum(r["wf1415"] for r in picked),
             "members_files": chunks}
    if on_disk is not None:
        here = sum(r["doc_id"] in on_disk for r in picked)
        notes["on_disk"] = here
        notes["to_fetch"] = len(picked) - here
    write_atomic(os.path.join(out_dir, "campaign.json"), (json.dumps(notes, indent=1, sort_keys=True) + "\n").encode())
    print(json.dumps(notes, indent=1, sort_keys=True), flush=True)
    return notes


def parser():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter,
                                 allow_abbrev=False)
    ap.add_argument("--out-dir", required=True)
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--dsn-file", help="the quality_reader DSN (the replica); read with a read-only transaction")
    src.add_argument("--input", help="a draw.csv from an earlier run")
    ap.add_argument("--n", type=int, default=10000, help="pages in the pilot")
    ap.add_argument("--seed", default="x08-eng-pilot-1", help="the draw's seed; the same seed draws the same pages")
    ap.add_argument("--floor", type=int, default=30, help="fewest pages a stratum takes (quality_gain.py's interval)")
    ap.add_argument("--chunk", type=int, default=500, help="doc ids in a members file (qwen stage reads 500)")
    ap.add_argument("--wf1415-plan", action="append", default=[], help="a plan.jsonl of #1867's first draw")
    ap.add_argument("--on-disk", help="doc ids whose scans are on disk, one a line")
    return ap


def main(argv=None):
    ap = parser()
    a = ap.parse_args(argv)
    if not 1 <= a.n <= 1_000_000 or a.floor < 1 or not 1 <= a.chunk <= 500:
        ap.error("--n 1 to 1,000,000, --floor at least 1, --chunk 1 to 500")
    if not a.seed or not all(ch.isalnum() or ch in "-_." for ch in a.seed):
        ap.error("--seed: [A-Za-z0-9_.-]")
    os.makedirs(a.out_dir, exist_ok=True)
    draw = os.path.join(a.out_dir, "draw.csv")
    if a.dsn_file:
        meta = export(a.dsn_file, SQL_FILE, draw, a.n, a.seed, a.floor)
    else:
        meta = replay(a.input, draw)
    rows = list(read_draw(draw))
    if not rows:
        sys.exit("the query gave no pages")
    picked, takes, kept, lost, pages = select(rows, a.n, plan_docs(a.wf1415_plan))
    return write_outputs(a.out_dir, picked, takes, kept, lost, pages, a, meta,
                         lines(a.on_disk) if a.on_disk else None)


if __name__ == "__main__":
    main()
