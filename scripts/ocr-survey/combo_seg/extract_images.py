#!/usr/bin/env python3
"""Unpack one batched tar stream of training inputs for the combined segmenter (#2107), keeping only what is needed.

Reads a tar from stdin. A .parquet member is written to --tmp-dir, the page images of the rows that --paths files
ask for (values "<...>/<parquet name>#row=N") are written to --out-dir/images/<page id>.img (the stored bytes,
unchanged), and the parquet is deleted. Every other member is written under --out-dir at its own path. Then each
--paths file is rewritten to --out-dir/<name>.local.json, pointing at the local copies. Progress counts go to stdout.

    tar cf - ... | extract_images.py --out-dir D --tmp-dir D --paths a/paths.json [--paths b/paths.json]
                                     [--strip-prefix /data/layout-refs/]
"""
import argparse
import json
import os
import shutil
import sys
import tarfile


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--tmp-dir", required=True)
    ap.add_argument("--paths", action="append", required=True, help="paths.json, as a member path inside the tar")
    ap.add_argument("--strip-prefix", default="/data/layout-refs/")
    a = ap.parse_args(argv)
    import pyarrow.parquet as pq
    os.makedirs(os.path.join(a.out_dir, "images"), exist_ok=True)
    os.makedirs(a.tmp_dir, exist_ok=True)
    want_paths = set(a.paths)
    maps = {}
    rows_by_parquet = {}
    done_pages = 0
    with tarfile.open(fileobj=sys.stdin.buffer, mode="r|") as tf:
        for m in tf:
            if not m.isfile():
                continue
            if m.name.endswith(".parquet"):
                tmp = os.path.join(a.tmp_dir, os.path.basename(m.name))
                with tf.extractfile(m) as src, open(tmp, "wb") as dst:
                    shutil.copyfileobj(src, dst, 1 << 22)
                need = rows_by_parquet.get(os.path.basename(m.name), {})
                pf = pq.ParquetFile(tmp)
                start = 0
                for rg in range(pf.metadata.num_row_groups):
                    n = pf.metadata.row_group(rg).num_rows
                    sel = [i for i in need if start <= i < start + n]
                    if sel:
                        col = pf.read_row_group(rg, columns=["page_image"]).column("page_image").to_pylist()
                        for i in sel:
                            with open(os.path.join(a.out_dir, "images", need[i] + ".img"), "wb") as f:
                                f.write(col[i - start]["bytes"])
                            done_pages += 1
                    start += n
                os.remove(tmp)
                print("parquet %s: %d pages (total %d)" % (os.path.basename(m.name), len(need), done_pages), flush=True)
                continue
            dst = os.path.join(a.out_dir, m.name)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with tf.extractfile(m) as src, open(dst, "wb") as f:
                shutil.copyfileobj(src, f, 1 << 22)
            if m.name in want_paths:
                maps[m.name] = json.load(open(dst))
                for pid, src_path in maps[m.name].items():
                    if "#row=" in src_path:
                        f_, r_ = src_path.rsplit("#row=", 1)
                        rows_by_parquet.setdefault(os.path.basename(f_), {})[int(r_)] = pid
    for name, mp_ in maps.items():
        local = {}
        for pid, src_path in mp_.items():
            if "#row=" in src_path:
                local[pid] = os.path.join(a.out_dir, "images", pid + ".img")
            else:
                local[pid] = os.path.join(a.out_dir, src_path[len(a.strip_prefix):] if src_path.startswith(a.strip_prefix) else src_path)
        missing = [p for p, v in local.items() if not os.path.exists(v)]
        json.dump(local, open(os.path.join(a.out_dir, name[:-5] + ".local.json"), "w"), sort_keys=True)
        print("%s: %d pages, %d missing locally" % (name, len(local), len(missing)), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
