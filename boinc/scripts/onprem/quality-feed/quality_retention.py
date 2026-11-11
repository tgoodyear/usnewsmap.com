#!/usr/bin/env python3
"""Frees the download/ copies of finished usnm_quality bundle images (retention step).

`usnm-boinc quality submit` copies each unit's page.jpg into the download tree as
<job id>_page_jpg. Once the workunit is assimilated and every replica is over, BOINC never
sends that copy again, and nothing else reads it: collection reads units/<unit>/page.jpg
and the result spool. The copy is a JPEG re-encoded from scans the scan server keeps, so it
is derived data, and file_deleter's --preserve_wu_files (which keeps scans and files fetched
from LoC) no longer has to keep it. This step frees only those copies. It leaves everything
else in download/ alone: usnm_ocr's JP2 inputs (LoC's bytes), the loc/texts/quality json,
places files, models, and every other app's inputs.

  quality_retention.py list [--config /project/usnmocr/config.xml]
      [--db-ca /usr/local/share/usnm/mysql-ca.pem]
      [--db-defaults /usr/local/share/usnm/mysql-tls.cnf] --out FILE
  quality_retention.py prune --eligible FILE --download DIR --quality-root DIR
      --scan-index FILE --scan-root DIR [--scan-root DIR ...] --journal FILE
      [--commit SHA] [--delete] [--skip-cmp] [--max-files N] [--max-bytes N]

`list` runs in the server container and only reads the database. It writes one JSON line
per usnm_quality workunit whose assimilate_state is done and whose results are all over
(server_state 5): the workunit's id and name and its <id>_page_jpg input.

`prune` runs on the storage host as the share's owner, not root. For each listed file it
needs, or it leaves the file and journals why:
  - the file in the download tree, older than an hour;
  - the submitted unit it came from (units/<unit>/submitted names the workunit) under some
    campaign of --quality-root, whose quality.json has this job id;
  - that unit's page.jpg, a different inode of the same size as the job's page input, and
    byte for byte the same as the download copy (full compare; --skip-cmp is for dry runs
    only).
If every page of the job has its original scan in --scan-index and on disk under a
--scan-root (keep/ or buffer/), the copy is deleted. If any page lacks one, the bundle image
is that page's only local image: the copy is replaced by a hard link to the unit's page.jpg
(link beside it, then rename over it), so both paths stay readable on one physical copy, and
the journal marks it only_local_image and never_prune.
Without --delete it changes nothing on the share and reports what it would free. With
--delete it appends a journal line before each unlink or link and another after it, both
fsynced, so the journal records every file it changed and the --commit the run came from.
--max-files and --max-bytes bound a batch (links count toward both).
"""
import argparse
import json
import os
import re
import sys
import time

APP = "usnm_quality"
PAGE_SUFFIX = "_page_jpg"
JOB_ID = re.compile(r"[0-9a-f]{64}")
DOC = re.compile(r"([a-z0-9]{1,16})_(\d{4}-\d{2}-\d{2})_ed-(\d+)_seq-(\d+)")
MIN_AGE_SECS = 3600
CHUNK = 8 << 20
RESULT_OVER = 5


def list_eligible(config, db_ca, db_defaults, out):
    import xml.etree.ElementTree as ET
    import MySQLdb  # only in the server image

    cfg = ET.parse(config).find("config")
    db = MySQLdb.connect(host=cfg.findtext("db_host"), user=cfg.findtext("db_user"),
                         passwd=cfg.findtext("db_passwd"), db=cfg.findtext("db_name"),
                         ssl={"ca": db_ca}, read_default_file=db_defaults,
                         read_default_group="client")
    n = 0
    try:
        cur = db.cursor()
        cur.execute("SET SESSION MAX_EXECUTION_TIME=300000")
        cur.execute("START TRANSACTION READ ONLY")
        cur.execute(
            "SELECT w.id, w.name, w.xml_doc FROM workunit w JOIN app a ON a.id = w.appid "
            "WHERE a.name = %s AND w.assimilate_state = 2 AND NOT EXISTS "
            "(SELECT 1 FROM result r WHERE r.workunitid = w.id AND r.server_state <> %s)",
            (APP, RESULT_OVER))
        tmp = out + ".tmp" if out != "-" else None
        with (open(tmp, "w") if tmp else os.fdopen(os.dup(1), "w")) as f:
            for wid, name, xml in cur:
                if isinstance(xml, bytes):
                    xml = xml.decode("utf-8", "replace")
                for file in page_inputs(xml or ""):
                    f.write(json.dumps({"wu_id": int(wid), "wu_name": name, "file": file}) + "\n")
                    n += 1
            f.flush()
            if tmp:
                os.fsync(f.fileno())
        if tmp:
            os.replace(tmp, out)
    finally:
        db.close()
    return n


def page_inputs(xml):
    """The <job id>_page_jpg names in a workunit's xml_doc."""
    return [m for m in re.findall(r"<name>([^<]+)</name>", xml)
            if m.endswith(PAGE_SUFFIX) and JOB_ID.fullmatch(m[:-len(PAGE_SUFFIX)])]


def download_path(download, name, fanout=1024):
    """BOINC's dir_hier_path: md5 of the name, characters 1-7 as hex, mod fanout."""
    import hashlib
    h = hashlib.md5(name.encode()).hexdigest()
    return os.path.join(download, "%x" % (int(h[1:8], 16) % fanout), name)


def submitted_units(quality_root):
    """Workunit name -> unit directory, from every campaign's units/*/submitted marker."""
    found = {}
    for camp in sorted(os.listdir(quality_root)):
        units = os.path.join(quality_root, camp, "units")
        if not os.path.isdir(units):
            continue
        for unit in os.listdir(units):
            marker = os.path.join(units, unit, "submitted")
            try:
                with open(marker) as f:
                    wu = f.read(512).strip()
            except (FileNotFoundError, NotADirectoryError):
                continue
            if wu:
                found[wu] = os.path.join(units, unit)
    return found


def scan_index(path):
    """doc_id -> batch, from the scan server's index (one JSON object a line)."""
    batches = {}
    with open(path) as f:
        for line in f:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get("doc_id") and row.get("batch"):
                batches[row["doc_id"]] = row["batch"]
    return batches


def scan_present(doc, batches, roots):
    """Whether doc's original scan is on disk under a root's keep/ or buffer/."""
    m = DOC.fullmatch(doc)
    batch = batches.get(doc)
    if not m or not batch:
        return False
    rel = os.path.join(batch, m[1], m[2], str(int(m[3])))
    seq = int(m[4])
    for root in roots:
        for kept in ("keep", "buffer"):
            for ext in ("jp2", "jpg"):
                if os.path.isfile(os.path.join(root, kept, rel, f"{seq}.{ext}")):
                    return True
    return False


def same_bytes(a, b):
    with open(a, "rb") as fa, open(b, "rb") as fb:
        while True:
            x = fa.read(CHUNK)
            if x != fb.read(CHUNK):
                return False
            if not x:
                return True


def check(entry, download, units, batches, roots, cmp, now):
    """("delete" or "link", path, bytes, unit, detail) for a file to act on, or a reason
    string to leave it. "link" is a copy whose bundle has a page without its original: the
    bundle image is then that page's only local image, so the copy becomes a hard link to the
    unit's page.jpg (one physical copy, both paths readable) and is never deleted."""
    name = entry["file"]
    if not name.endswith(PAGE_SUFFIX) or not JOB_ID.fullmatch(name[:-len(PAGE_SUFFIX)]):
        return "not a quality page input"
    path = download_path(download, name)
    try:
        st = os.stat(path)
    except FileNotFoundError:
        return "not in download"
    if now - st.st_mtime < MIN_AGE_SECS:
        return "written in the last hour"
    unit = units.get(entry["wu_name"])
    if not unit:
        return "no submitted unit"
    try:
        with open(os.path.join(unit, "quality.json"), "rb") as f:
            job = json.load(f)
    except (OSError, ValueError):
        return "unit quality.json unreadable"
    if job.get("id") != name[:-len(PAGE_SUFFIX)]:
        return "unit job id differs"
    page = os.path.join(unit, "page.jpg")
    want = ((job.get("inputs") or {}).get("page") or {}).get("bytes")
    try:
        ust = os.stat(page)
    except FileNotFoundError:
        return "unit page.jpg missing"
    if (ust.st_dev, ust.st_ino) == (st.st_dev, st.st_ino):
        return "already one copy (hard link)"
    if ust.st_size != st.st_size or (want is not None and want != ust.st_size):
        return "size differs from unit page.jpg"
    docs = [p.get("doc_id") for p in job.get("pages") or []] or [job.get("doc_id")]
    missing = [d for d in docs if not d or not scan_present(d, batches, roots)]
    if cmp and not same_bytes(path, page):
        return "bytes differ from unit page.jpg"
    if missing:
        return ("link", path, st.st_size, unit,
                f"original scan missing for {len(missing)} page(s), first {missing[0]}")
    return "delete", path, st.st_size, unit, None


def link_copy(path, page):
    """Replaces path with a hard link to page, atomically: link beside it, then rename."""
    tmp = os.path.join(os.path.dirname(path), f".{os.path.basename(path)}.link")
    if os.path.lexists(tmp):
        os.unlink(tmp)
    os.link(page, tmp)
    os.replace(tmp, path)


def journal_line(journal, record):
    journal.write(json.dumps(record, sort_keys=True) + "\n")
    journal.flush()
    os.fsync(journal.fileno())


def prune(a):
    if a.skip_cmp and a.delete:
        sys.exit("--skip-cmp is for dry runs only")
    roots = [r for r in a.scan_root if os.path.isdir(r)]
    if not roots:
        sys.exit("no --scan-root exists")
    with open(a.eligible) as f:
        eligible = [json.loads(line) for line in f if line.strip()]
    units = submitted_units(a.quality_root)
    batches = scan_index(a.scan_index)
    now = time.time()
    totals = {"files": 0, "bytes": 0, "units": set()}
    linked = {"files": 0, "bytes": 0}
    skipped = {}
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    with open(a.journal, "a") as journal:
        for entry in eligible:
            if (totals["files"] + linked["files"] >= a.max_files
                    or totals["bytes"] + linked["bytes"] >= a.max_bytes):
                break
            got = check(entry, a.download, units, batches, roots, not a.skip_cmp, now)
            if isinstance(got, str):
                kind = got.split(" for ")[0]
                skipped[kind] = skipped.get(kind, 0) + 1
                if got != "not in download":
                    journal_line(journal, {"time": stamp, "commit": a.commit, "action": "keep",
                                           "file": entry["file"], "wu_id": entry["wu_id"],
                                           "reason": got})
                continue
            act, path, size, unit, detail = got
            record = {"time": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "commit": a.commit,
                      "file": entry["file"], "path": path, "bytes": size, "unit": unit,
                      "wu_id": entry["wu_id"], "wu_name": entry["wu_name"]}
            if act == "link":
                # The bundle image is some page's only local image: one copy stays, forever.
                record.update(reason=detail, only_local_image=True, never_prune=True)
                if a.delete:
                    journal_line(journal, dict(record, action="linking"))
                    link_copy(path, os.path.join(unit, "page.jpg"))
                    journal_line(journal, dict(record, action="linked"))
                linked["files"] += 1
                linked["bytes"] += size
            else:
                if a.delete:
                    journal_line(journal, dict(record, action="deleting"))
                    os.unlink(path)
                    journal_line(journal, dict(record, action="deleted"))
                totals["files"] += 1
                totals["bytes"] += size
                totals["units"].add(unit)
    summary = {"mode": "delete" if a.delete else "dry-run", "commit": a.commit,
               "eligible": len(eligible), "files": totals["files"], "bytes": totals["bytes"],
               "gib": round(totals["bytes"] / 2**30, 2), "bundles": len(totals["units"]),
               "linked": linked["files"], "linked_gib": round(linked["bytes"] / 2**30, 2),
               "kept": skipped}
    print(json.dumps(summary, sort_keys=True))
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    ls = sub.add_parser("list")
    ls.add_argument("--config", default="/project/usnmocr/config.xml")
    ls.add_argument("--db-ca", default="/usr/local/share/usnm/mysql-ca.pem")
    ls.add_argument("--db-defaults", default="/usr/local/share/usnm/mysql-tls.cnf")
    ls.add_argument("--out", required=True)
    pr = sub.add_parser("prune")
    pr.add_argument("--eligible", required=True)
    pr.add_argument("--download", required=True)
    pr.add_argument("--quality-root", required=True)
    pr.add_argument("--scan-index", required=True)
    pr.add_argument("--scan-root", action="append", required=True)
    pr.add_argument("--journal", required=True)
    pr.add_argument("--commit", default="unknown")
    pr.add_argument("--delete", action="store_true")
    pr.add_argument("--skip-cmp", action="store_true")
    pr.add_argument("--max-files", type=int, default=sys.maxsize)
    pr.add_argument("--max-bytes", type=int, default=sys.maxsize)
    a = ap.parse_args(argv)
    if a.cmd == "list":
        n = list_eligible(a.config, a.db_ca, a.db_defaults, a.out)
        print(f"{n} files listed", file=sys.stderr)
    else:
        prune(a)


if __name__ == "__main__":
    main()
