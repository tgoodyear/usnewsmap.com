#!/usr/bin/env python3
"""Resolves published BOINC OCR pages to the host that read them (#1956).

Reads files only and writes a TSV for provenance_load.sql; it never touches a database. Run it
before db_purge removes the workunit and result rows it depends on (30 days after a workunit is
done). It needs pyarrow (to read the page parquet parts).

  boinc_provenance.py --pages DIR --wu-tsv FILE --av-tsv FILE --bundles DIR --out FILE [--skip FILE]

--pages    a copy of curated ocr-local/pages (the parquet parts); the rows with node
           `boinc-<job12>` are the BOINC pages. `ocr_engine` supplies image=sha256:<12 hex>.
--wu-tsv   the BOINC database, read only (mysql --batch, with a header row):
             SELECT w.id, w.appid, w.name, w.canonical_resultid, w.file_delete_state,
                    w.create_time, COALESCE(r.hostid, ''), COALESCE(r.app_version_id, ''),
                    COALESCE(r.id, ''), COALESCE(r.outcome, ''), COALESCE(h.os_name, ''),
                    COALESCE(h.p_vendor, ''), COALESCE(h.p_model, '')
             FROM workunit w
             LEFT JOIN result r ON r.id = w.canonical_resultid AND w.canonical_resultid > 0
             LEFT JOIN host h ON h.id = r.hostid
             WHERE w.appid IN (1, 11) AND w.id >= :lo AND w.id < :hi;
           Run it in id ranges of a few thousand and concatenate the files (one header each).
--av-tsv   SELECT av.id, av.appid, av.version_num, p.name, av.plan_class FROM app_version av
           JOIN platform p ON p.id = av.platformid WHERE av.appid IN (1, 11);
--bundles  the `<bundle>_ocr_bundle_json` files of the bundle workunits, from the project's
           download directory (kept after a workunit is purged): their member list maps a
           job to its bundle `usnm_ocrb_<bundle>`.

Job12 is matched by prefix against the job ids of single workunits (`usnm_<job id>`) and of the
bundle members. A job that ran in two workunits on one host resolves (`+dup_wu_same_host`); on
different hosts it is `ambiguous_duplicate_wu` with `candidate_host_ids`; two job ids sharing a
prefix are `ambiguous_prefix`. The family is from the host row, as in the spot checks (#1954);
a host with an empty or unknown CPU vendor keeps its host id and gets no family.
"""
import argparse
import collections
import datetime
import glob
import json
import os
import sys

X86_VENDORS = ("GenuineIntel", "AuthenticAMD", "HygonGenuine")


def family(os_name, vendor):
    if vendor in X86_VENDORS:
        return "amd64"
    if (os_name == "Darwin" and vendor == "Apple") or vendor.lower().startswith("arm"):
        return "arm64"
    return ""


def resolve_host(w, av):
    """The status of a job's canonical workunit `w` and the host fields it gives the row.

    A host whose CPU vendor is empty or unknown stays resolved: it keeps its host id, with an
    empty family (NULL in the table), because the host is what a hold matches on."""
    if w["canon"] == 0:
        return "no_canonical", {}
    if w["hostid"] == "":
        return "result_missing", {}
    return "resolved", dict(canon=w["canon"], hostid=int(w["hostid"]), avid=int(w["avid"]),
                            platform=av.get(w["avid"], ""), family=family(w["os"], w["vendor"]))


def read_skip(path):
    """The readings to leave out, one `doc_id<TAB>node` a line: those the table already
    resolved (see provenance-daily.sh), so a daily run only looks at new and unresolved pages."""
    if not path:
        return set()
    with open(path) as fh:
        return {tuple(line.rstrip("\n").split("\t")[:2]) for line in fh if line.strip()}


def read_tsv(files):
    for f in files:
        with open(f) as fh:
            for i, line in enumerate(fh):
                if i:
                    yield line.rstrip("\n").split("\t")


def boinc_rows(pages_dir):
    import pyarrow.parquet as pq

    for f in sorted(glob.glob(os.path.join(pages_dir, "**", "*.parquet"), recursive=True)):
        cols = ["doc_id", "node", "ocr_engine"]
        t = pq.read_table(f, columns=cols).to_pylist()
        for r in t:
            if r["node"].startswith("boinc-"):
                yield r


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--pages", required=True)
    ap.add_argument("--wu-tsv", required=True, nargs="+")
    ap.add_argument("--av-tsv", required=True)
    ap.add_argument("--bundles", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--skip", help="doc_id<TAB>node lines of readings already resolved: left out of --out "
                    "(the upsert never lowers a resolved row, so they would only cost work)")
    a = ap.parse_args()

    wu = {}
    for p in read_tsv(a.wu_tsv):
        wu[p[2]] = dict(wu_id=int(p[0]), canon=int(p[3]), hostid=p[6], avid=p[7], os=p[10], vendor=p[11])
    av = {p[0]: p[3] for p in read_tsv([a.av_tsv])}
    members = collections.defaultdict(list)  # job id -> [(bundle workunit name, image digest)]
    for f in glob.glob(os.path.join(a.bundles, "**", "*_ocr_bundle_json"), recursive=True):
        d = json.load(open(f))
        for pg in d["pages"]:
            members[pg["job"]["id"]].append(("usnm_ocrb_" + d["id"], pg["job"]["engine"]["image"]))
    cand = collections.defaultdict(list)  # job12 -> [(job id, workunit name, how)]
    for n in wu:
        if n.startswith("usnm_") and not n.startswith("usnm_ocrb_"):
            cand[n[5:17]].append((n[5:], n, "wu_name"))
    for job, found in members.items():
        for bundle, _ in found:
            cand[job[:12]].append((job, bundle, "bundle_member"))

    skip = read_skip(a.skip)
    now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    n = collections.Counter()
    seen = set()
    with open(a.out, "w") as out:
        for r in boinc_rows(a.pages):
            if (r["doc_id"], r["node"]) in seen or (r["doc_id"], r["node"]) in skip:
                continue
            seen.add((r["doc_id"], r["node"]))
            job12 = r["node"][6:]
            image12 = r["ocr_engine"].split("image=sha256:")[1][:12]
            # A bundle request outlives its purged workunit: only surviving workunits count.
            cs = [c for c in cand.get(job12, []) if c[1] in wu]
            jobs = {c[0] for c in cs}
            rec = dict(job_id=None, wu_id=None, canon=None, hostid=None, avid=None, platform=None,
                       family=None, via=None, digest=None, cands=None)
            if not cs:
                status = "host_unknown"
                # The bundle request may still name the job and its image.
                known = {j for j in members if j.startswith(job12)}
                if len(known) == 1:
                    rec["job_id"] = next(iter(known))
                    rec["digest"] = members[rec["job_id"]][0][1]
            elif len(jobs) > 1:
                status = "ambiguous_prefix"
            else:
                job = next(iter(jobs))
                rec["job_id"] = job
                rec["digest"] = members[job][0][1] if job in members else None
                infos = [(how, name, wu[name]) for _, name, how in cs]
                resolved = [i for i in infos if i[2]["canon"] > 0 and i[2]["hostid"] != ""]
                hosts = {i[2]["hostid"] for i in resolved}
                if len(hosts) > 1:
                    status = "ambiguous_duplicate_wu"
                    rec["cands"] = "{" + ",".join(map(str, sorted(map(int, hosts)))) + "}"
                else:
                    how, name, w = (resolved or infos)[-1]
                    rec["wu_id"] = w["wu_id"]
                    rec["via"] = how if len(cs) == 1 else how + "+dup_wu_same_host"
                    status, found = resolve_host(w, av)
                    rec.update(found)
            n[status] += 1
            row = [r["doc_id"], r["node"], rec["job_id"], rec["wu_id"], rec["canon"], rec["hostid"], rec["avid"],
                   rec["platform"], rec["family"], rec["via"], now, status, image12, rec["digest"], rec["cands"]]
            out.write("\t".join("\\N" if x is None or x == "" else str(x) for x in row) + "\n")
    print(json.dumps(dict(n), sort_keys=True), file=sys.stderr)


if __name__ == "__main__":
    main()
