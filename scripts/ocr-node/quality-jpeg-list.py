#!/usr/bin/env python3
"""Build quality-jpeg-cache.py's page list from a quality wave manifest (#850).

The runner gives shard i every NUM_SHARDS-th batch, with batches ordered by
page count (descending), then batch, then version. Each shard walks its
batches in order. This lists the JPEG2000 pages of each shard's batches from
--start-after onward, interleaved across shards, so the cache stays just
ahead of every shard at once. JPEG masters are left out: most go to workers
as they are.

    quality-jpeg-list.py --manifest wave9_manifest.parquet --out pages.txt \
        [--shards 8] [--current 0=dlc_friendship_ver03,...] [--start-after 0=0,4=1] \
        [--damage-index damage.txt --keep-files keep-jp2.txt] [--done done.txt]

After the manifest's batches the runner takes tier 1: batches in the damage
audit index (curated audit/damage/v2/<batch>_verNN.parquet, in name order)
that are not in the manifest and have archived scans under keep/<batch>/.
--damage-index lists those blob names and --keep-files every archived page
(`find keep -type f` under the scans root, JPEG2000 or not: a batch with only
JPEG masters still takes its place in the rotation); with both, tier-1
batches follow the manifest's in the same shard rotation, their pages in the
runner's doc_id order.

--current s=<batch>_verNN skips shard s's batches up to and including the one
its status file names now; an unknown batch is an error. The manifest's
batches never change, so each shard takes every Nth of them exactly. Tier 1
comes from listings that grow after the runner starts, so its rotation can't
be verified: each shard's queue continues with every tier-1 batch after its
own position (all of tier 1 if it is still in the manifest), then, last, the
tier-1 batches before it: a batch archived after the runner started was in
its tier 2 then and may still be pending. The queues are deduplicated. That
converts some pages another shard has passed, but none a shard still needs
is left out. --start-after s=k does the same by index (k is the index
of the batch it is in now). Shards not named start at their first batch.
--done drops pages already in the converter's done.txt. Needs duckdb.
"""

import argparse
import itertools
import re

DAMAGE = re.compile(r"^audit/damage/v2/(.+)_ver(\d+)\.parquet$")


def page_key(path):
    """The runner's doc_id for keep/<batch>/<lccn>/<date>/<edition>/<seq>.<ext>,
    <lccn>_<date>_ed-<edition>_seq-<seq>, which it sorts as a string (seq-10 before seq-2)."""
    parts = path.split("/")
    if len(parts) != 6:
        return path
    return f"{parts[2]}_{parts[3]}_ed-{parts[4]}_seq-{parts[5].rsplit('.', 1)[0]}"


def tier1_batches(manifest_batches, damage_names, keep_files):
    """The runner's tier 1: damage-index batches, in name order, not in the manifest, with
    archived pages. Returns [((batch, version), [pages])]."""
    pages = {}
    for p in keep_files:
        parts = p.split("/")
        if len(parts) > 2 and parts[0] == "keep":
            pages.setdefault(parts[1], []).append(p)
    seen = set(manifest_batches)
    out = []
    for name in sorted(damage_names):
        m = DAMAGE.match(name)
        if not m:
            continue
        key = (m.group(1), int(m.group(2)))
        if key in seen:
            continue
        seen.add(key)
        if key[0] in pages:
            out.append((key, sorted(pages[key[0]], key=page_key)))
    return out


def ordered_batches(rows, tier1=()):
    """Every batch in the runner's order with its pages: the manifest's by page count, then tier 1."""
    by = {}
    for b, bv, name in rows:
        by.setdefault((b, int(bv)), []).append(name)
    batches = sorted(by, key=lambda k: (-len(by[k]), k[0], k[1]))
    return [(k, by[k]) for k in batches] + list(tier1)


def manifest_count(rows):
    """How many batches the manifest has: the start of tier 1 in ordered_batches."""
    return len({(b, int(bv)) for b, bv, _ in rows})


def remaining_batches(ordered, n_manifest, shards, s, after):
    """Shard s's batches after global index `after` (-1: from its first): its own manifest
    batches, every `shards`-th, then every tier-1 batch after `after`, unstrided."""
    mine = [k for i, k in enumerate(ordered[:n_manifest]) if i % shards == s and i > after]
    cut = max(n_manifest, after + 1)
    # Then the tier-1 batches before `after`, last: a batch archived after the runner started
    # was in its tier 2 then, and can sort ahead of every shard now while still pending.
    return mine + ordered[cut:] + ordered[n_manifest:cut]


def resolve_current(ordered, shards, current, warn=print, n_manifest=None):
    """{shard: "<batch>_verNN"} to {shard: the batches it has left}.

    The runner lists the scan archive and the damage index once, when it starts; both grow,
    so a later list can rotate tier 1 differently. A batch that is in the order but under
    another shard means the runner's rotation is unknown, so that shard gets every later
    batch (the queues are deduplicated when interleaved), with a warning. A batch not in the
    order at all is an error."""
    keys = [k for k, _ in ordered]
    if n_manifest is None:
        n_manifest = len(ordered)
    out = {}
    for s, name in current.items():
        batch, _, ver = name.rpartition("_ver")
        try:
            g = keys.index((batch, int(ver)))
        except ValueError:
            raise SystemExit(f"shard {s}: {name} is not in the batch order; check the inputs")
        if g < n_manifest and g % shards != s:
            warn(f"shard {s}: {name} is shard {g % shards}'s in the manifest order; check the inputs")
        out[s] = remaining_batches(ordered, n_manifest, shards, s, g)
    return out


def shard_queues(rows, shards, start_after, done, tier1=(), remaining=None):
    """rows: (batch, batch_version, blob_name) in the runner's doc_id order. One page list per
    shard. remaining: {shard: batches left} from resolve_current, which wins over start_after."""
    ordered = ordered_batches(rows, tier1)
    n_manifest = manifest_count(rows)
    remaining = remaining or {}
    queues = []
    for s in range(shards):
        if s in remaining:
            left = remaining[s]
        else:
            # start_after counts this shard's manifest batches (every Nth from s).
            k = start_after.get(s, -1)
            mine = list(range(s, n_manifest, shards))
            after = mine[k] if 0 <= k < len(mine) else (-1 if k < 0 else n_manifest - 1)
            left = remaining_batches(ordered, n_manifest, shards, s, after)
        queues.append([n for _, pages in left for n in pages
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


def parse_current(text):
    out = {}
    for part in filter(None, (text or "").split(",")):
        s, name = part.split("=")
        out[int(s)] = name.strip()
    return out


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--manifest", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--shards", type=int, default=8)
    p.add_argument("--start-after", default="")
    p.add_argument("--current", default="")
    p.add_argument("--damage-index")
    p.add_argument("--keep-files")
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
    tier1 = []
    if bool(a.damage_index) != bool(a.keep_files):
        p.error("--damage-index and --keep-files go together")
    if a.damage_index:
        with open(a.damage_index) as f:
            damage = [line.strip() for line in f if line.strip()]
        with open(a.keep_files) as f:
            keep = [line.strip().removeprefix("./") for line in f if line.strip()]
        tier1 = tier1_batches({(b, int(bv)) for b, bv, _ in rows}, damage, keep)
    remaining = resolve_current(ordered_batches(rows, tier1), a.shards, parse_current(a.current),
                                n_manifest=manifest_count(rows))
    queues = shard_queues(rows, a.shards, parse_start_after(a.start_after), done, tier1, remaining)
    out = list(dict.fromkeys(interleave(queues)))
    with open(a.out, "w") as f:
        f.write("".join(f"{n}\n" for n in out))
    for s, q in enumerate(queues):
        print(f"shard {s}: {len(q)} pages")
    print(f"{len(out)} pages to {a.out}")


if __name__ == "__main__":
    main()
