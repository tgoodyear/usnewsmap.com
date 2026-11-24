#!/usr/bin/env python3
"""Write the reserved-test page lists of the #2107 ledger as Chronicling America id files (#2107, model v2).

Each subcommand writes one id per line in doc_id form (<lccn>_<yyyy-mm-dd>_ed-<n>_seq-<k>). Where a
source names only an issue, the id gets seq-0; alto_weak.py's issue-level exclusion (on by default)
then drops every page of that issue.

  bwval     Beyond Words validation pages: refs rows (layout_refs) biglam__loc_beyond_words__val.jsonl,
            meta.ca_identity {lccn, date, edition} -> issue ids.
  asgold    en-asgold: asgold-ca-map.json (s9-heldout/en-asgold) "ca.doc_id" and selection.tsv "ca_page".
  overproof Overproof dataset 3 (Chronicling America) as curated in HIPE-OCRepair-2026: the
            overproof-combined JSONL files (data/v0.9/overproof/en/); document_metadata.document_id
            <lccn>-<n>-<reel>-<yyyymmdd><ed>-<file>
            (gold.py's CA_DOC_ID) -> issue ids. The file number is a reel image number, not a page
            sequence, so only the issue is known.
  ids       a plain list (e.g. ~/code/enjudge-2107/out/pages-300.txt), copied after a format check.

    exclusions.py bwval --refs FILE --out bw-val.txt
    exclusions.py asgold --map asgold-ca-map.json --selection selection.tsv --out asgold.txt
    exclusions.py overproof --hipe-dir DIR --out overproof.txt
    exclusions.py ids --src pages-300.txt --out en2107.txt
"""
import argparse
import csv
import glob
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import alto_weak  # noqa: E402

CA_DOC_ID = re.compile(r"^(?P<lccn>sn\d+)-\d+-(?P<reel>[0-9A-Za-z]+)-(?P<date>\d{8})(?P<ed>\d{1,2})-(?P<file>\d{4})$")


def issue_id(lccn, date, ed):
    return "%s_%s_ed-%d_seq-0" % (lccn, date, int(ed))


def write(out, ids):
    ids = sorted(set(ids))
    bad = [i for i in ids if not alto_weak.DOC_RE.match(i)]
    if bad:
        raise SystemExit("not doc ids: %s" % bad[:5])
    with open(out, "w") as f:
        f.write("".join(i + "\n" for i in ids))
    print(json.dumps({"ids": len(ids), "out": out}))


def cmd_bwval(a):
    ids = []
    with open(a.refs) as f:
        for ln in f:
            if ln.strip():
                ci = json.loads(ln)["meta"]["ca_identity"]
                ids.append(issue_id(ci["lccn"], ci["date"], ci["edition"]))
    write(a.out, ids)


def cmd_asgold(a):
    ids = []
    with open(a.map) as f:
        for d in json.load(f):
            if d.get("ca") and d["ca"].get("doc_id"):
                ids.append(d["ca"]["doc_id"])
    if a.selection:
        with open(a.selection, newline="") as f:
            ids += [r["ca_page"] for r in csv.DictReader(f, delimiter="\t") if r.get("ca_page")]
    write(a.out, ids)


def cmd_overproof(a):
    ids, seen = [], 0
    for p in sorted(glob.glob(os.path.join(a.hipe_dir, "**", "*overproof*.jsonl"), recursive=True)):
        with open(p) as f:
            for ln in f:
                if not ln.strip():
                    continue
                d = json.loads(ln)
                meta = d.get("document_metadata") or {}
                for key in ("document_id", "doc_id", "id"):
                    v = meta.get(key) or d.get(key)
                    m = CA_DOC_ID.match(str(v)) if v else None
                    if m:
                        dt = m.group("date")
                        ids.append(issue_id(m.group("lccn"), "%s-%s-%s" % (dt[:4], dt[4:6], dt[6:]), m.group("ed")))
                        break
                seen += 1
    if not ids:
        raise SystemExit("no Overproof CA ids in %d rows under %s" % (seen, a.hipe_dir))
    write(a.out, ids)


def cmd_ids(a):
    write(a.out, alto_weak.read_id_list(a.src))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("bwval")
    s.add_argument("--refs", required=True)
    s.add_argument("--out", required=True)
    s = sub.add_parser("asgold")
    s.add_argument("--map", required=True)
    s.add_argument("--selection")
    s.add_argument("--out", required=True)
    s = sub.add_parser("overproof")
    s.add_argument("--hipe-dir", required=True)
    s.add_argument("--out", required=True)
    s = sub.add_parser("ids")
    s.add_argument("--src", required=True)
    s.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    {"bwval": cmd_bwval, "asgold": cmd_asgold, "overproof": cmd_overproof, "ids": cmd_ids}[a.cmd](a)
    return 0


if __name__ == "__main__":
    sys.exit(main())
