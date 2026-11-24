#!/usr/bin/env python3
"""Page sample for the English pick-between-readings test (#2122, route 2): Tesseract against LoC on body text.

Reads only metadata (no image is opened). The universe is every English Chronicling America page we hold, in the
sense of the #2107 page list (enjudge select_pages.py, plan comment 7103801519):

  held     the doc_id has a fetch-index row (full resolution) whose sha256 is in usnm-scan-confirm's list;
  English  damage audit v2 for that row's batch version: lang == "eng" and decision == "detected";
  ALTO     the row's bulk archive <batch>_ver<NN>.tar.bz2 is in --locbulk (LoC's ALTO is read from it at build time,
           after the locstats index confirms the page is in it).

Left out: every page of the #2107 and #2122 ledgers, each counted in the report:
  --exclude-ids FILE (repeatable): plain doc_id lists, e.g. the #2107 enjudge pages-300.txt and reserves-50.txt;
  en-asgold: the Chronicling America pages of --asgold/selection.tsv (ca_page);
  Overproof: the 18 pages of --overproof (overproof-ca-pages.json);
  Beyond Words: every page of --refs-dir/biglam__loc_beyond_words__{train,val}.jsonl matched to an index row by its
    batch path (lccn, reel, issue folder, image file), as #2107 did, and also every page of an issue (lccn, date,
    edition) that holds a Beyond Words page, since a scan fetched after the index snapshot would not match by path.

Ranking: sha256("2122:pick:" + doc_id), ascending (ties by doc_id). Selection, in that order:
  1. the pilot (--pilot pages), then the sample (--n), then reserves (--reserves), each drawn from what is left;
  2. at most --per-title pages of one title (lccn) over pilot, sample and reserves together;
  3. spread by decade: each draw first gives every decade --decade-floor pages (or all it has), then shares the
     rest in proportion to the decades' shares of the universe (largest remainder; allocate()). A draw too small for
     the floors (the pilot) gives every decade an equal share instead. A decade's pages are taken in rank order;
     a page whose title is at its cap is skipped, and what a decade then cannot fill is shared equally among the
     others. The estimate is weighted back to the universe's decade shares (the plan), so the floor buys per-decade
     description without biasing the overall figure.

Writes --out/{pilot,sample,reserves}.tsv (rank in the universe, decade, key, doc_id, path form, batch, batch
version, archive, scan sha256, scan format, damage rate), exclusions.json (the ids left out, by ledger) and
report.json (counts, the decade quotas and the sha256 of each list file).

    select_pages.py --inputs DIR --refs-dir DIR --asgold DIR --locbulk DIR --exclude-ids F [--exclude-ids F ...]
        --out DIR [--n 200] [--reserves 50] [--pilot 20] [--per-title 5]
"""
import argparse
import collections
import glob
import hashlib
import json
import os
import re
import sys

PREFIX = "2122:pick:"
DOC = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
COLUMNS = ["rank", "decade", "sha256_key", "doc_id", "path_form", "batch", "batch_version", "archive", "scan_sha256",
           "format", "damage_rate"]


def key(doc_id):
    return hashlib.sha256((PREFIX + doc_id).encode()).hexdigest()


def path_form(doc_id):
    m = DOC.match(doc_id)
    return "%s/%s/ed-%d/seq-%d" % (m["lccn"], m["date"], int(m["ed"]), int(m["seq"]))


def decade(doc_id):
    return int(DOC.match(doc_id)["date"][:4]) // 10 * 10


def issue(doc_id):
    m = DOC.match(doc_id)
    return (m["lccn"], m["date"], int(m["ed"]))


def archive_name(batch, batch_version):
    return "%s_ver%02d.tar.bz2" % (batch, int(batch_version or 1))


def iiif_tail(iiif_base):
    """(lccn, reel, folder, file) from ...service:ndnp:<st>:<batch>:data:<lccn>[:<reel>]:<folder>:<file>"""
    ident = iiif_base.split("/image-services/iiif/")[1].split("/")[0]
    parts = ident.split(":")
    rest = parts[parts.index("data") + 1:]
    reel = rest[1] if len(rest) == 4 else ""
    return (rest[0], reel, rest[-2], rest[-1].split(".")[0])


def path_tail(url):
    """(lccn, reel, folder, file) from a batch path .../data/<lccn>[/<reel>]/<folder>/<file>.<ext>"""
    rest = url.split("/data/", 1)[1].split("/")
    reel = rest[1] if len(rest) == 4 else ""
    return (rest[0], reel, rest[-2], rest[-1].split(".")[0])


def jsonl(path):
    with open(path) as f:
        for ln in f:
            if ln.strip():
                yield json.loads(ln)


def tsv(path):
    with open(path) as f:
        head = f.readline().rstrip("\n").split("\t")
        for ln in f:
            yield dict(zip(head, ln.rstrip("\n").split("\t")))


def quotas(available, n, order=None):
    """Equal shares of n over the decades, water-filled: {decade: quota}, each at most what the decade has. A
    remainder that does not divide goes one page each to the decades first in `order` (default: ascending)."""
    left = dict(available)
    out = {d: 0 for d in available}
    order = list(order) if order is not None else sorted(available)
    need = n
    while need > 0 and any(v > 0 for v in left.values()):
        open_ = [d for d in order if left.get(d, 0) > 0]
        share, extra = divmod(need, len(open_))
        for i, d in enumerate(open_):
            take = min(left[d], share + (1 if i < extra else 0))
            out[d] += take
            left[d] -= take
            need -= take
        if share == 0 and extra == 0:
            break
    return out


def allocate(available, n, weights, floor, order):
    """{decade: quota}: every decade first gets min(what it has, floor); the rest of n is shared in proportion to
    `weights` (the decade's share of the universe), largest remainder first (ties in `order`), never more than a
    decade has; what a full decade cannot take is shared again among the others. When the floors alone exceed n,
    the decades share n equally instead (quotas())."""
    base = {d: min(available[d], floor) for d in available}
    if sum(base.values()) >= n:
        return quotas(available, n, order)
    out = dict(base)
    need = n - sum(out.values())
    pos = {d: i for i, d in enumerate(order)}
    while need > 0:
        open_ = [d for d in available if out[d] < available[d]]
        if not open_:
            break
        total = sum(weights.get(d, 0) for d in open_) or len(open_)
        exact = {d: need * (weights.get(d, 0) if total != len(open_) else 1) / total for d in open_}
        take = {d: min(int(exact[d]), available[d] - out[d]) for d in open_}
        rest = need - sum(take.values())
        for d in sorted(open_, key=lambda d: (-(exact[d] - int(exact[d])), pos.get(d, 0))):
            if rest <= 0:
                break
            if out[d] + take[d] < available[d]:
                take[d] += 1
                rest -= 1
        if not any(take.values()):
            break
        for d in open_:
            out[d] += take[d]
            need -= take[d]
    return out


def draw(ranked, n, per_title, title_count, taken, weights=None, floor=None):
    """Up to n pages from `ranked` (doc_ids in rank order, not yet taken), spread by decade and at most per_title per
    lccn (title_count is updated). With `weights` and `floor` the first plan is allocate()'s, else equal quotas; a
    decade that cannot fill its quota (title cap) leaves its shortfall to the others in equal shares. Returns the drawn
    doc_ids in rank order and the pages taken per decade."""
    by_dec = collections.defaultdict(list)
    for d in ranked:
        if d not in taken:
            by_dec[decade(d)].append(d)

    def eligible(ds):
        return [d for d in ds if title_count[DOC.match(d)["lccn"]] < per_title]

    rank_of = {d: i for i, d in enumerate(ranked)}
    chosen, final_quota = [], collections.Counter()
    need = n
    # Re-plan while some decade cannot fill its quota (title cap): its shortfall is shared among the others.
    while need > 0:
        avail = {dec: len(eligible(ds)) for dec, ds in by_dec.items()}
        if not any(avail.values()):
            break
        # the remainder goes to decades in the order of their best-ranked eligible page, so the ranking fixes it
        first = {dec: next((rank_of[d] for d in eligible(ds)), len(ranked)) for dec, ds in by_dec.items()}
        order = sorted(avail, key=first.get)
        if weights is not None and floor is not None and need == n:
            q = allocate(avail, need, weights, floor, order)
        else:
            q = quotas(avail, need, order)
        got = 0
        for dec in sorted(q):
            k = 0
            for d in by_dec[dec]:
                if k >= q[dec]:
                    break
                t = DOC.match(d)["lccn"]
                if title_count[t] >= per_title:
                    continue
                title_count[t] += 1
                chosen.append(d)
                taken.add(d)
                k += 1
            by_dec[dec] = [d for d in by_dec[dec] if d not in taken]
            final_quota[dec] += k
            got += k
        need -= got
        if got == 0:
            break
    return sorted(chosen, key=rank_of.get), dict(sorted(final_quota.items()))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--inputs", required=True, help="index-full.jsonl, confirmed-sha256.txt, damage-v2/, overproof-ca-pages.json")
    ap.add_argument("--refs-dir", required=True, help="biglam__loc_beyond_words__{train,val}.jsonl")
    ap.add_argument("--asgold", required=True, help="the en-asgold folder with selection.tsv")
    ap.add_argument("--locbulk", required=True, help="the folder of LoC's bulk OCR archives (<batch>_verNN.tar.bz2)")
    ap.add_argument("--exclude-ids", action="append", default=[], help="a file of doc_ids to leave out; repeatable")
    ap.add_argument("--out", required=True)
    ap.add_argument("--n", type=int, default=200)
    ap.add_argument("--reserves", type=int, default=50)
    ap.add_argument("--pilot", type=int, default=20)
    ap.add_argument("--per-title", type=int, default=5)
    ap.add_argument("--decade-floor", type=int, default=3, help="pages every decade gets first (or all it has); the rest "
                    "of a draw follows the decades' shares of the universe. A floor of n or more gives equal quotas")
    a = ap.parse_args(argv)
    import pyarrow.parquet as pq

    os.makedirs(a.out, exist_ok=True)
    rep = collections.OrderedDict()
    conf = set()
    with open(os.path.join(a.inputs, "confirmed-sha256.txt")) as f:
        rep["confirm_list"] = json.loads(f.readline()[1:].strip())
        conf.update(ln.strip() for ln in f)
    by_doc = collections.defaultdict(list)
    by_tail = collections.defaultdict(set)
    for r in jsonl(os.path.join(a.inputs, "index-full.jsonl")):
        if r.get("resolution", "full") != "full" or not DOC.match(r["doc_id"]):
            continue
        r["present"] = r["sha256"] in conf
        by_doc[r["doc_id"]].append(r)
        by_tail[iiif_tail(r["iiif_base"])].add(r["doc_id"])
    held = {d for d, rs in by_doc.items() if any(x["present"] for x in rs)}
    rep["index"] = {"doc_ids": len(by_doc), "held": len(held)}

    want = collections.defaultdict(set)
    for d in held:
        for x in by_doc[d]:
            if x["present"]:
                want[(x["batch"], int(x.get("batch_version") or 1))].add(d)
    english, damage = {}, {}
    for f in glob.glob(os.path.join(a.inputs, "damage-v2", "*.parquet")):
        batch, bv = os.path.basename(f)[:-len(".parquet")].rsplit("_ver", 1)
        docs = want.get((batch, int(bv)))
        if not docs:
            continue
        for x in pq.read_table(f, columns=["doc_id", "lang", "decision", "damage_rate"]).to_pylist():
            if x["doc_id"] in docs and x["lang"] == "eng" and x["decision"] == "detected":
                english[x["doc_id"]] = (batch, int(bv))
                damage[x["doc_id"]] = x["damage_rate"]
    rep["english_detected_held"] = len(english)
    archives = {os.path.basename(p) for p in glob.glob(os.path.join(a.locbulk, "*.tar.bz2"))}
    with_alto = {d for d, (b, v) in english.items() if archive_name(b, v) in archives}
    rep["english_with_bulk_archive"] = len(with_alto)

    excl = collections.OrderedDict()
    for path in a.exclude_ids:
        ids = {ln.strip() for ln in open(path) if ln.strip() and not ln.startswith("#")}
        excl["file:" + os.path.basename(path)] = ids
    excl["en-asgold"] = {r["ca_page"] for r in tsv(os.path.join(a.asgold, "selection.tsv")) if r.get("ca_page")}
    op = set()
    for v in json.load(open(os.path.join(a.inputs, "overproof-ca-pages.json"))).values():
        m = re.search(r"/resource/(?P<lccn>[^/]+)/(?P<date>\d{4}-\d{2}-\d{2})/ed-(?P<ed>\d+)/", v["page_url"])
        op.add("%s_%s_ed-%s_seq-%d" % (m["lccn"], m["date"], m["ed"], v["seq"]))
    excl["overproof-ca"] = op
    bw, bw_issues = set(), set()
    for sp in ("train", "val"):
        for r in jsonl(os.path.join(a.refs_dir, "biglam__loc_beyond_words__%s.jsonl" % sp)):
            u = r["meta"].get("source_url") or ""
            if "/data/" in u:
                bw |= by_tail.get(path_tail(u), set())
            ident = r["meta"].get("ca_identity") or {}
            if ident.get("lccn") and ident.get("date"):
                bw_issues.add((ident["lccn"], ident["date"], int(ident.get("edition") or 1)))
    excl["beyond-words"] = bw
    excl["beyond-words-issues"] = {d for d in with_alto if issue(d) in bw_issues}
    allx = set().union(*excl.values())
    rep["exclusions"] = {k: {"ids": len(v), "in_universe_before_exclusion": len(v & with_alto)} for k, v in excl.items()}
    rep["exclusions"]["union_in_universe"] = len(allx & with_alto)
    with open(os.path.join(a.out, "exclusions.json"), "w") as f:
        json.dump({k: sorted(v) for k, v in excl.items()}, f, indent=1, sort_keys=True)

    universe = sorted(with_alto - allx)
    rep["universe"] = len(universe)
    rep["universe_by_decade"] = dict(sorted(collections.Counter(decade(d) for d in universe).items()))
    rep["universe_titles"] = len({DOC.match(d)["lccn"] for d in universe})
    ranked = sorted(universe, key=lambda d: (key(d), d))
    weights = rep["universe_by_decade"]
    rank_of = {d: i + 1 for i, d in enumerate(ranked)}
    titles, taken = collections.Counter(), set()
    lists = {}
    for name, n in (("pilot", a.pilot), ("sample", a.n), ("reserves", a.reserves)):
        got, q = draw(ranked, n, a.per_title, titles, taken, weights, a.decade_floor)
        lists[name] = got
        rep[name] = {"pages": len(got), "by_decade": q, "titles": len({DOC.match(d)["lccn"] for d in got})}
    for name, ids in lists.items():
        path = os.path.join(a.out, name + ".tsv")
        with open(path, "w") as f:
            f.write("\t".join(COLUMNS) + "\n")
            for d in ids:
                x = next(x for x in by_doc[d] if x["present"])
                b, v = english[d]
                f.write("\t".join(str(c) for c in (
                    rank_of[d], decade(d), key(d), d, path_form(d), b, v, archive_name(b, v), x["sha256"],
                    x.get("format", ""), "" if damage[d] is None else round(damage[d], 6))) + "\n")
        rep[name]["sha256"] = hashlib.sha256(open(path, "rb").read()).hexdigest()
    rep["max_pages_per_title"] = max(titles.values()) if titles else 0
    json.dump(rep, open(os.path.join(a.out, "report.json"), "w"), indent=1)
    print(json.dumps(rep, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
