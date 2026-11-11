#!/usr/bin/env python3
"""Frees the download/ copies of finished usnm_quality bundle images (retention step).

`usnm-boinc quality submit` copies each unit's page.jpg into the download tree as
<job id>_page_jpg. Once the workunit is assimilated and every replica is over, BOINC never
sends that copy again, and nothing else reads it: collection reads units/<unit>/page.jpg
and the result spool. The copy is a bundle JPEG that staging re-encoded from scans, so it is
derived data, which file_deleter's --preserve_wu_files (kept for scans and files fetched from
LoC) need not keep. This step frees only those copies. It leaves everything
else in download/ alone: usnm_ocr's JP2 inputs (LoC's bytes), the loc/texts/quality json,
places files, models, and every other app's inputs.

  quality_retention.py list [--config /project/usnmocr/config.xml]
      [--db-ca /usr/local/share/usnm/mysql-ca.pem]
      [--db-defaults /usr/local/share/usnm/mysql-tls.cnf] --out FILE
  quality_retention.py prune --eligible FILE --download DIR --quality-root DIR
      --journal FILE [--commit SHA] [--delete] [--skip-cmp] [--max-files N] [--max-bytes N]

`list` runs in the server container and only reads the database. It writes one JSON line
per usnm_quality workunit whose assimilate_state is done and whose results are all over
(server_state 5): the workunit's id and name and its <id>_page_jpg input.

`prune` runs on the storage host as the share's owner, not root. For each listed file it
needs, or it leaves the file and journals why:
  - the file in the download tree, older than an hour;
  - the submitted bundle unit it came from (units/bundle-*/submitted names the workunit)
    under some campaign of --quality-root, whose quality.json has this job id and pages;
  - that unit's page.jpg, the same size as the job's page input and byte for byte the same
    as the download copy (full compare; --skip-cmp is for dry runs only), or the same inode.
    When prune-units already removed the unit's page.jpg, the copy must instead be the size
    and (full read; --skip-cmp skips it) sha256 that quality.json gives the page input.
Derived images may be deleted once nothing uses them (rule of 11 November 2026), so the
copy is deleted whether or not the pages' original scans are still on disk.
Without --delete it changes nothing on the share and reports what it would free. With
--delete it appends a journal line before each unlink and another after it, both fsynced,
so the journal records every file it removed and the --commit the run came from.
--max-files and --max-bytes bound a batch; a file that would pass --max-bytes ends the
batch before it is touched.

  quality_retention.py prune-units --quality-root DIR --campaign NAME --results-root DIR
      --scan-index FILE --scan-root DIR [--scan-root DIR] --journal FILE [--commit SHA]
      [--delete] [--max-units N] [--max-bytes N]

`prune-units` frees the unit copy, units/bundle-*/page.jpg, of bundles that quality collect no
longer needs (#1743). Collect takes a bundle's page sizes from its dims.json instead. It runs
on the storage host as the share's owner, like `prune`. A unit's page.jpg goes only when:
  - the unit is a submitted bundle whose quality.json lists its pages, and page.jpg is the
    size quality.json gives the page input;
  - its dims.json belongs to this job (version 1, the job id, every page's doc id and slice
    sha256 in order) and every page's JPEG header, read at its slice, gives the size the
    record gives;
  - the unit is fully collected: in the partial loader's state under --results-root/NAME,
    inventory.json has it canonical-present and frontier.json has loaded that fingerprint;
  - that inventory was written more than RECORD_SETTLE_SECS after dims.json, so the loaded
    fingerprint is one that collect computed with the record in the unit;
  - every page came from the on-prem feeder's hand-off (source handoff:scans/<doc_id>.jpg)
    and its scan is in --scan-index, and no page is LoC's own JPEG (its slice sha256 is the
    indexed scan's: the feeder passes a small grey JPEG through unchanged) unless that scan
    is under keep/ of a --scan-root at its indexed size. buffer/ doesn't count; the feeder
    evicts it. Files fetched from LoC are kept.
The image checks run last, so a unit not yet collected costs no image reads.
The same dry run, journal, --max-bytes rule and summary as `prune`; a page.jpg with other
hard links (a download copy an earlier version linked) counts no bytes.
"""
import argparse
import json
import os
import hashlib
import re
import sys
import time

APP = "usnm_quality"
PAGE_SUFFIX = "_page_jpg"
JOB_ID = re.compile(r"[0-9a-f]{64}")
MIN_AGE_SECS = 3600
CHUNK = 8 << 20
RESULT_OVER = 5
DIMS_FILE = "dims.json"
DIMS_VERSION = 1
MAX_DIMS_JSON = 1 << 20
# A full inventory refresh may run for its budget (an hour) before it writes inventory.json.
RECORD_SETTLE_SECS = 2 * 3600
# A bundle page's JPEG header (staging re-encodes the pages, so no large EXIF or ICC blocks).
JPEG_HEADER_WINDOW = 1 << 20
# The page source the on-prem feeder's hand-off gives each page (quality stage, handoff scans).
HANDOFF_SOURCE = "handoff:scans/{doc_id}.jpg"


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


def same_bytes(a, b):
    with open(a, "rb") as fa, open(b, "rb") as fb:
        while True:
            x = fa.read(CHUNK)
            if x != fb.read(CHUNK):
                return False
            if not x:
                return True


def check(entry, download, units, cmp, now):
    """(path, bytes freed, unit) for a copy to delete, or a reason string to leave it."""
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
    # Only bundle images are re-encoded derived copies; a single-page unit can hold LoC's
    # own JPEG unchanged, so it is out of scope.
    if not job.get("pages") or not os.path.basename(unit).startswith("bundle-"):
        return "not a bundle"
    page = os.path.join(unit, "page.jpg")
    want = ((job.get("inputs") or {}).get("page") or {}).get("bytes")
    try:
        ust = os.stat(page)
    except FileNotFoundError:
        if not os.path.exists(os.path.join(unit, DIMS_FILE)):
            return "unit page.jpg missing"
        # prune-units freed the unit's copy: the download copy must be the job's own input.
        if want != st.st_size:
            return "size differs from the job's page input"
        if cmp and file_sha256(path) != ((job.get("inputs") or {}).get("page") or {}).get("sha256"):
            return "bytes differ from the job's page input"
        return path, st.st_size, unit
    if ust.st_size != st.st_size or (want is not None and want != ust.st_size):
        return "size differs from unit page.jpg"
    if (ust.st_dev, ust.st_ino) == (st.st_dev, st.st_ino):
        # A hard link to the unit's copy (an earlier version linked some): dropping the
        # download name frees nothing now, the unit's page.jpg keeps the bytes.
        return path, 0, unit
    if cmp and not same_bytes(path, page):
        return "bytes differ from unit page.jpg"
    return path, st.st_size, unit


def file_sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            x = f.read(CHUNK)
            if not x:
                return h.hexdigest()
            h.update(x)


def jpeg_size(data):
    """(width, height) from a JPEG's frame header in `data`, its first bytes, or None."""
    if data[:2] != b"\xff\xd8":
        return None
    i = 2
    while i + 4 <= len(data):
        if data[i] != 0xFF:
            return None
        marker = data[i + 1]
        if marker == 0xFF:
            i += 1
            continue
        if marker == 0x01 or 0xD0 <= marker <= 0xD7:
            i += 2
            continue
        length = int.from_bytes(data[i + 2:i + 4], "big")
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            if i + 9 > len(data):
                return None
            height = int.from_bytes(data[i + 5:i + 7], "big")
            width = int.from_bytes(data[i + 7:i + 9], "big")
            return (width, height) if width and height else None
        if marker == 0xDA or length < 2:
            return None
        i += 2 + length
    return None


def unit_check(unit, results, scans):
    """(path, bytes freed, record) for a unit page.jpg to delete, or a reason to leave it."""
    name = os.path.basename(unit)
    page = os.path.join(unit, "page.jpg")
    try:
        st = os.stat(page)
    except FileNotFoundError:
        return "already pruned"
    if not os.path.isfile(os.path.join(unit, "submitted")):
        return "not submitted"
    try:
        with open(os.path.join(unit, "quality.json"), "rb") as f:
            job = json.load(f)
    except (OSError, ValueError):
        return "quality.json unreadable"
    pages = job.get("pages") or []
    if not pages or not name.startswith("bundle-"):
        return "not a bundle"
    if ((job.get("inputs") or {}).get("page") or {}).get("bytes") != st.st_size:
        return "page.jpg is not the size quality.json gives"
    dims_path = os.path.join(unit, DIMS_FILE)
    try:
        dst = os.stat(dims_path)
        with open(dims_path, "rb") as f:
            dims = json.loads(f.read(MAX_DIMS_JSON + 1))
    except FileNotFoundError:
        return "no dims.json"
    except (OSError, ValueError):
        return "dims.json unreadable"
    if (not isinstance(dims, dict) or dims.get("v") != DIMS_VERSION
            or dims.get("job") != job.get("id")
            or not isinstance(dims.get("pages"), list) or len(dims["pages"]) != len(pages)):
        return "dims.json is not this job's"
    sizes = []
    for d, p in zip(dims["pages"], pages):
        slice_ = ((p.get("inputs") or {}).get("page")) or {}
        if (not isinstance(d, dict) or d.get("doc_id") != p.get("doc_id")
                or d.get("sha256") != slice_.get("sha256")):
            return "dims.json is not this job's"
        w, h = d.get("width"), d.get("height")
        if not (isinstance(w, int) and isinstance(h, int) and w > 0 and h > 0):
            return "dims.json gives an empty size"
        sizes.append(((w, h), slice_))
    entry = results["inventory"].get(name)
    if not entry or entry.get("status") != "canonical-present":
        return "no canonical result in the inventory"
    if not entry.get("fingerprint") or results["loaded"].get(name) != entry["fingerprint"]:
        return "not loaded at its current fingerprint"
    if results["generated_at"] - dst.st_mtime < RECORD_SETTLE_SECS:
        return "inventory not newer than dims.json"
    # A page staging passed through unchanged is LoC's own JPEG, not a transcode: the unit keeps
    # it unless the original scan is on disk (every file fetched from LoC is kept).
    # Only the on-prem feeder's pages have known provenance: it reads each scan from the scan
    # server (checked against the index) and either transcodes it or passes it through. A page
    # fetched from LoC directly (its source a URL), or whose scan the index doesn't know, could
    # be LoC's own JPEG under another hash, so its unit stays.
    for d, p in zip(dims["pages"], pages):
        source = ((p.get("inputs") or {}).get("page") or {}).get("source", "")
        if source != HANDOFF_SOURCE.format(doc_id=p.get("doc_id")):
            return "a page did not come from the feeder's hand-off"
        known = scans["index"].get(p.get("doc_id"))
        if not known:
            return "a page's scan is not in the scan index"
        loc = known.get(d["sha256"])
        if loc is not None and not original_on_disk(p["doc_id"], loc, scans["roots"]):
            return "a page is LoC's own JPEG and its original is not in keep/"
    with open(page, "rb") as f:
        for (size, slice_) in sizes:
            offset, length = slice_.get("offset"), slice_.get("bytes")
            if not (isinstance(offset, int) and isinstance(length, int)
                    and 0 <= offset and offset + length <= st.st_size):
                return "page slice out of range"
            f.seek(offset)
            if jpeg_size(f.read(min(length, JPEG_HEADER_WINDOW))) != size:
                return "page header differs from dims.json"
    freed = st.st_size if st.st_nlink == 1 else 0
    return page, freed, {"job": job.get("id"), "nlink": st.st_nlink,
                         "fingerprint": entry["fingerprint"]}


def scan_index(path):
    """doc_id -> {sha256: (batch, bytes)} from the scan server's index of LoC's files."""
    index = {}
    with open(path) as f:
        for line in f:
            try:
                r = json.loads(line)
            except ValueError:
                continue
            if r.get("doc_id") and r.get("sha256") and r.get("batch"):
                index.setdefault(r["doc_id"], {})[r["sha256"]] = (r["batch"], r.get("bytes"))
    return index


def original_on_disk(doc_id, loc, roots):
    """Whether LoC's file for `doc_id` is under keep/ of a scan root, at its size. buffer/ is
    not enough: the feeder evicts buffer scans (quality_feed.py, delete_shipped)."""
    batch, size = loc
    m = re.fullmatch(r"([^_/]+)_(\d{4}-\d{2}-\d{2})_ed-(\d+)_seq-(\d+)", doc_id)
    if not m:
        return False
    lccn, date, ed, seq = m.groups()
    for root in roots:
        try:
            st = os.stat(os.path.join(root, "keep", batch, lccn, date, ed, seq + ".jpg"))
        except FileNotFoundError:
            continue
        if size is not None and st.st_size == size:
            return True
    return False


def results_state(results_root, campaign):
    """The partial loader's inventory and loaded fingerprints for `campaign`."""
    base = os.path.join(results_root, campaign)
    with open(os.path.join(base, "inventory.json"), "rb") as f:
        inventory = json.load(f)
    with open(os.path.join(base, "frontier.json"), "rb") as f:
        frontier = json.load(f)
    if (inventory.get("schema") != 1 or not isinstance(inventory.get("units"), dict)
            or not isinstance(inventory.get("generated_at"), (int, float))
            or frontier.get("schema") != 1 or not isinstance(frontier.get("loaded", {}), dict)):
        sys.exit("unexpected partial loader state under " + base)
    return {"inventory": inventory["units"], "loaded": frontier.get("loaded", {}),
            "generated_at": inventory["generated_at"]}


def prune_units(a):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", a.campaign):
        sys.exit("--campaign: a campaign name")
    results = results_state(a.results_root, a.campaign)
    scans = {"index": scan_index(a.scan_index), "roots": a.scan_root}
    units_dir = os.path.join(a.quality_root, a.campaign, "units")
    names = sorted(n for n in os.listdir(units_dir) if n.startswith("bundle-"))
    totals = {"units": 0, "bytes": 0}
    kept = {}
    kept_bytes = {}
    with open(a.journal, "a") as journal:
        for name in names:
            if totals["units"] >= a.max_units or totals["bytes"] >= a.max_bytes:
                break
            unit = os.path.join(units_dir, name)
            if not os.path.isdir(unit):
                continue
            got = unit_check(unit, results, scans)
            if isinstance(got, str):
                kept[got] = kept.get(got, 0) + 1
                if got != "already pruned":
                    try:
                        kept_bytes[got] = kept_bytes.get(got, 0) + os.stat(
                            os.path.join(unit, "page.jpg")).st_size
                    except OSError:
                        pass
                continue
            path, freed, info = got
            if totals["bytes"] + freed > a.max_bytes:
                break
            record = {"time": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "commit": a.commit,
                      "kind": "unit-page", "campaign": a.campaign, "unit": name,
                      "path": path, "bytes": freed, **info}
            if a.delete:
                journal_line(journal, dict(record, action="deleting"))
                os.unlink(path)
                journal_line(journal, dict(record, action="deleted"))
            totals["units"] += 1
            totals["bytes"] += freed
    summary = {"mode": "delete" if a.delete else "dry-run", "commit": a.commit,
               "campaign": a.campaign, "bundles": len(names), "units": totals["units"],
               "bytes": totals["bytes"], "gib": round(totals["bytes"] / 2**30, 2),
               "kept": kept, "kept_gib": {k: round(v / 2**30, 2) for k, v in kept_bytes.items()}}
    print(json.dumps(summary, sort_keys=True))
    return summary


def journal_line(journal, record):
    journal.write(json.dumps(record, sort_keys=True) + "\n")
    journal.flush()
    os.fsync(journal.fileno())


def prune(a):
    if a.skip_cmp and a.delete:
        sys.exit("--skip-cmp is for dry runs only")
    with open(a.eligible) as f:
        eligible = [json.loads(line) for line in f if line.strip()]
    units = submitted_units(a.quality_root)
    now = time.time()
    totals = {"files": 0, "bytes": 0, "units": set()}
    skipped = {}
    stamp = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    with open(a.journal, "a") as journal:
        for entry in eligible:
            if totals["files"] >= a.max_files or totals["bytes"] >= a.max_bytes:
                break
            got = check(entry, a.download, units, not a.skip_cmp, now)
            if isinstance(got, str):
                kind = got.split(" for ")[0]
                skipped[kind] = skipped.get(kind, 0) + 1
                if got != "not in download":
                    journal_line(journal, {"time": stamp, "commit": a.commit, "action": "keep",
                                           "file": entry["file"], "wu_id": entry["wu_id"],
                                           "reason": got})
                continue
            path, size, unit = got
            if totals["bytes"] + size > a.max_bytes:
                break
            record = {"time": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "commit": a.commit,
                      "file": entry["file"], "path": path, "bytes": size, "unit": unit,
                      "wu_id": entry["wu_id"], "wu_name": entry["wu_name"]}
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
    pr.add_argument("--journal", required=True)
    pr.add_argument("--commit", default="unknown")
    pr.add_argument("--delete", action="store_true")
    pr.add_argument("--skip-cmp", action="store_true")
    pr.add_argument("--max-files", type=int, default=sys.maxsize)
    pr.add_argument("--max-bytes", type=int, default=sys.maxsize)
    pu = sub.add_parser("prune-units")
    pu.add_argument("--quality-root", required=True)
    pu.add_argument("--campaign", required=True)
    pu.add_argument("--results-root", required=True)
    pu.add_argument("--scan-index", required=True)
    pu.add_argument("--scan-root", action="append", required=True)
    pu.add_argument("--journal", required=True)
    pu.add_argument("--commit", default="unknown")
    pu.add_argument("--delete", action="store_true")
    pu.add_argument("--max-units", type=int, default=sys.maxsize)
    pu.add_argument("--max-bytes", type=int, default=sys.maxsize)
    a = ap.parse_args(argv)
    if a.cmd == "prune-units":
        prune_units(a)
    elif a.cmd == "list":
        n = list_eligible(a.config, a.db_ca, a.db_defaults, a.out)
        print(f"{n} files listed", file=sys.stderr)
    else:
        prune(a)


if __name__ == "__main__":
    main()
