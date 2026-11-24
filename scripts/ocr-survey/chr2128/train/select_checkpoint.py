#!/usr/bin/env python3
"""Choose a #2128 arm's model on lstmtraining's own evaluation list (training pages only).

The candidates are every checkpoint lstmtraining kept in --checkpoints (`<name>_<error>_<iter>_<iter>.checkpoint`,
written at each new best training error, and `<name>_checkpoint`, the last). Each is turned into a model
(`lstmtraining --stop_training`) and scored with `lstmeval` on --eval-list; the one with the lowest character
error (ties: the later iteration) becomes <out>/<name>.traineddata. Every score goes to <out>/selection.json.
The last checkpoint's model is also kept, as <out>/<name>-last.traineddata.

    select_checkpoint.py --start DIR --checkpoints DIR --name chr_phoenix --eval-list eval.list --out DIR
"""
import argparse
import glob
import hashlib
import json
import os
import re
import shutil
import subprocess


def iteration(path, name):
    m = re.search(rf"{re.escape(name)}_[\d.]+_(\d+)_\d+\.checkpoint$", path)
    return int(m.group(1)) if m else 10 ** 9  # the last checkpoint


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--start", required=True)
    ap.add_argument("--checkpoints", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--eval-list", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    td = os.path.join(a.start, "lang", "chr", "chr.traineddata")
    cands = sorted(glob.glob(os.path.join(a.checkpoints, f"{a.name}_*.checkpoint")) +
                   glob.glob(os.path.join(a.checkpoints, f"{a.name}_checkpoint")), key=lambda p: iteration(p, a.name))
    tmp = os.path.join(a.out, "candidates")
    os.makedirs(tmp, exist_ok=True)
    rows = []
    for c in cands:
        model = os.path.join(tmp, os.path.basename(c).replace(".checkpoint", "") + ".traineddata")
        subprocess.run(["lstmtraining", "--stop_training", "--continue_from", c, "--traineddata", td,
                        "--model_output", model], check=True, capture_output=True)
        r = subprocess.run(["lstmeval", "--model", model, "--eval_listfile", a.eval_list], capture_output=True, text=True)
        m = re.search(r"Eval Char error rate=([\d.]+), Word error rate=([\d.]+)", r.stdout + r.stderr)
        rows.append({"checkpoint": os.path.basename(c), "iteration": iteration(c, a.name),
                     "eval_cer": float(m.group(1)) if m else None, "eval_wer": float(m.group(2)) if m else None})
        print(json.dumps(rows[-1]), flush=True)
    ok = [r for r in rows if r["eval_cer"] is not None]
    best = min(ok, key=lambda r: (r["eval_cer"], -r["iteration"]))
    pick = os.path.join(tmp, best["checkpoint"].replace(".checkpoint", "") + ".traineddata")
    dst = os.path.join(a.out, f"{a.name}.traineddata")
    shutil.copyfile(pick, dst)
    last = [r for r in rows if r["iteration"] == 10 ** 9]
    if last:
        shutil.copyfile(os.path.join(tmp, last[0]["checkpoint"].replace(".checkpoint", "") + ".traineddata"),
                        os.path.join(a.out, f"{a.name}-last.traineddata"))
    sha = hashlib.sha256(open(dst, "rb").read()).hexdigest()
    json.dump({"candidates": rows, "chosen": best, "model": os.path.basename(dst), "sha256": sha},
              open(os.path.join(a.out, "selection.json"), "w"), indent=1)
    print(json.dumps({"chosen": best, "sha256": sha}))


if __name__ == "__main__":
    main()
