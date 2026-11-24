#!/usr/bin/env python3
"""The test pages no #2128 training step may use, as one manifest the recipe's tools read: test set v2's
pages (#1620, the same 29 as v1) and the 210 GHN test pages. `sets.*.pages` is what `usnm-glyphs clean
--exclude-manifest` and `chr_real_lines.py` read; `exclude_from_training` is what `chr_synth_lines.py` reads.

    exclude_manifest.py --testset-manifest v2/manifest.json --ghn-test-pages ghn-test-pages.txt --out exclude.json
"""
import argparse
import json

import common


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--testset-manifest", action="append", required=True)
    ap.add_argument("--ghn-test-pages", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    loc = set()
    for path in a.testset_manifest:
        m = json.load(open(path, encoding="utf-8"))
        for s in m["sets"].values():
            if isinstance(s, dict) and isinstance(s.get("pages"), list):
                loc.update(s["pages"])
    ghn = set(common.read_pages(a.ghn_test_pages))
    assert len(ghn) == 210 and common.sha256_list(ghn) == "7d7ffee27ce9a17fd20a308e1f75932aec4afdf1bd7940a75642c52f2121534c"
    out = {"name": "chr2128-training-exclusions", "issue": 2128,
           "sets": {"testset_v2": {"pages": sorted(loc)}, "ghn_test": {"pages": sorted(ghn)}},
           "exclude_from_training": sorted(loc | ghn)}
    json.dump(out, open(a.out, "w"), indent=1)
    print(json.dumps({"testset_pages": len(loc), "ghn_test_pages": len(ghn)}))


if __name__ == "__main__":
    main()
