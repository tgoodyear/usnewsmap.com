#!/usr/bin/env python3
"""LoC's text of one page, for the native spot check's low-F1 scoring (batch_scheduler.py,
`native_spot_score_low`; docs/boinc/server/batch-scheduler.md).

The controller on CT 211 reads it from what that host already holds, never from the BOINC server
and never from /tank/ocr:

1. `quality.loc_alto`, joined with `quality.pages` on the batch version the corpus takes, names the bulk OCR archive (the quality reader's DSN file).
2. The ALTO cache `/tank/locstats/qwen-alto/<doc id>.xml` (`archive_alto.py`), when its receipt names that archive.
3. Otherwise the archive itself, `/tank/locbulk/ocr/<archive>`, streamed once in order (`nice`
   and `ionice` idle, as the qe-loc extraction runs) and stopped at the page's `ocr.xml`. An
   archive is up to about 4 GB of bzip2, so this takes minutes; the wait is bounded by
   `--timeout-secs`.

The text is the ALTO's words, line by line (`String/@CONTENT`), the way the word-bag F1 reads it;
it is not byte for byte what `usnm_alto::text` joins (hyphen parts and the like), which a bag of
words doesn't see. Run it by hand: `native_spot_loc.py --page lccn/yyyy-mm-dd/ed-1/seq-N`.
"""
import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tarfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path

DSN_FILE = Path("/etc/usnm/quality-reader/dsn")
ARCHIVES = Path("/tank/locbulk/ocr")
ALTO_CACHE = Path("/tank/locstats/qwen-alto")
TIMEOUT_SECS = 600
MAX_ALTO_BYTES = 64 << 20
PAGE = re.compile(r"([A-Za-z0-9]+)/(\d{4})-(\d{2})-(\d{2})/ed-(\d+)/seq-(\d+)")
ARCHIVE = re.compile(r"[A-Za-z0-9_.-]+\.tar\.bz2")


def doc_id_of(page):
    """The engine document id of a page key (`lccn/yyyy-mm-dd/ed-N/seq-N`), or None."""
    return page.replace("/", "_") if PAGE.fullmatch(page) else None


def member_suffixes(page):
    """The ends of the archive member paths that hold `page`'s ALTO, in either layout
    `qe_text::doc_id_of` reads: `<lccn>/<date>/ed-N/seq-N/ocr.xml` or with the date split into
    `<yyyy>/<mm>/<dd>`."""
    lccn, year, month, day, edition, seq = PAGE.fullmatch(page).groups()
    tail = f"ed-{edition}/seq-{seq}/ocr.xml"
    return (f"{lccn}/{year}-{month}-{day}/{tail}", f"{lccn}/{year}/{month}/{day}/{tail}")


def alto_text(xml):
    """The words of an ALTO page, a line to a line, in reading order. A line's `HYP` keeps its
    hyphen at the line's end, as `usnm_alto::text` does, so normalisation joins the word."""
    lines = []
    for line in ET.fromstring(xml).iter():
        if line.tag.rsplit("}", 1)[-1] != "TextLine":
            continue
        words, hyphen = [], ""
        for child in line:
            tag = child.tag.rsplit("}", 1)[-1]
            if tag == "String" and child.get("CONTENT"):
                words.append(child.get("CONTENT"))
            elif tag == "HYP":
                hyphen = child.get("CONTENT") or "-"
        lines.append(" ".join(words) + hyphen)
    return "\n".join(lines)


def archive_of(doc_id, dsn_file=DSN_FILE):
    """The bulk archive `quality.loc_alto` has `doc_id` in, for the batch version the corpus takes
    the page from (the join with `quality.pages`, as qe_loc_extract.sh does), or None."""
    if not re.fullmatch(r"[A-Za-z0-9_-]+", doc_id):
        raise ValueError("invalid document id")
    dsn = Path(dsn_file).read_text().strip()
    done = subprocess.run(
        ["psql", dsn, "-X", "-At", "-c", "SELECT a.archive FROM quality.loc_alto a JOIN quality.pages p USING (doc_id, batch, batch_version) "
         f"WHERE a.doc_id = '{doc_id}' ORDER BY a.batch_version DESC, a.archive LIMIT 1"],
        capture_output=True, text=True, timeout=60, check=False)
    if done.returncode != 0:
        raise RuntimeError("quality.loc_alto lookup failed")
    name = done.stdout.strip().splitlines()[0] if done.stdout.strip() else None
    if name is not None and not ARCHIVE.fullmatch(name):
        raise ValueError("invalid archive name")
    return name


def scan_archive(path, suffixes, deadline):
    """The ALTO bytes of the first member of the bzip2 tar at `path` ending in one of `suffixes`,
    or None when the archive has none or `deadline` (a monotonic time) passes first."""
    idle = ["nice", "-n", "19"] + (["ionice", "-c", "3"] if shutil.which("ionice") else [])
    proc = subprocess.Popen(idle + ["bzip2", "-dc", str(path)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        with tarfile.open(fileobj=proc.stdout, mode="r|") as tar:
            for member in tar:
                if time.monotonic() > deadline:
                    raise TimeoutError(f"{path} not read in time")
                if member.isfile() and member.name.endswith(suffixes):
                    if member.size > MAX_ALTO_BYTES:
                        return None
                    return tar.extractfile(member).read()
        return None
    finally:
        proc.kill()
        proc.wait()
        proc.stdout.close()


def cached_alto(cache, doc_id, archive):
    """The ALTO bytes `archive_alto.py` cached for `doc_id` (flat `<doc id>.xml` with a
    `<doc id>.provenance.json` receipt), when the receipt names `archive` and the file's SHA256;
    otherwise None."""
    xml_path, receipt_path = cache / f"{doc_id}.xml", cache / f"{doc_id}.provenance.json"
    try:
        if xml_path.stat().st_size > MAX_ALTO_BYTES:
            return None
        receipt, xml = json.loads(receipt_path.read_text()), xml_path.read_bytes()
    except (OSError, ValueError):
        return None
    ok = (receipt.get("archive") == archive and receipt.get("doc_id") == doc_id
          and receipt.get("sha256") == hashlib.sha256(xml).hexdigest())
    return xml if ok else None


def page_text(page, *, dsn_file=DSN_FILE, archives=ARCHIVES, cache=ALTO_CACHE, timeout_secs=TIMEOUT_SECS,
              lookup=None, scan=scan_archive):
    """LoC's text of `page`, or None when no archive has it. Raises on a lookup that fails or a scan
    that runs out of time; the caller treats that as no text."""
    doc_id = doc_id_of(page)
    if doc_id is None:
        return None
    name = (lookup or (lambda d: archive_of(d, dsn_file)))(doc_id)
    if name is None:
        return None
    xml = cached_alto(Path(cache), doc_id, name)
    if xml is not None:
        return alto_text(xml)
    xml = scan(Path(archives) / name, member_suffixes(page), time.monotonic() + timeout_secs)
    return None if xml is None else alto_text(xml)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--page", required=True, help="lccn/yyyy-mm-dd/ed-N/seq-N")
    parser.add_argument("--timeout-secs", type=int, default=TIMEOUT_SECS)
    args = parser.parse_args(argv)
    text = page_text(args.page, timeout_secs=args.timeout_secs)
    if text is None:
        print("no LoC text for the page", file=sys.stderr)
        return 1
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
