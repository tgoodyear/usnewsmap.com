"""Read example_id/dataset_id columns from CHURRO-DS parquet files by range requests. Usage: --sources a,b --out FILE"""
import argparse, json, urllib.request
import fsspec, pyarrow.parquet as pq
ap = argparse.ArgumentParser(); ap.add_argument("--sources", required=True); ap.add_argument("--out", required=True); a = ap.parse_args()
want = set(a.sources.split(","))
api = "https://huggingface.co/api/datasets/stanford-oval/churro-dataset/parquet/default"
import time
for attempt in range(20):
    try:
        splits = json.load(urllib.request.urlopen(api, timeout=60)); break
    except Exception as e:
        print("list retry", e, flush=True); time.sleep(60)
res = {}
fs = fsspec.filesystem("http")
for sp, urls in splits.items():
    for u in urls:
        import os, time, hashlib
        cache = "/tmp/churro-cache/" + sp + "-" + u.rsplit("/", 1)[-1] + ".json"
        os.makedirs("/tmp/churro-cache", exist_ok=True)
        if os.path.exists(cache):
            rows = json.load(open(cache))
        else:
            for attempt in range(8):
                try:
                    with fs.open(u, "rb", block_size=1 << 20) as f:
                        t = pq.read_table(f, columns=["example_id", "dataset_id"])
                    break
                except Exception as e:
                    print("retry", u, str(e)[:80], flush=True); time.sleep(30 * (attempt + 1))
            rows = list(zip(t.column("example_id").to_pylist(), t.column("dataset_id").to_pylist()))
            json.dump(rows, open(cache, "w")); time.sleep(3)
        class T: pass
        for e, dsid in rows:
            if dsid in want: res.setdefault(f"{dsid}/{sp}", []).append(e)
        print(sp, u.rsplit("/", 1)[-1], {k: len(v) for k, v in res.items()}, flush=True)
json.dump(res, open(a.out, "w"), indent=1)
