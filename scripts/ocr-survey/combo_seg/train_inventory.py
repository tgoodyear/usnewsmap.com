#!/usr/bin/env python3
"""Which pages the combined segmenter (#2107) may train on, per source, from reference rows and the test ledger.

Reads reference rows and page-id lists only: no image, no detector output, no score.

La Liberte (Teklia/Newspapers-finlam-La-Liberte, MIT), train split. Taken out, in this order:
  - inventory.py's used set: the first 500 in sha256("2107:ll:"+id) order (banded tuning) and the first 2,000
    in sha256("2107:tr:"+id) order (trained-detector training and holdout);
  - the combination test (diag2107/combo/page-ids.txt) and the threshold test's dev and test lists
    (thr2107/work/{dev,test}-liberte/page-ids.txt), each checked against its published sha256;
  - banded v2 step 1, reconstructed from its frozen rule (comment 7122420362): the first 500 pages with a non-empty
    scoring reference in sha256("2107:v2:"+id) order; its sha256 is printed for checking against the v2 branch;
  - the combined segmenter's proposed test draw: the next 500 with a non-empty reference in
    sha256("2107:comboseg:test:"+id) order (draft until the plan is frozen).
  The rest with a non-empty reference is the clean training pool (no detector saw it, the finder was not tuned
  on it). The 2,390 used pages are listed apart: usable, but the trained model (2,000) or the banded finder (500)
  saw them, so their detector features are optimistic.
Chronicling Germany (CC BY-NC 4.0): the Test split (100) is de-cg, scored in step 1's first attempt (comment
  7086303955); every non-Test page with a paragraph and a vertical separator (615) was spent as a test page (the
  German combination test, 300, and the threshold test's dev 100 and test 215). Those 615 are listed as
  spent-test pages usable for training once nothing will test on them again.
Beyond Words (CC0): train pages with at least one marked non-text box (photograph, illustration, comics/cartoon,
  editorial cartoon, map, advertisement, or a headline >= 25% of the page width): negatives only. Validation pages
  are the English false-gutter measure and are never used.
en-gutter volunteer labels: tasks of set en2107-l2 by role; the bw-nontext gold negatives lie on Beyond Words
  validation pages and are left out.
Finlam, Colorado, Europeana: listed with the reason they are left out or caveated.

    train_inventory.py --refs-dir DIR --ledger-root DIR --cg-inventory TSV --engutter-tasks FILE --out-dir DIR
                       [--sample-n 70 --sample-image-prefix PARQUET] [--sample-cg-n 30]
"""
import argparse
import collections
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import refs as refsmod  # noqa: E402

LL = "Teklia__Newspapers-finlam-La-Liberte__%s.jsonl"
SHA = {"combo": "eddf61bedddaefa714d000446a0043f7d10b62f79e93679038c51251e47bb523",
       "thr-dev-liberte": "184b3997480d5ca196c37990998b2e2d6fbeee87d3d442718f3214a802103ca8",
       "thr-test-liberte": "71708bf332b9c40152e5af0e80e72ac337c63bd2bbbf58f9e4d20dd761bceca0",
       "thr-dev-cg": "4b1d2ac8b767ad58efe46cc9c28abdfe6f7c7ec75dbfb4a8bbacb05b7e432c17",
       "thr-test-cg": "2eb289b10cc742a7f74a180ddc751f72f7403d994a26ca7d28b27c4b47534db5",
       "cg-german-test": "e67d4a615e3cc2007f7f2c64c35e2197f6087934ee2fdbc5164628964b9d1bda"}
BW_NONTEXT = {"photograph", "illustration", "comics/cartoon", "editorial cartoon", "map", "advertisement"}
N_V2, N_TEST = 500, 500


def key(prefix, pid):
    return hashlib.sha256((prefix + str(pid)).encode()).hexdigest()


def id_list(path, expect=None):
    raw = open(path, "rb").read()
    if expect is not None:
        got = hashlib.sha256(raw).hexdigest()
        if got != expect:
            raise SystemExit("%s: sha256 %s, expected %s" % (path, got, expect))
    return [ln.strip() for ln in raw.decode().splitlines() if ln.strip()]


def write_list(path, ids):
    text = "".join(p + "\n" for p in ids)
    open(path, "w").write(text)
    return hashlib.sha256(text.encode()).hexdigest()


def rows(path):
    with open(path) as f:
        for ln in f:
            if ln.strip():
                yield json.loads(ln)


def liberte(a, inv, lists):
    has_ref, image = {}, {}
    for r in rows(os.path.join(a.refs_dir, LL % "train")):
        image[r["page_id"]] = r["image_path"]
        has_ref[r["page_id"]] = bool(refsmod.row_reference(r)["gutters"])
    ids = sorted(has_ref)
    ll = set(sorted(ids, key=lambda p: key("2107:ll:", p))[:500])
    tr = set(sorted(ids, key=lambda p: (key("2107:tr:", p), p))[:2000])
    root = a.ledger_root
    combo = set(id_list(os.path.join(root, "diag2107/combo/page-ids.txt"), SHA["combo"]))
    thr = set(id_list(os.path.join(root, "thr2107/work/dev-liberte/page-ids.txt"), SHA["thr-dev-liberte"]))
    thr |= set(id_list(os.path.join(root, "thr2107/work/test-liberte/page-ids.txt"), SHA["thr-test-liberte"]))
    used = ll | tr
    spent = combo | thr
    rest = [p for p in ids if p not in used and p not in spent]
    v2 = [p for p in sorted(rest, key=lambda p: (key("2107:v2:", p), p)) if has_ref[p]][:N_V2]
    rest2 = [p for p in rest if p not in set(v2)]
    test = [p for p in sorted(rest2, key=lambda p: (key("2107:comboseg:test:", p), p)) if has_ref[p]][:N_TEST]
    pool = sorted(p for p in rest2 if p not in set(test) and has_ref[p])
    lists["liberte-train-pool"] = pool
    lists["liberte-detector-seen"] = sorted(p for p in used if has_ref[p])
    inv["liberte"] = {
        "train_split_pages": len(ids), "with_reference": sum(has_ref.values()),
        "used_inventory_ll500_or_tr2000": len(used), "used_inventory_with_reference": len(lists["liberte-detector-seen"]),
        "spent_combination_test": len(combo), "spent_threshold_dev_and_test": len(thr),
        "unused_before_v2": len(rest), "unused_before_v2_with_reference": sum(has_ref[p] for p in rest),
        "v2_step1_reconstructed": len(v2), "v2_step1_page_ids_sha256": write_list(os.path.join(a.out_dir, "liberte-v2-step1-reconstructed.txt"), v2),
        "comboseg_test_draft": len(test), "comboseg_test_draft_page_ids_sha256": write_list(os.path.join(a.out_dir, "liberte-comboseg-test-DRAFT.txt"), test),
        "train_pool_clean": len(pool), "train_pool_without_reference_dropped": sum(1 for p in rest2 if not has_ref[p]),
        "val_and_test_splits": "left out: step 1 secondary and test sets (446 val, 433 test), spent",
    }
    return image


def chronicling_germany(a, inv, lists):
    by_split = collections.Counter()
    qual, nonq = [], []
    for ln in open(a.cg_inventory).read().splitlines()[1:]:
        p = ln.split("\t")
        pid, split, para, sepv = p[2], p[3], int(p[4]), int(p[5])
        by_split[split] += 1
        if split == "Test":
            continue
        (qual if para >= 1 and sepv >= 1 else nonq).append((pid, split))
    root = a.ledger_root
    german = set(id_list(os.path.join(root, "diag2107/cgfresh/page-ids.txt"), SHA["cg-german-test"]))
    thr = set(id_list(os.path.join(root, "thr2107/work/dev-cg/page-ids.txt"), SHA["thr-dev-cg"]))
    thr |= set(id_list(os.path.join(root, "thr2107/work/test-cg/page-ids.txt"), SHA["thr-test-cg"]))
    q = {p for p, _ in qual}
    lists["cg-spent-nontest"] = sorted(q)
    inv["chronicling_germany"] = {
        "pages_by_split": dict(by_split),
        "test_split_status": "de-cg, scored in step 1 first attempt (comment 7086303955); excluded from the German test "
                             "(comment 7102857621). The ledger in comment 7122420362 calls it unused: it is not.",
        "nontest_with_paragraph_and_vertical_separator": len(q),
        "of_which_german_combination_test": len(q & german), "of_which_threshold_dev_or_test": len(q & thr),
        "of_which_never_scored": len(q - german - thr),
        "nontest_without_vertical_separator": len(nonq),
        "nontest_without_vertical_separator_by_split": dict(collections.Counter(s for _, s in nonq)),
        "usable_for_training": "the 615 spent-test pages, once no later test uses them; no German page is unused",
    }


def beyond_words(a, inv, lists):
    out = {}
    for split in ("train", "val"):
        n, with_neg, nbox = 0, 0, collections.Counter()
        negs = {}
        for r in rows(os.path.join(a.refs_dir, "biglam__loc_beyond_words__%s.jsonl" % split)):
            n += 1
            w = r["width"]
            bx = [b["box"] + [b["type"]] for b in r["nontext_regions"] if b.get("type") in BW_NONTEXT]
            bx += [b["box"] + ["headline"] for b in r["text_regions"] if b.get("type") == "headline"
                   and b["box"][2] - b["box"][0] >= 0.25 * w]
            for b in bx:
                nbox[b[4]] += 1
            if bx:
                with_neg += 1
                if split == "train":
                    negs[r["page_id"]] = {"boxes": bx, "size": [r["width"], r["height"]], "ca_identity": r["meta"].get("ca_identity")}
        out[split] = {"pages": n, "pages_with_negative_box": with_neg, "boxes_by_type": dict(sorted(nbox.items()))}
        if split == "train":
            json.dump(negs, open(os.path.join(a.out_dir, "bw-train-negatives.json"), "w"), sort_keys=True)
    out["use"] = ("train: negatives only, boxes in the Beyond Words image frame (scale to the full-resolution scan "
                  "before build_features.py --negatives); the trained YOLOv8m was trained on these pages. val: never "
                  "(the English false-gutter measure)")
    inv["beyond_words"] = out


def engutter(a, inv):
    c = collections.Counter()
    pages = collections.defaultdict(set)
    for t in rows(a.engutter_tasks):
        pv = t["private"]
        c["%s / %s" % (pv["role"], pv["method"])] += 1
        pages[pv["method"]].add(pv["page_id"])
    trainable = [k for k in c if not k.endswith("bw-nontext")]
    inv["en_gutter_l2"] = {"tasks_by_role_method": dict(sorted(c.items())),
                           "pages_by_method": {k: len(v) for k, v in sorted(pages.items())},
                           "pages_trainable": len(set().union(*[pages[m] for m in pages if m != "bw-nontext"])),
                           "tasks_trainable_if_settled": sum(c[k] for k in trainable),
                           "settled_now": "see export_engutter.py summary"}


def others(a, inv):
    cnt = lambda f: sum(1 for _ in rows(os.path.join(a.refs_dir, f)))  # noqa: E731
    inv["finlam"] = {"train": cnt("Teklia__Newspapers-finlam__train.jsonl"),
                     "status": "train: banded tuning and trained-detector training (detector-seen, caveated); val 50 and "
                               "test 48 spent (step 1)"}
    inv["colorado"] = {"train": cnt("NealCaren__co-newspaper-layout-yolo__train.jsonl"),
                       "status": "left out: no licence stated (private tuning only), banded tuning set"}
    inv["europeana"] = {"pages": cnt("biglam__europeana-newspapers-ground-truth__train.jsonl"),
                        "status": "left out: its 50 pages were the step 1 test set"}


def sample(a, inv, lists, image):
    if a.sample_n and a.sample_image_prefix:
        cand = [p for p in lists["liberte-train-pool"] if image[p].startswith(a.sample_image_prefix)]
        pick = sorted(cand, key=lambda p: (key("2107:comboseg:smoke:", p), p))[:a.sample_n]
        by = {}
        for r in rows(os.path.join(a.refs_dir, LL % "train")):
            if r["page_id"] in set(pick):
                by[r["page_id"]] = r
        refs = {p: refsmod.row_reference(by[p]) for p in pick}
        json.dump({"for_publication": True, "pages": refs, "set": "liberte-smoke"}, open(os.path.join(a.out_dir, "smoke-liberte-refs.json"), "w"), sort_keys=True)
        json.dump({p: by[p]["image_path"] for p in pick}, open(os.path.join(a.out_dir, "smoke-liberte-paths.json"), "w"), sort_keys=True)
        inv["smoke"] = {"liberte": len(pick), "liberte_available_in_shard": len(cand)}
    if a.sample_cg_n:
        pick = sorted(lists["cg-spent-nontest"], key=lambda p: (key("2107:comboseg:smoke:", p), p))[:a.sample_cg_n]
        write_list(os.path.join(a.out_dir, "smoke-cg-ids.txt"), pick)
        inv.setdefault("smoke", {})["cg"] = len(pick)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--refs-dir", required=True)
    ap.add_argument("--ledger-root", required=True, help="directory holding diag2107/ and thr2107/")
    ap.add_argument("--cg-inventory", required=True)
    ap.add_argument("--engutter-tasks", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--sample-n", type=int, default=0)
    ap.add_argument("--sample-image-prefix")
    ap.add_argument("--sample-cg-n", type=int, default=0)
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    inv, lists = {}, {}
    image = liberte(a, inv, lists)
    chronicling_germany(a, inv, lists)
    beyond_words(a, inv, lists)
    engutter(a, inv)
    others(a, inv)
    for k, v in lists.items():
        inv.setdefault("lists_sha256", {})[k] = write_list(os.path.join(a.out_dir, k + ".txt"), v)
    sample(a, inv, lists, image)
    json.dump(inv, open(os.path.join(a.out_dir, "train-inventory.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(inv, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
