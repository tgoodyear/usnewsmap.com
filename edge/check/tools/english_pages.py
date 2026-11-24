"""Make tess_pick.py's --pages file for English Chronicling America pages (#2122, route 2): LoC's ALTO from the bulk
archives and a whole-page Tesseract reading of each scan. No network requests.

  python3 english_pages.py --selection pilot.tsv --scans DIR --locbulk /data/locbulk/ocr --locstats /data/locstats \
      --work DIR [--workers 3]

--selection is select_pages.py's TSV (doc_id, path_form, archive, damage_rate, ...). --scans holds each page's scan
as <doc_id>.<format> (copied from the held scans; its sha256 is checked against the selection's scan_sha256).

ALTO. The page's archive is the one the selection names; the locstats index (<archive stem>.parquet, the per-batch
index of what each archive holds) must list the doc_id, or the page is left out as `not_in_locstats`. The archive is
then read as a stream, once for all the pages it holds, and only until the last of them is found: only regular
`ocr.xml` members whose path names a wanted page are read, and nothing is extracted as a filesystem path (as
archive_alto.py); archives are read --workers at a time. Each ALTO is written to WORK/alto/<doc_id>.xml with its member name and sha256 in WORK/alto.jsonl.

Tesseract. Tesseract `eng` at PSM 3 with the worker's arguments, as #2122's arm W (segfirst.py at b045416f):
`tesseract <scan> <out> -l eng --psm 3 --dpi 300 txt tsv` with OMP_THREAD_LIMIT=1, on the scan file. Outputs:
WORK/tess/<doc_id>.{txt,tsv}; a failure is recorded, not retried. WORK/meta.json records the Tesseract version,
the traineddata path and sha256, this script's sha256 and the arguments. --tess-lang MODEL and --tessdata-dir DIR
read with another model at the same settings (#2127's US test: `--tess-lang frak2127_b`, `-l frak2127_b --psm 3
--dpi 300 --tessdata-dir DIR`; DIR needs the model and a configs/ folder holding Tesseract's txt and tsv configs).

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


def tess_args(lang="eng", tessdata_dir=None):
    args = ["-l", lang] + TESS_ARGS[2:]
    return args + (["--tessdata-dir", tessdata_dir] if tessdata_dir else [])


def _run_tess(image, base, args):
    env = dict(os.environ, OMP_THREAD_LIMIT="1")
    return subprocess.run(["tesseract", image, base] + args + ["txt", "tsv"], env=env, stdin=subprocess.DEVNULL,
                          stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)


def tesseract(job):
    """(doc_id, return code, stderr tail, how). A JP2 that crashes Tesseract's own JPEG 2000 path (SIGFPE,
    TESS-CRASH.md on enpick-2122) is decoded by Pillow to a lossless PNG of the same pixels and read again
    (how = "png_fallback"); the PNG is deleted after the read."""
    doc_id, image, out_dir, args = job
    base = os.path.join(out_dir, doc_id)
    p = _run_tess(image, base, args)
    if p.returncode in (-8, 136) and image.endswith(".jp2"):
        from PIL import Image
        Image.MAX_IMAGE_PIXELS = None
        png = base + ".fallback.png"
        with Image.open(image) as im:
            im.save(png)
        try:
            p = _run_tess(png, base, args)
        finally:
            os.remove(png)
        return doc_id, p.returncode, p.stderr[-2000:], "png_fallback"
    return doc_id, p.returncode, p.stderr[-2000:], "direct"


def tess_meta(args=TESS_ARGS, lang="eng", tessdata_dir=None):
    v = subprocess.run(["tesseract", "--version"], capture_output=True, text=True)
    lp = subprocess.run(["tesseract", "--list-langs"], capture_output=True, text=True)
    tessdata = None
    for ln in (lp.stdout + lp.stderr).splitlines():
        if "List of available languages in" in ln:
            tessdata = ln.split('"')[1] if '"' in ln else ln.split(" in ")[-1].rstrip(":")
    tessdata = tessdata_dir or tessdata
    td = os.path.join(tessdata, lang + ".traineddata") if tessdata else None
    return {"tesseract_version": (v.stdout + v.stderr).splitlines()[0].strip(), "args": args,
            "omp_thread_limit": 1, "traineddata": td,
            "traineddata_sha256": sha256_file(td) if td and os.path.exists(td) else None}


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
    ap.add_argument("--tess-lang", default="eng", help="Tesseract model (-l), e.g. frak2127_b")
    ap.add_argument("--tessdata-dir", help="folder holding the model's traineddata (default: Tesseract's own)")
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
        path = os.path.join(a.scans, "%s.%s" % (d, r.get("format") or "jp2"))
        if not os.path.exists(path):
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
    for archive, docs in sorted(by_archive.items()):
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
    targs = tess_args(a.tess_lang, a.tessdata_dir)
    meta = tess_meta(targs, a.tess_lang, a.tessdata_dir)
    meta["script_sha256"] = sha256_file(os.path.abspath(__file__))
    meta["selection_sha256"] = sha256_file(a.selection)
    json.dump(meta, open(os.path.join(a.work, "meta.json"), "w"), indent=1, sort_keys=True)
    jobs = [(d, scans[d], os.path.join(a.work, "tess"), targs) for d in sorted(alto)]
    errors, read_how = {}, {}
    with mp.Pool(a.workers) as pool:
        for n, (d, code, err, how) in enumerate(pool.imap_unordered(tesseract, jobs), 1):
            read_how[d] = how
            if code != 0 or not os.path.exists(os.path.join(a.work, "tess", d + ".tsv")):
                errors[d] = err
                left[d] = "tesseract_failed"
            print(f"tesseract {n}/{len(jobs)} {d} rc={code}", flush=True)
    rep["tesseract_errors"] = errors
    rep["png_fallback"] = sorted(d for d, h in read_how.items() if h == "png_fallback")
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
                "tsv": os.path.join("tess", d + ".tsv"), "tess_input": read_how.get(d),
                "damage_rate": float(r["damage_rate"]) if r.get("damage_rate") else None}) + "\n")
    rep["written"] = len(sel) - len(left)
    json.dump(rep, open(os.path.join(a.work, "prepare-report.json"), "w"), indent=1, sort_keys=True)
    print(json.dumps(rep, indent=1))


if __name__ == "__main__":
    main()
