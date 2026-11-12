#!/usr/bin/env python3
"""Lists the scans whose copy is confirmed on the storage host, for the hand-off retention step.

The BOINC host (CT 230) never reads /tank/ocr, and the scan server's allow list does not
include it, so it cannot ask whether a scan it holds a second copy of still has its original.
This runs on the storage host, which sees /tank/ocr/scans (keep/ and buffer/) and
/cold/ocr-scans (where scan-cold-mover moves buffer batches, leaving symlinks), and writes the
sha256 of every scan it can confirm to a file the BOINC host can read. handoff_retention.py
prune-bundles frees a bundle's held `pages` only when each scan in it is in that file (rule of
8 November 2026: no scan is ever deleted; derived copies go once unused).

  scan_confirm.py publish --index FILE --root DIR [--root DIR ...] --out FILE

A fetch-index row (index-full.jsonl: doc_id, batch, bytes, sha256, format) is confirmed when,
under some --root, keep/ or buffer/ holds <batch>/<lccn>/<date>/<ed>/<seq>.<jp2|jpg> (a
symlinked batch directory is followed) whose size is the row's bytes and whose `.json` sidecar
names the row's sha256. The scan is not read: its sidecar is the fetcher's record of its
digest. A scan the index lists that is not found (a buffer scan deleted before 8 November) is
left out, and the run reports how many.

--out is written through a temporary name and renamed: a first line `# {json}` (version,
generated time, the index's size and row count, confirmed and missing counts), then the
confirmed sha256 values, sorted, one per line. It reads only and writes only --out.
"""
import argparse
import json
import os
import re
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor

VERSION = 1
SHA256 = re.compile(r"[0-9a-f]{64}")
DOC_ID = re.compile(r"(?P<lccn>[A-Za-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)")
SAFE = re.compile(r"[A-Za-z0-9_-]+")
SETS = ("keep", "buffer")
FIELDS = ("doc_id", "batch", "format", "sha256", "bytes")


def candidates(row, roots):
    """The paths the row's scan can be at: each root, each set, the row's format first."""
    m = DOC_ID.fullmatch(str(row.get("doc_id")))
    batch = row.get("batch")
    if not m or not isinstance(batch, str) or not SAFE.fullmatch(batch):
        return
    exts = ("jp2", "jpg") if row.get("format") == "jp2" else ("jpg", "jp2")
    for root in roots:
        for set_ in SETS:
            for ext in exts:
                yield os.path.join(root, set_, batch, m["lccn"], m["date"], m["ed"], f"{m['seq']}.{ext}")


def confirmed(row, roots):
    """True when a copy of the row's scan is under a root, with its size and recorded digest."""
    sha, size = row.get("sha256"), row.get("bytes")
    if not isinstance(sha, str) or not SHA256.fullmatch(sha) or not isinstance(size, int):
        return False
    for path in candidates(row, roots):
        try:
            if os.stat(path).st_size != size:
                continue
            with open(path + ".json", "rb") as f:
                sidecar = json.load(f)
        except (OSError, ValueError):
            continue
        if isinstance(sidecar, dict) and sidecar.get("sha256") == sha:
            return True
    return False


def publish(a):
    roots = [os.path.realpath(r) for r in a.root]
    for r in roots:
        if not os.path.isdir(os.path.join(r, "keep")) and not os.path.isdir(os.path.join(r, "buffer")):
            sys.exit(f"error: {r} holds neither keep/ nor buffer/")
    st = os.stat(a.index)
    rows = []
    with open(a.index, "rb") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                # The fetcher may be mid-append: a torn last line is not a row.
                continue
            if isinstance(row, dict):
                # Only what the check reads, as a tuple: the index is hundreds of MB.
                rows.append(tuple(row.get(k) for k in FIELDS))
    with ThreadPoolExecutor(max_workers=a.workers) as pool:
        flags = list(pool.map(lambda r: confirmed(dict(zip(FIELDS, r)), roots), rows, chunksize=256))
    shas = set()
    missing = set()
    for row, ok in zip(rows, flags):
        (shas if ok else missing).add(row[FIELDS.index("sha256")])
    # A digest the index lists twice (a re-fetch) counts once, and once any copy is confirmed
    # it is confirmed.
    missing -= shas
    header = {"v": VERSION, "generated": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "index_bytes": st.st_size, "rows": len(rows), "confirmed": len(shas),
              "missing": len(missing)}
    out_dir = os.path.dirname(os.path.abspath(a.out))
    fd, tmp = tempfile.mkstemp(dir=out_dir, prefix=".scan-confirmed-")
    try:
        with os.fdopen(fd, "w") as out:
            out.write("# " + json.dumps(header, sort_keys=True) + "\n")
            for sha in sorted(shas):
                out.write(sha + "\n")
            out.flush()
            os.fsync(out.fileno())
        os.chmod(tmp, 0o644)
        os.replace(tmp, a.out)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
    print(json.dumps(dict(header, step="scan-confirm"), sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("publish")
    p.add_argument("--index", required=True, help="the fetcher's index-full.jsonl")
    p.add_argument("--root", action="append", required=True,
                   help="a directory holding keep/ and buffer/ (repeat for each)")
    p.add_argument("--out", required=True)
    p.add_argument("--workers", type=int, default=16)
    a = ap.parse_args(argv)
    return publish(a)


if __name__ == "__main__":
    sys.exit(main())
