#!/usr/bin/env python3
"""Arms B and C of #2127: fine-tune frak2021 0.905 on the real training lines (B), or on them plus
arm C's synthetic lines (C, --arm C --synth-lstmf); pick the checkpoint on dev.

The budget and the stopping rule are fixed here, before any run, and are the same for arm C:

  MAX_ITERATIONS  200,000 training iterations (one line each; about 0.4 of a pass over the
                  training lines, and about three hours on the dev CT).
  EVAL_EVERY      every 10,000 iterations the checkpoint is made a model and read on the 17 dev
                  pages (PSM 13, normalised, pooled CER: frak_score.py).
  PATIENCE        training stops early once 5 evaluations in a row fail to beat the best dev CER.
  Selection       the evaluated checkpoint with the lowest dev CER (the earliest on a tie). The
                  start model's dev CER (iteration 0) is reported but is not a candidate.

lstmtraining's own evaluation list is the dev lines too. The training list is shuffled with a fixed
seed so the four sets are mixed. Lines whose text has a character outside frak2021's character set
are left out of the training list (lstmtraining could not encode them); dev scoring keeps them.
No line of a held-out or official-Test page may be in either list: asserted below.

    train_b.py --lines /srv/frak2127/lines --train-lstmf /srv/frak2127/lstmf/train \
        --dev-lstmf /srv/frak2127/lstmf/dev --start frak2021-0.905.traineddata \
        --heldout heldout-pages.txt --split-csv cg/split.csv --work /srv/frak2127/work/b
"""
import argparse, csv, glob, hashlib, json, os, random, shutil, subprocess, sys, time

import frak_score

MAX_ITERATIONS = 200_000
EVAL_EVERY = 10_000
PATIENCE = 5
SEED = 2127
TARGET_ERROR_RATE = 0.001   # lstmtraining's own stop; training BCER on real lines will not reach it
MAX_IMAGE_MB = 2500          # lstmtraining's image cache, to stay inside the CT's 8 GB
START_SHA256 = "1da2384254fa8462c776faf3b43307fe19ce51be0931c623b2fdd560f96e299a"  # models.lock


def page_of(name):
    st, stem, _ = name.split("__")
    return st, stem


def lstmf_name(path):
    """cut_lines.py names an .lstmf <first line>__<line count>.lstmf; it holds one line."""
    name, n = os.path.basename(path)[:-6].rsplit("__", 1)
    assert n == "1", path
    return name


def log(msg):
    print(time.strftime("%H:%M:%S"), msg, flush=True)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lines", required=True, help="cut_lines.py --out: train/, dev/, lines.jsonl")
    ap.add_argument("--train-lstmf", required=True); ap.add_argument("--dev-lstmf", required=True)
    ap.add_argument("--start", required=True); ap.add_argument("--heldout", required=True)
    ap.add_argument("--split-csv", required=True); ap.add_argument("--work", required=True)
    ap.add_argument("--jobs", type=int, default=20)
    ap.add_argument("--arm", default="B", choices=["B", "C"], help="C adds --synth-lstmf to B's real lines")
    ap.add_argument("--synth-lstmf", help="arm C: the synthetic lines' .lstmf folder (synth_lines.py render)")
    a = ap.parse_args()
    A = a.arm
    assert (A == "C") == bool(a.synth_lstmf), "arm C, and only arm C, takes --synth-lstmf"
    assert sha256(a.start) == START_SHA256, "start model is not frak2021 0.905"
    heldout = set(open(a.heldout).read().split())
    test = {r["Filename"] for r in csv.DictReader(open(a.split_csv)) if r["Test"] == "1"}
    meta = {}
    with open(os.path.join(a.lines, "lines.jsonl"), encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            meta[r["name"]] = r

    def check(names, split):
        for n in names:
            st, stem = page_of(n)
            assert meta[n]["split"] == split, n
            assert not (st == "cg" and (stem in heldout or stem in test)), f"held-out/Test line in {split}: {n}"

    os.makedirs(os.path.join(a.work, "out"), exist_ok=True)
    os.makedirs(os.path.join(a.work, "models"), exist_ok=True)
    # Lists.
    train, skipped = [], 0
    for p in sorted(glob.glob(os.path.join(a.train_lstmf, "*.lstmf"))):
        n = lstmf_name(p)
        if meta[n]["unencodable"]:
            skipped += 1
            continue
        train.append(p)
    real_n = len(train)
    synth = sorted(glob.glob(os.path.join(a.synth_lstmf, "*.lstmf"))) if a.synth_lstmf else []
    for p in synth:
        assert os.path.basename(p).startswith("synth__"), p
    train += synth
    random.Random(SEED).shuffle(train)
    dev = sorted(glob.glob(os.path.join(a.dev_lstmf, "*.lstmf")))
    dev_png_dir = os.path.join(a.lines, "dev")
    dev_pngs = [os.path.basename(p)[:-4] for p in glob.glob(os.path.join(dev_png_dir, "*.png"))]
    check([lstmf_name(p) for p in train if not os.path.basename(p).startswith("synth__")], "train")
    check([lstmf_name(p) for p in dev] + dev_pngs, "dev")
    assert len({page_of(n)[1] for n in dev_pngs}) == 17
    open(os.path.join(a.work, "train.list"), "w").write("\n".join(train) + "\n")
    open(os.path.join(a.work, "dev.list"), "w").write("\n".join(dev) + "\n")
    log(f"train.list {len(train)} lines ({real_n} real, {len(synth)} synthetic; {skipped} unencodable left out); dev.list {len(dev)}; dev crops {len(dev_pngs)}")

    start = os.path.join(a.work, "frak2021.traineddata")
    if not os.path.exists(start):
        shutil.copy(a.start, start)
        subprocess.run(["combine_tessdata", "-e", start, os.path.join(a.work, "frak2021.lstm")], check=True,
                       stdout=subprocess.DEVNULL)
    tessdata = os.path.join(a.work, "models")
    for c in ("configs",):
        if not os.path.exists(os.path.join(tessdata, c)):
            os.symlink("/usr/share/tesseract-ocr/5/tessdata/configs", os.path.join(tessdata, c))
    scores_path = os.path.join(a.work, "dev_scores.jsonl")
    done = {}
    if os.path.exists(scores_path):
        for line in open(scores_path):
            r = json.loads(line)
            done[r["iteration"]] = r

    def evaluate(it, model):
        if it in done:
            return done[it]
        lang = f"frak{A}_{it}"
        dst = os.path.join(tessdata, lang + ".traineddata")
        if model != dst:
            shutil.copy(model, dst)
        log(f"dev read of iteration {it}")
        res, rows = frak_score.score(dev_png_dir, tessdata, lang, a.jobs)
        with open(os.path.join(a.work, f"dev_{it}.jsonl"), "w", encoding="utf-8") as f:
            for name, ref, hyp, e in rows:
                f.write(json.dumps({"name": name, "ref": ref, "hyp": hyp, "edits": e}, ensure_ascii=False) + "\n")
        res["iteration"] = it
        done[it] = res
        with open(scores_path, "a") as f:
            f.write(json.dumps(res) + "\n")
        log(f"iteration {it}: dev CER {100 * res['cer']:.3f}% over {res['lines']} lines")
        return res

    evaluate(0, start)
    ckpt = os.path.join(a.work, "out", f"frak{A}_checkpoint")
    best_it, best, since = None, None, 0
    for it in range(EVAL_EVERY, MAX_ITERATIONS + 1, EVAL_EVERY):
        model = os.path.join(tessdata, f"frak{A}_{it}.traineddata")
        if it not in done:
            cont = ckpt if os.path.exists(ckpt) else os.path.join(a.work, "frak2021.lstm")
            log(f"training to iteration {it} from {os.path.basename(cont)}")
            logf = os.path.join(a.work, "train.log")
            off = os.path.getsize(logf) if os.path.exists(logf) else 0
            with open(logf, "a") as lf:
                rc = subprocess.run(["lstmtraining", "--continue_from", cont, "--traineddata", start,
                                     "--model_output", os.path.join(a.work, "out", f"frak{A}"),
                                     "--train_listfile", os.path.join(a.work, "train.list"),
                                     "--eval_listfile", os.path.join(a.work, "dev.list"),
                                     "--max_iterations", str(it), "--target_error_rate", str(TARGET_ERROR_RATE),
                                     "--max_image_MB", str(MAX_IMAGE_MB)], stdout=lf, stderr=subprocess.STDOUT).returncode
            if rc != 0:
                # lstmtraining 5.5.0 can crash on exit while its evaluation thread is still reading the
                # dev list, after it has written the checkpoint. Accept the run only if it got there.
                with open(logf, "rb") as lf:
                    lf.seek(off)
                    tail = lf.read().decode("utf-8", "replace")
                assert "Finished!" in tail and f"/{it}/" in tail and "wrote checkpoint" in tail, f"lstmtraining failed ({rc})"
                log(f"lstmtraining exited {rc} after finishing iteration {it} and writing its checkpoint; continuing")
            subprocess.run(["lstmtraining", "--stop_training", "--continue_from", ckpt, "--traineddata", start,
                            "--model_output", model], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            # Keep this point's trainer state too, so any checkpoint can be revisited.
            shutil.copy(ckpt, os.path.join(a.work, "out", f"frak{A}_{it}.checkpoint"))
        r = evaluate(it, model)
        if best is None or r["cer"] < best:
            best_it, best, since = it, r["cer"], 0
        else:
            since += 1
        if since >= PATIENCE:
            log(f"stopping: {PATIENCE} evaluations without a better dev CER")
            break
    final = os.path.join(a.work, f"frak2127_{A.lower()}.traineddata")
    shutil.copy(os.path.join(tessdata, f"frak{A}_{best_it}.traineddata"), final)
    out = {"selected_iteration": best_it, "dev_cer": best, "start_dev_cer": done[0]["cer"],
           "last_iteration": max(done), "sha256": sha256(final), "max_iterations": MAX_ITERATIONS,
           "eval_every": EVAL_EVERY, "patience": PATIENCE, "seed": SEED, "arm": A, "train_lines": len(train), "real_lines": real_n, "synth_lines": len(synth),
           "unencodable_left_out": skipped, "dev_lstmf": len(dev), "dev_crops": len(dev_pngs)}
    json.dump(out, open(os.path.join(a.work, "result.json"), "w"), indent=1)
    log("done " + json.dumps(out))


if __name__ == "__main__":
    sys.exit(main())
