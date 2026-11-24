#!/usr/bin/env python3
"""Draw the US German pages for #2127's test of B against LoC (comment 7132047952, "Pages").

Pool. select_bank_pages.py's frame, built from the same inputs (a keep-set scan, LoC ALTO in the bulk store with
a locstats row with words, a `ger` page-language decision where the batch has one, and none of the US German
evaluation artefacts or #835's rule), restricted to the bank's titles (the LCCNs of --bank-pages), and less every
page of the arm C exclusion manifest (--exclusion-manifest; its pages_sha256 is checked).

Sample. Pages ranked by sha256("2127:us:" + page_id); taken in rank order, skipping a page whose title already
has --cap, until --n are chosen.

Writes in --out: pages.txt (sorted ids, one per line, trailing newline), pages.jsonl (one record a page, with its
scan path, batch and archive), selection.tsv (english_pages.py's --selection: doc_id, path_form, archive, format;
the scan sha256 is filled when the scans are copied) and summary.json (the list's sha256, title count, pool size, what was left out).

    select_us_pages.py --pilot-pages fraktur-pilot-pages.jsonl --keep-listing keep-listing.tsv \
        --locbulk-listing locbulk-listing.txt --locstats-dir locstats --langs-dir langs \
        --exclude de968 --exclude check-de --exclude calib --exclude fraktur-pilot-pages.jsonl \
        --exclude scripts/ocr-node/fixtures/reading-order \
        --bank-pages bank-select/out/pages.jsonl --exclusion-manifest bank-select/out/exclusion-manifest.json \
        --n 60 --cap 2 --out us-pages
"""
import argparse, collections, hashlib, json, os, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import select_bank_pages as sbp  # noqa: E402


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pilot-pages", required=True); ap.add_argument("--keep-listing", required=True)
    ap.add_argument("--locbulk-listing", required=True); ap.add_argument("--locstats-dir", required=True)
    ap.add_argument("--langs-dir", required=True)
    ap.add_argument("--exclude", action="append", required=True)
    ap.add_argument("--bank-pages", required=True, help="the bank's pages.jsonl: its titles define the pool")
    ap.add_argument("--exclusion-manifest", required=True)
    ap.add_argument("--n", type=int, default=60); ap.add_argument("--cap", type=int, default=2)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)

    man = json.load(open(a.exclusion_manifest))
    man_txt = "".join(i + "\n" for i in sorted(man["pages"]))
    assert hashlib.sha256(man_txt.encode()).hexdigest() == man["pages_sha256"], "manifest pages do not match its sha256"
    manifest = set(man["pages"])
    bank_titles = {json.loads(l)["lccn"] for l in open(a.bank_pages) if l.strip()}

    F = sbp.build_frame(a)
    left = collections.Counter(F["reasons"])
    pool = []
    for r in F["frame"]:
        if r["lccn"] not in bank_titles:
            left["not_a_bank_title"] += 1
        elif r["page_id"] in manifest:
            left["in_exclusion_manifest"] += 1
        else:
            r = dict(r, rank_key=sbp.key("2127:us:" + r["page_id"]))
            pool.append(r)
    pool.sort(key=lambda r: r["rank_key"])
    chosen, per_title = [], collections.Counter()
    for r in pool:
        if len(chosen) == a.n:
            break
        if per_title[r["lccn"]] < a.cap:
            chosen.append(r)
            per_title[r["lccn"]] += 1
    assert len(chosen) == a.n, f"only {len(chosen)} pages available"
    ids = sorted(r["page_id"] for r in chosen)
    assert len(set(ids)) == len(ids) and not (set(ids) & manifest) and not (set(ids) & F["excluded"])

    os.makedirs(a.out, exist_ok=True)
    txt = "".join(i + "\n" for i in ids)
    open(os.path.join(a.out, "pages.txt"), "w").write(txt)
    digest = hashlib.sha256(txt.encode()).hexdigest()
    with open(os.path.join(a.out, "pages.jsonl"), "w") as f:
        for r in sorted(chosen, key=lambda r: r["page_id"]):
            f.write(json.dumps(r, sort_keys=True) + "\n")
    with open(os.path.join(a.out, "selection.tsv"), "w") as f:
        f.write("doc_id\tpath_form\tarchive\tformat\tdamage_rate\tscan_sha256\tscan_path\n")
        for r in sorted(chosen, key=lambda r: r["page_id"]):
            lccn, date, ed, seq = r["page_id"].split("_")
            f.write("\t".join([r["page_id"], f"{lccn}/{date}/{ed}/{seq}", r["archive"], r["scan_format"], "", "",
                               r["scan_path"]]) + "\n")
    summary = {
        "pages": len(ids), "pages_sha256": digest, "n": a.n, "cap": a.cap,
        "titles": len(per_title), "bank_titles": len(bank_titles),
        "pool_pages": len(pool), "pool_titles": len({r["lccn"] for r in pool}),
        "frame_pages": len(F["frame"]), "left_out": dict(left),
        "exclusion_manifest_sha256": man["pages_sha256"], "exclusion_manifest_count": len(manifest),
        "excluded_ids_by_source": {k: len(v) for k, v in F["excl_by_source"].items()},
        "by_title": dict(sorted(per_title.items())),
        "by_decade": dict(sorted(collections.Counter(r["decade"] for r in chosen).items())),
        "scan_formats": dict(collections.Counter(r["scan_format"] for r in chosen)),
        "inputs_sha256": {"pilot_pages": sbp.sha256_file(a.pilot_pages), "keep_listing": sbp.sha256_file(a.keep_listing),
                          "locbulk_listing": sbp.sha256_file(a.locbulk_listing),
                          "bank_pages": sbp.sha256_file(a.bank_pages), "exclusion_manifest": sbp.sha256_file(a.exclusion_manifest),
                          "select_us_pages": sbp.sha256_file(os.path.abspath(__file__))},
    }
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), indent=1)
    print(json.dumps({k: summary[k] for k in ("pages", "pages_sha256", "titles", "pool_pages", "pool_titles",
                                              "frame_pages", "left_out", "by_decade")}, indent=1))


if __name__ == "__main__":
    sys.exit(main())
