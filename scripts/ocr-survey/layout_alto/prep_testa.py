#!/usr/bin/env python3
"""List the pages of #2103's word tables for test A of #2107 step 2, as inputs for pagecache.py, columns.py and asmodel.py.

Method sources: step 1 of #2107 (#2112; jscancella/NYTribuneOCRExperiments findText_usingSums.py, GPL-3.0,
reimplemented); #2103 (word tables); American Stories (Dell et al., arXiv:2308.12477).

    prep_testa.py --set NAME,WORDS.jsonl,ALTO_DIR [--set ...] --paths-out paths.json --pages-out pages.json

paths.json maps page id -> `<ALTO_DIR>/<stem>.jpg` (pagecache.py's --paths); pages.json is {"pages": {page id: {}}}
(columns.py and asmodel.py's --pages-from). The stem is the rows' `source_image` without its extension.
"""
import argparse
import json
import os
import sys


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--set", action="append", required=True, help="NAME,WORDS.jsonl,ALTO_DIR (repeat)")
    ap.add_argument("--paths-out", required=True)
    ap.add_argument("--pages-out", required=True)
    a = ap.parse_args(argv)
    paths = {}
    for spec in a.set:
        name, words, adir = spec.split(",", 2)
        with open(words) as f:
            for ln in f:
                r = json.loads(ln)
                stem = r["source_image"].rsplit(".", 1)[0]
                p = os.path.join(adir, stem + ".jpg")
                if paths.setdefault(r["page_id"], p) != p:
                    raise SystemExit(f"page {r['page_id']} has two images")
    with open(a.paths_out, "w") as f:
        json.dump(paths, f, sort_keys=True)
    with open(a.pages_out, "w") as f:
        json.dump({"pages": {k: {} for k in sorted(paths)}}, f, sort_keys=True)
    print(f"{len(paths)} pages")
    return 0


if __name__ == "__main__":
    sys.exit(main())
