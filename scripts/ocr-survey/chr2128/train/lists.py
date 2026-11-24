#!/usr/bin/env python3
"""Training and evaluation lists (#2128), by round two's rule (chr_finetune.sh step 2): the evaluation list
is the --real lines of about one page in twenty, chosen by sha256 of the page; everything else trains.
--extra lines (R4c's GHN lines) always train, so R3 and R4c are chosen on the same evaluation list.

    lists.py --lstmf DIR --real NAME --synth NAME [--extra NAME] --out-dir DIR
NAME is the basename of the line directory make_lstmf.sh was given (the .lstmf prefix).
"""
import argparse
import glob
import hashlib
import os


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lstmf", required=True)
    ap.add_argument("--real", required=True)
    ap.add_argument("--synth", required=True)
    ap.add_argument("--extra")
    ap.add_argument("--out-dir", required=True)
    a = ap.parse_args()
    d = os.path.abspath(a.lstmf)
    get = lambda n: sorted(glob.glob(os.path.join(d, f"{n}__*.lstmf"))) if n else []
    real, synth, extra = get(a.real), get(a.synth), get(a.extra)
    train, ev = synth + extra, []
    for f in real:
        page = "_".join(os.path.basename(f).split("__", 1)[1].split("_")[:4])
        (ev if int(hashlib.sha256(page.encode()).hexdigest(), 16) % 20 == 0 else train).append(f)
    train.sort()
    os.makedirs(a.out_dir, exist_ok=True)
    open(os.path.join(a.out_dir, "train.list"), "w").write("\n".join(train) + "\n")
    open(os.path.join(a.out_dir, "eval.list"), "w").write("\n".join(ev) + "\n")
    print(f"{len(train)} train ({len(synth)} synthetic, {len(real) - len(ev)} real, {len(extra)} extra), {len(ev)} eval")


if __name__ == "__main__":
    main()
