"""Make tess_pick.py's --pages file for English Chronicling America pages (#2122, route 2): LoC's ALTO from the bulk
archives and a whole-page Tesseract reading of each scan. No network requests.

  python3 english_pages.py --selection pilot.tsv --scans DIR --locbulk /data/locbulk/ocr --locstats /data/locstats \
      --work DIR [--workers 3]

--selection is select_pages.py's TSV (doc_id, path_form, batch, archive, damage_rate, ...). --scans holds each page's scan
as <doc_id>.<format>, or is a held-scan root read in place (keep/ and buffer/ by batch; scan_path()); its sha256 is
checked against the selection's scan_sha256.

ALTO. The page's archive is the one the selection names; the locstats index (<archive stem>.parquet, the per-batch
index of what each archive holds) must list the doc_id, or the page is left out as `not_in_locstats`. The archive is
then read as a stream, once for all the pages it holds, and only until the last of them is found: only regular
`ocr.xml` members whose path names a wanted page are read, and nothing is extracted as a filesystem path (as
archive_alto.py); archives are read --workers at a time. Each ALTO is written to WORK/alto/<doc_id>.xml with its member name and sha256 in WORK/alto.jsonl.

Tesseract. Tesseract `eng` at PSM 3 with the worker's arguments, as #2122's arm W (segfirst.py at b045416f):
`tesseract <scan> <out> -l eng --psm 3 --dpi 300 txt tsv` with OMP_THREAD_LIMIT=1, on the scan file. Outputs:
WORK/tess/<doc_id>.{txt,tsv}; a failure is recorded, not retried. WORK/meta.json records the Tesseract version,
the traineddata path and sha256, this script's sha256 and the arguments.

A JP2 on which Tesseract dies with SIGFPE (status 136) is read once more from LoC's IIIF JPEG of the page, as the
production worker does (crates/usnm-ocr-worker/src/worker.rs, #454): the JPEG must be in --fallback-jpegs as
<doc_id>.jpg (this script fetches nothing). The page's image is then the JPEG, for its crops too, and the page is
listed in the report's `jpeg_fallback`. Without a JPEG, or if that run fails too, the page is left out as
`tesseract_status_136` (or `tesseract_failed` for any other failure).

With --resume, ALTO already extracted (sha256 as recorded) and Tesseract runs already done on the same image (a
marker per outcome: .ok, .136, .failed) are kept; a JP2 that crashed is not read again, only its JPEG fallback.

Writes WORK/pages.jsonl, one row per page with both readings (tess_pick.py's input; paths relative to WORK), and
WORK/prepare-report.json (pages read, left out and why).
"""
import argparse
import csv
import hashlib
import json
import multiprocessing as mp
import os
import re
import subprocess
import tarfile

DOC = re.compile(r"^(?P<lccn>[a-z0-9]+)_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)$")
MAX_ALTO = 32 * 1024 * 1024
TESS_ARGS = ["-l", "eng", "--psm", "3", "--dpi", "300"]


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def member_doc(name):
    """The doc_id an archive member's path names, or None (archive_alto.member_doc)."""
    while name.startswith("./"):
        name = name[2:]
    fields = name.split("/")
    if name.startswith("/") or any(p in ("", ".", "..") for p in fields) or len(fields) < 5:
        return None
    ed, seq, filename = fields[-3:]
    if filename != "ocr.xml" or not ed.startswith("ed-") or not seq.startswith("seq-"):
        return None
    if re.fullmatch(r"\d{4}-\d{2}-\d{2}", fields[-4]):
        title, date = fields[-5:-3]
    elif len(fields) >= 7:
        title, date = fields[-7], "-".join(fields[-6:-3])
    else:
        return None
    doc = f"{title}_{date}_{ed}_{seq}"
    return doc if DOC.fullmatch(doc) else None


def canonical(doc_id):
    """doc_id with unpadded edition and sequence numbers, as archive members name them."""
    m = DOC.match(doc_id)
    return "%s_%s_ed-%d_seq-%d" % (m["lccn"], m["date"], int(m["ed"]), int(m["seq"]))


def read_members(archive_path, wanted, out_dir):
    """{doc_id: {member, sha256, bytes}} for the wanted docs found in one archive, reading it as a stream and
    stopping once every wanted doc is found."""
    want = {canonical(d): d for d in wanted}
    found = {}
    with tarfile.open(archive_path, mode="r|bz2") as tf:
        for m in tf:
            doc = member_doc(m.name)
            if doc is None:
                continue
            doc = canonical(doc)
            if doc not in want or want[doc] in found or not m.isreg() or m.size > MAX_ALTO:
                continue
            raw = tf.extractfile(m).read()
            d = want[doc]
            path = os.path.join(out_dir, d + ".xml")
            tmp = path + ".tmp"
            with open(tmp, "wb") as f:
                f.write(raw)
            os.replace(tmp, path)
            found[d] = {"member": m.name, "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}
            if len(found) == len(want):
                break
    return found


def _read(job):
    path, listed, out_dir = job
    return read_members(path, set(listed), out_dir)


def in_locstats(locstats_dir, archive, doc_ids):
    import pyarrow.parquet as pq
    path = os.path.join(locstats_dir, archive[:-len(".tar.bz2")] + ".parquet")
    if not os.path.exists(path):
        return set()
    have = set(pq.read_table(path, columns=["doc_id", "archive"]).column("doc_id").to_pylist())
    return {d for d in doc_ids if d in have or canonical(d) in have}


def tesseract(job):
    doc_id, image, out_dir = job
    base = os.path.join(out_dir, doc_id)
    env = dict(os.environ, OMP_THREAD_LIMIT="1")
    p = subprocess.run(["tesseract", image, base] + TESS_ARGS + ["txt", "tsv"], env=env, stdin=subprocess.DEVNULL,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    # A marker per outcome, so a later run (--resume) neither reads a page again nor retries a JP2 that crashed.
    marker = base + (".ok" if p.returncode == 0 else ".136" if is_status_136(p.returncode) else ".failed")
    with open(marker, "w") as f:
        f.write(image + "\n")
    return doc_id, p.returncode, p.stderr[-2000:]


def done_before(out_dir, doc_id, image):
    """(returncode, '') from a marker an earlier run left for this image, else None."""
    base = os.path.join(out_dir, doc_id)
    for ext, code in ((".ok", 0), (".136", 136), (".failed", 1)):
        if os.path.exists(base + ext) and open(base + ext).read().strip() == image:
            return code
    return None


def is_status_136(code):
    """SIGFPE: 136 from a shell or container, -8 from subprocess (the worker's Exited::is_status_136)."""
    return code in (136, -8)


def tess_meta():
    v = subprocess.run(["tesseract", "--version"], capture_output=True, text=True)
    lp = subprocess.run(["tesseract", "--list-langs"], capture_output=True, text=True)
    tessdata = None
    for ln in (lp.stdout + lp.stderr).splitlines():
        if "List of available languages in" in ln:
            tessdata = ln.split('"')[1] if '"' in ln else ln.split(" in ")[-1].rstrip(":")
    td = os.path.join(tessdata, "eng.traineddata") if tessdata else None
    return {"tesseract_version": (v.stdout + v.stderr).splitlines()[0].strip(), "args": TESS_ARGS,
            "omp_thread_limit": 1, "traineddata": td,
            "traineddata_sha256": sha256_file(td) if td and os.path.exists(td) else None}


def scan_path(scans, row):
    """The page's scan: <scans>/<doc_id>.<format>, or the held layout <scans>/{keep,buffer}/<batch>/<lccn>/<date>/<ed>/<seq>.<format>
    (as /tank/ocr/scans and its cold copy hold them, read in place). None when neither exists."""
    d, fmt = row["doc_id"], row.get("format") or "jp2"
    m = DOC.match(d)
    exts = [fmt] + (["jpg"] if fmt == "jpeg" else [])  # the index says "jpeg"; held files are .jpg
    for ext in exts:
        for p in [os.path.join(scans, "%s.%s" % (d, ext))] + [
                os.path.join(scans, area, row.get("batch", ""), m["lccn"], m["date"], str(int(m["ed"])), "%d.%s" % (int(m["seq"]), ext))
                for area in ("keep", "buffer")]:
            if os.path.isfile(p):
                return p
    return None


def source_url(doc_id):
    m = DOC.match(doc_id)
    return "https://www.loc.gov/resource/%s/%s/ed-%d/?sp=%d" % (m["lccn"], m["date"], int(m["ed"]), int(m["seq"]))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--selection", required=True)
    ap.add_argument("--scans", required=True)
    ap.add_argument("--locbulk", required=True)
    ap.add_argument("--locstats", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--resume", action="store_true", help="keep ALTO and Tesseract results an earlier run in --work left "
                    "(checked by sha256 and by the image read), and redo only the rest")
    ap.add_argument("--fallback-jpegs", help="LoC's IIIF JPEGs as <doc_id>.jpg, read when Tesseract dies with SIGFPE on the "
                    "JP2, as the production worker does (#454)")
    a = ap.parse_args(argv)
    for sub in ("alto", "tess"):
        os.makedirs(os.path.join(a.work, sub), exist_ok=True)
    with open(a.selection) as f:
        sel = list(csv.DictReader(f, delimiter="\t"))
    rep = {"pages": len(sel), "left_out": {}}
    left = rep["left_out"]
    scans = {}
    for r in sel:
        d = r["doc_id"]
        path = scan_path(a.scans, r)
        if path is None:
            left[d] = "no_scan"
        elif r.get("scan_sha256") and sha256_file(path) != r["scan_sha256"]:
            left[d] = "scan_sha256_differs"
        else:
            scans[d] = path
    by_archive = {}
    for r in sel:
        if r["doc_id"] in scans:
            by_archive.setdefault(r["archive"], []).append(r["doc_id"])
    alto, jobs = {}, []
    kept = {}
    if a.resume and os.path.exists(os.path.join(a.work, "alto.jsonl")):
        for r in map(json.loads, open(os.path.join(a.work, "alto.jsonl"))):
            path = os.path.join(a.work, "alto", r["doc_id"] + ".xml")
            if os.path.exists(path) and sha256_file(path) == r["sha256"]:
                kept[r["doc_id"]] = {k: v for k, v in r.items() if k != "doc_id"}
    for archive, docs in sorted(by_archive.items()):
        for d in [d for d in docs if d in kept]:
            alto[d] = kept[d]
        docs = [d for d in docs if d not in kept]
        if not docs:
            continue
        listed = in_locstats(a.locstats, archive, docs)
        for d in set(docs) - listed:
            left[d] = "not_in_locstats"
        if listed:
            jobs.append((os.path.join(a.locbulk, archive), sorted(listed), os.path.join(a.work, "alto")))
    # one archive per worker: each is a single bz2 stream, read until its last wanted page
    with mp.Pool(a.workers) as pool:
        for n, ((path, listed, _), got) in enumerate(zip(jobs, pool.imap(_read, jobs)), 1):
            archive = os.path.basename(path)
            print(f"alto {n}/{len(jobs)} {archive}: {len(got)}/{len(listed)} page(s)", flush=True)
            for d in set(listed) - set(got):
                left[d] = "not_in_archive"
            for d, v in got.items():
                alto[d] = dict(v, archive=archive)
    with open(os.path.join(a.work, "alto.jsonl"), "w") as f:
        for d in sorted(alto):
            f.write(json.dumps({"doc_id": d, **alto[d]}, sort_keys=True) + "\n")
    meta = tess_meta()
    meta["script_sha256"] = sha256_file(os.path.abspath(__file__))
    meta["selection_sha256"] = sha256_file(a.selection)
    json.dump(meta, open(os.path.join(a.work, "meta.json"), "w"), indent=1, sort_keys=True)
    tess_dir = os.path.join(a.work, "tess")
    earlier = {d: done_before(tess_dir, d, scans[d]) for d in sorted(alto)} if a.resume else {}
    jobs = [(d, scans[d], tess_dir) for d in sorted(alto) if earlier.get(d) is None]
    results = [(d, code, "(earlier run)") for d, code in earlier.items() if code is not None]
    errors, fallback = {}, {}
    with mp.Pool(a.workers) as pool:
        for n, (d, code, err) in enumerate(results + list(pool.imap_unordered(tesseract, jobs)), 1):
            print(f"tesseract {n}/{len(jobs)} {d} rc={code}", flush=True)
            if code == 0:
                continue
            jpeg = os.path.join(a.fallback_jpegs, d + ".jpg") if a.fallback_jpegs else None
            if is_status_136(code) and scans[d].endswith(".jp2") and jpeg and os.path.isfile(jpeg):
                # As the worker does (worker.rs, #454): one more run, on LoC's IIIF JPEG of the page.
                code2 = done_before(tess_dir, d, jpeg) if a.resume else None
                if code2 is None:
                    _, code2, err2 = tesseract((d, jpeg, tess_dir))
                else:
                    err2 = "(earlier run)"
                print(f"tesseract {d} IIIF JPEG fallback rc={code2}", flush=True)
                if code2 == 0:
                    scans[d] = jpeg
                    fallback[d] = jpeg
                    continue
                err = err + "\nJPEG fallback: " + err2
            errors[d] = {"returncode": code, "stderr": err}
            left[d] = "tesseract_status_136" if is_status_136(code) else "tesseract_failed"
    rep["tesseract_errors"] = errors
    rep["jpeg_fallback"] = sorted(fallback)
    with open(os.path.join(a.work, "pages.jsonl"), "w") as f:
        for r in sel:
            d = r["doc_id"]
            if d in left:
                continue
            m = DOC.match(d)
            f.write(json.dumps({
                "page": r["path_form"], "page_id": d, "source": source_url(d), "date": m["date"],
                "label": "%s, %s, page %d" % (m["lccn"], m["date"], int(m["seq"])),
                "alto": os.path.join("alto", d + ".xml"), "image": os.path.abspath(scans[d]),
                "tsv": os.path.join("tess", d + ".tsv"),
                "damage_rate": float(r["damage_rate"]) if r.get("damage_rate") else None}) + "\n")
    rep["written"] = len(sel) - len(left)
    json.dump(rep, open(os.path.join(a.work, "prepare-report.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(rep, indent=1))


if __name__ == "__main__":
    main()
