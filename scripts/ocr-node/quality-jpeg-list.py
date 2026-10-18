#!/usr/bin/env python3
"""Build quality-jpeg-cache.py's page list from a quality wave manifest (#850).

The runner gives shard i every NUM_SHARDS-th batch, with batches ordered by
page count (descending), then batch, then version. Each shard walks its
batches in order. This lists the JPEG2000 pages of each shard's batches from
--start-after onward, interleaved across shards, so the cache stays just
ahead of every shard at once. JPEG masters are left out: most go to workers
as they are.

    quality-jpeg-list.py --manifest wave9_manifest.parquet --out pages.txt \
        [--shards 8] [--start-after 0=0,4=1] [--done done.txt]

--start-after s=k skips shard s's first k+1 batches (k is the index of the
batch it is in now). Shards not named start at their first batch. --done
drops pages already in the converter's done.txt. Needs duckdb.
"""

import argparse
import itertools


def shard_queues(rows, shards, start_after, done):
    """rows: (batch, batch_version, blob_name) in the runner's doc_id order. One page list per shard."""
    by = {}
    for b, bv, name in rows:
        by.setdefault((b, int(bv)), []).append(name)
    batches = sorted(by, key=lambda k: (-len(by[k]), k[0], k[1]))
    queues = []
    for s in range(shards):
        mine = batches[s::shards]
        skip = start_after[s] + 1 if s in start_after else 0
        queues.append([n for k in mine[skip:] for n in by[k]
                       if n.endswith(".jp2") and n not in done])
    return queues


def interleave(queues):
    return [n for t in itertools.zip_longest(*queues) for n in t if n]


def parse_start_after(text):
    out = {}
    for part in filter(None, (text or "").split(",")):
        s, k = part.split("=")
        out[int(s)] = int(k)
    return out


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--manifest", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--shards", type=int, default=8)
    p.add_argument("--start-after", default="")
    p.add_argument("--done")
    a = p.parse_args(argv)
    import duckdb

    rows = duckdb.connect().execute(
        "SELECT batch, batch_version, blob_name FROM read_parquet(?) "
        "WHERE blob_name IS NOT NULL AND blob_name <> '' ORDER BY doc_id", [a.manifest]
    ).fetchall()
    done = set()
    if a.done:
        with open(a.done) as f:
            done = {line.split("\t")[0].strip() for line in f}
    queues = shard_queues(rows, a.shards, parse_start_after(a.start_after), done)
    out = interleave(queues)
    with open(a.out, "w") as f:
        f.write("".join(f"{n}\n" for n in out))
    for s, q in enumerate(queues):
        print(f"shard {s}: {len(q)} pages")
    print(f"{len(out)} pages to {a.out}")


if __name__ == "__main__":
    main()
