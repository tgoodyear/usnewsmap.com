#!/usr/bin/env python3
"""Read line crops with a Tesseract model at PSM 13 and score them (#2127).

CER is edit distance over reference length, pooled over lines, after frak_norm.normalise on both
sides. Each worker reads a shard of crops in one tesseract process (an image list), so the model is
loaded once per shard; pages in the output are split on the form feed tesseract writes between them.

    frak_score.py --lines DIR --tessdata DIR --lang NAME --out FILE.jsonl [--jobs 20]

--lines must be the dev directory: this script refuses a path whose lines.jsonl or names mark it as
held out, so phase 1 cannot score the test pages by mistake.
"""
import argparse, glob, json, os, subprocess, sys, tempfile, time
from multiprocessing import Pool

from frak_norm import normalise


def edit_distance(a: str, b: str) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def read_shard(job):
    pngs, tessdata, lang = job
    with tempfile.TemporaryDirectory() as td:
        lst = os.path.join(td, "list.txt")
        open(lst, "w").write("\n".join(pngs) + "\n")
        env = dict(os.environ, OMP_THREAD_LIMIT="1")
        subprocess.run(["tesseract", lst, os.path.join(td, "out"), "--psm", "13", "-l", lang,
                        "--tessdata-dir", tessdata], check=True, env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        text = open(os.path.join(td, "out.txt"), encoding="utf-8").read()
    parts = text.split("\f")  # tesseract writes the separator between pages
    assert len(parts) == len(pngs), (len(parts), len(pngs))
    return list(zip(pngs, parts))


def score_pair(pair):
    png, hyp = pair
    ref = normalise(open(png[:-4] + ".gt.txt", encoding="utf-8").read())
    hyp = normalise(hyp)
    return os.path.basename(png)[:-4], ref, hyp, edit_distance(ref, hyp)


def score(lines_dir, tessdata, lang, jobs=20, shard=200):
    assert "heldout" not in os.path.realpath(lines_dir), "phase 1 never reads the held-out lines"
    pngs = sorted(glob.glob(os.path.join(lines_dir, "*.png")))
    assert pngs, lines_dir
    shards = [(pngs[i:i + shard], tessdata, lang) for i in range(0, len(pngs), shard)]
    with Pool(jobs) as pool:
        read = [pair for part in pool.imap_unordered(read_shard, shards) for pair in part]
        rows = sorted(pool.map(score_pair, read, chunksize=100))
    errs, chars = sum(r[3] for r in rows), sum(len(r[1]) for r in rows)
    return {"lines": len(rows), "ref_chars": chars, "edits": errs, "cer": errs / chars}, rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lines", required=True); ap.add_argument("--tessdata", required=True)
    ap.add_argument("--lang", required=True); ap.add_argument("--out", required=True)
    ap.add_argument("--jobs", type=int, default=20)
    a = ap.parse_args()
    t0 = time.time()
    res, rows = score(a.lines, a.tessdata, a.lang, a.jobs)
    with open(a.out, "w", encoding="utf-8") as f:
        for name, ref, hyp, e in rows:
            f.write(json.dumps({"name": name, "ref": ref, "hyp": hyp, "edits": e}, ensure_ascii=False) + "\n")
    res["seconds"] = round(time.time() - t0)
    print(json.dumps(res))


if __name__ == "__main__":
    sys.exit(main())
