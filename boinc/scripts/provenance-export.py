#!/usr/bin/env python3
"""The BOINC half of the daily page provenance run (#1956). Read-only.

Runs as root inside the BOINC server container, with the server's own database credentials
(`config.xml`), and writes one gzip tar to stdout:

    wu.tsv        the workunit query of scripts/onprem/quality/boinc_provenance.py (header row first)
    av.tsv        the app_version query of the same (header row first)
    bundles/      every `<bundle>_ocr_bundle_json` of the download tree, which outlives a purged workunit

It writes nothing in the container and takes no other credential. The database is read in one
`START TRANSACTION READ ONLY` of bounded SELECTs (an id range at a time).

    ssh root@<boinc host> 'pct exec 230 -- podman exec -i usnm-boinc-server python3 - export' \
        < boinc/scripts/provenance-export.py > export.tar.gz

The caller is scripts/onprem/quality/provenance-export-fetch.sh. Nothing passes through a
workstation.
"""

import argparse
import io
import os
import sys
import tarfile
import time
import xml.etree.ElementTree as ET

WU_SQL = (
    "SELECT w.id, w.appid, w.name, w.canonical_resultid, w.file_delete_state, w.create_time, "
    "COALESCE(r.hostid, ''), COALESCE(r.app_version_id, ''), COALESCE(r.id, ''), "
    "COALESCE(r.outcome, ''), COALESCE(h.os_name, ''), COALESCE(h.p_vendor, ''), COALESCE(h.p_model, '') "
    "FROM workunit w LEFT JOIN result r ON r.id = w.canonical_resultid AND w.canonical_resultid > 0 "
    "LEFT JOIN host h ON h.id = r.hostid "
    "WHERE w.appid IN %s AND w.id >= %s AND w.id < %s"
)
WU_HEADER = (
    "id appid name canonical_resultid file_delete_state create_time hostid app_version_id result_id "
    "outcome os_name p_vendor p_model"
).split()
AV_SQL = (
    "SELECT av.id, av.appid, av.version_num, p.name, av.plan_class FROM app_version av "
    "JOIN platform p ON p.id = av.platformid WHERE av.appid IN %s"
)
AV_HEADER = "id appid version_num platform plan_class".split()
STEP = 5000


def clean(v):
    """One TSV cell. BOINC names and CPU strings carry no tabs or newlines; strip them anyway."""
    return str(v).replace("\t", " ").replace("\n", " ").replace("\r", " ")


def tsv(header, rows):
    out = ["\t".join(header)]
    out.extend("\t".join(clean(c) for c in row) for row in rows)
    return ("\n".join(out) + "\n").encode()


def add(tar, name, data, mtime):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mtime = mtime
    tar.addfile(info, io.BytesIO(data))


def export(args):
    import MySQLdb  # only in the server image

    cfg = ET.parse(args.config).find("config")
    db = MySQLdb.connect(
        host=cfg.findtext("db_host"),
        user=cfg.findtext("db_user"),
        passwd=cfg.findtext("db_passwd"),
        db=cfg.findtext("db_name"),
        ssl={"ca": args.db_ca},
        read_default_file=args.db_defaults,
        read_default_group="client",
    )
    try:
        cur = db.cursor()
        cur.execute("SET SESSION TRANSACTION READ ONLY")
        cur.execute("SET SESSION MAX_EXECUTION_TIME=120000")
        cur.execute("START TRANSACTION READ ONLY")
        cur.execute("SELECT id FROM app WHERE name IN %s", (tuple(args.apps),))
        appids = tuple(r[0] for r in cur.fetchall())
        if len(appids) != len(args.apps):
            sys.exit(f"expected apps {args.apps}, found ids {appids}")
        cur.execute("SELECT COALESCE(MIN(id), 0), COALESCE(MAX(id), 0) FROM workunit WHERE appid IN %s", (appids,))
        lo, hi = cur.fetchone()
        wu_rows = []
        for start in range(lo, hi + 1, STEP):
            cur.execute(WU_SQL, (appids, start, min(start + STEP, hi + 1)))
            wu_rows.extend(cur.fetchall())
        cur.execute(AV_SQL, (appids,))
        av_rows = cur.fetchall()
        db.rollback()
    finally:
        db.close()
    if not wu_rows or not av_rows:
        sys.exit("the database returned no workunits or no app versions")

    now = int(time.time())
    bundles = 0
    with tarfile.open(fileobj=sys.stdout.buffer, mode="w|gz") as tar:
        add(tar, "wu.tsv", tsv(WU_HEADER, wu_rows), now)
        add(tar, "av.tsv", tsv(AV_HEADER, av_rows), now)
        for sub in sorted(os.scandir(args.download), key=lambda e: e.name):
            if not sub.is_dir():
                continue
            for f in sorted(os.scandir(sub.path), key=lambda e: e.name):
                if f.name.endswith("_ocr_bundle_json") and f.is_file():
                    with open(f.path, "rb") as fh:
                        add(tar, f"bundles/{f.name}", fh.read(), int(f.stat().st_mtime))
                    bundles += 1
    print(f"export: {len(wu_rows)} workunits, {len(av_rows)} app versions, {bundles} bundle files", file=sys.stderr)


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="command", required=True)
    e = sub.add_parser("export", help="write the export tar.gz to stdout (server container, root)")
    e.add_argument("--config", default="/project/usnmocr/config.xml")
    e.add_argument("--download", default="/project/usnmocr/download")
    e.add_argument("--apps", nargs="+", default=["usnm_ocr", "usnm_ocr_native"],
                   help="the app names whose workunits carry pages (default: the OCR apps)")
    e.add_argument("--db-ca", default="/usr/local/share/usnm/mysql-ca.pem")
    e.add_argument("--db-defaults", default="/usr/local/share/usnm/mysql-tls.cnf")
    args = p.parse_args()
    export(args)


if __name__ == "__main__":
    main()
