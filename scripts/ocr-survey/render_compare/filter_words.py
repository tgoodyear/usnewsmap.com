"""Drop alignment overshoot from loader rows (#2103 deviation, 22 Nov).

Crops whose words are > --max-ins-share insertions have an ALTO box larger than the truth region;
insertion runs of >= --min-run words are box overshoot at a crop's edge. Neither is an OCR error.
Usage: filter_words.py --words IN --out OUT [--max-ins-share 0.5] [--min-run 5]
"""
import argparse, collections, json

ap = argparse.ArgumentParser()
ap.add_argument("--words", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--max-ins-share", type=float, default=0.5)
ap.add_argument("--min-run", type=int, default=5)
a = ap.parse_args()
by_crop = collections.defaultdict(list)
for l in open(a.words):
    r = json.loads(l)
    by_crop[r["crop_id"]].append(r)
kept = dropped_crops = dropped_run_words = 0
with open(a.out, "w") as f:
    for cid, rs in by_crop.items():
        rs.sort(key=lambda r: r["word_index"])
        if sum(r["op"] == "ins" for r in rs) / len(rs) > a.max_ins_share:
            dropped_crops += 1
            continue
        drop = set()
        i = 0
        while i < len(rs):
            j = i
            while j < len(rs) and rs[j]["op"] == "ins":
                j += 1
            if j - i >= a.min_run:
                drop.update(range(i, j))
            i = max(j, i + 1)
        for k, r in enumerate(rs):
            if k in drop:
                dropped_run_words += 1
                continue
            f.write(json.dumps(r) + "\n")
            kept += 1
print(json.dumps({"kept": kept, "dropped_crops": dropped_crops, "dropped_run_words": dropped_run_words}))
