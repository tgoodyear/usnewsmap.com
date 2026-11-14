#!/usr/bin/env python3
"""Builds the study's reading rows.

    prep.py --truth-968 DIR --truth-as DIR --unlabelled DIR --out-dir OUT

Truth rows (OUT/truth.jsonl): one per (set, crop, reader) with the human reference:
  {"set", "lang", "unit", "page", "reader", "ref", "hyp"}
  DIR(968)/refs.jsonl and DIR/hyps/{tess3,tess6,xiaomi-local,qwen8b-local,loc,as}.jsonl;
  DIR(as)/results/{refs.jsonl, loc/outputs.jsonl, tess-psm3/outputs.jsonl, tess-psm6/outputs.jsonl}.
Unlabelled rows (OUT/unlabelled.jsonl): one per (crop, reader) for the Qwen 8B and LoC readings of the
  same crop: {"campaign", "lang", "page", "unit", "reader" ("qwen"|"loc"), "hyp", "state"}.
  The Qwen text is crops.parquet's vlm_text; LoC's crop text is qwen.json's loc_text (LoC's ALTO words
  inside the crop box, in visual line order), from the campaign's qwen.json archive. Looped or unread
  crops are left out. Bundle jobs name crops "<doc_id>:cNN"; single-page jobs "cNN" with the job's doc_id. A crop in two campaigns is kept once (first campaign by name).
"""
import argparse, json, tarfile, glob, os
import pyarrow.parquet as pq

ap = argparse.ArgumentParser()
ap.add_argument("--truth-968", required=True)
ap.add_argument("--truth-as", required=True)
ap.add_argument("--unlabelled", required=True)
ap.add_argument("--out-dir", required=True)
a = ap.parse_args()
os.makedirs(a.out_dir, exist_ok=True)

def jl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]

n = 0
with open(os.path.join(a.out_dir, "truth.jsonl"), "w", encoding="utf-8") as out:
    refs = jl(os.path.join(a.truth_968, "refs.jsonl"))
    for rd in ["tess3", "tess6", "xiaomi-local", "qwen8b-local", "loc", "as"]:
        hyp = {r["id"]: r["text"] for r in jl(os.path.join(a.truth_968, "hyps", rd + ".jsonl"))}
        for r in refs:
            # fr-52, fi-52 and sv-52 are subsets of the new-newseye sets (the same crops): left out
            if r["id"] in hyp and not r["id"].split("/")[0].endswith("-52"):
                out.write(json.dumps({"set": r["id"].split("/")[0], "lang": r["lang"], "unit": r["id"], "page": r["page"],
                                      "reader": rd, "ref": r["ref"], "hyp": hyp[r["id"]] or ""}, ensure_ascii=False) + "\n"); n += 1
    res = os.path.join(a.truth_as, "results")
    refs = jl(os.path.join(res, "refs.jsonl"))
    for rd, sub in [("loc", "loc"), ("tess3", "tess-psm3"), ("tess6", "tess-psm6")]:
        hyp = {r["id"]: r["text"] for r in jl(os.path.join(res, sub, "outputs.jsonl"))}
        for r in refs:
            if r["id"] in hyp:
                out.write(json.dumps({"set": "en-as", "lang": "eng", "unit": "en-as/" + r["id"], "page": "en-as/" + r["page"],
                                      "reader": rd, "ref": r["ref"], "hyp": hyp[r["id"]] or ""}, ensure_ascii=False) + "\n"); n += 1
print("truth rows", n)

seen = set()
n = 0
with open(os.path.join(a.out_dir, "unlabelled.jsonl"), "w", encoding="utf-8") as out:
    for cdir in sorted(glob.glob(os.path.join(a.unlabelled, "qwen-*"))):
        camp = os.path.basename(cdir)
        if not os.path.exists(os.path.join(cdir, "crops.parquet")):
            continue
        lang = {r["doc_id"]: r["language"] for r in pq.read_table(os.path.join(cdir, "pages.parquet"), columns=["doc_id", "language"]).to_pylist()}
        loc = {}
        with tarfile.open(os.path.join(a.unlabelled, "qwen-json", camp + "-qwen-json.tgz")) as tf:
            for m in tf:
                if m.isfile() and m.name.endswith("qwen.json"):
                    j = json.load(tf.extractfile(m))
                    for c in j.get("crops", []):
                        cid = c["id"] if ":" in c["id"] else f"{j['doc_id']}:{c['id']}"
                        loc[cid] = c.get("loc_text") or ""
        rows = pq.read_table(os.path.join(cdir, "crops.parquet"), columns=["doc_id", "crop_id", "reader", "vlm_text", "vlm_crop_state", "vlm_reading_present", "vlm_model"]).to_pylist()
        for r in rows:
            if r["reader"] != "vlm" or not r["vlm_reading_present"] or r["vlm_crop_state"] in ("looped", "no_text", "unjudged"):
                continue
            key = (r["doc_id"], r["crop_id"])
            cid = f"{r['doc_id']}:{r['crop_id']}"
            if key in seen or cid not in loc:
                continue
            seen.add(key)
            base = {"campaign": camp, "lang": lang.get(r["doc_id"]), "page": r["doc_id"], "unit": cid, "state": r["vlm_crop_state"], "model": r["vlm_model"]}
            out.write(json.dumps(dict(base, reader="qwen", hyp=r["vlm_text"] or ""), ensure_ascii=False) + "\n")
            out.write(json.dumps(dict(base, reader="loc", hyp=loc[cid]), ensure_ascii=False) + "\n")
            n += 1
print("unlabelled crops", n)
