#!/usr/bin/env python3
"""Stage LoC's text for the cross-build check's day sample (#1929).

`native_crossbuild_check.py --loc-texts DIR` (env XB_LOC_TEXTS) compares each sampled page with LoC's text,
laid out as `<sn>/<date>/ed-<n>/seq-<m>.txt`. This fills DIR for the pages the coming run will sample. It
runs as the unit's `ExecStartPre=-` after the handoff fetch, picks the day's sample with the check's own
functions (same images, hosts, seen jobs, UTC date, count), and takes each missing page's text from the
first source that has it:

1. local: a sidecar next to the scan in the keep store (`<seq>.txt` or `<seq>/ocr.txt`);
2. archive: LoC's bulk OCR archive for the page's batch (`<archives>/<batch>.tar.bz2`, member
   `<sn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.txt`). One streaming pass per archive pulls every page the run
   needs from it, then stops reading it. No index: the sampled page's batch names the archive;
3. loc.gov, last resort: the item JSON (`/item/<sn>/<date>/ed-<n>/?fo=json`) names the page's
   `fulltext_service`, whose answer holds `full_text`. Two requests a page, at least 5 s apart
   (www.loc.gov's Crawl-Delay), timeouts on both, the project's own User-Agent, and the first 403, 429 or
   503 ends the tier for the run. The raw answers are kept under `<out>/_loc-gov/`.

Every staged file is kept; nothing is deleted. Files are written to a temporary name and renamed, so the
check never reads half a file. Fails soft: any error is logged and the exit status is 0, and a page with no
text is left out (the check then judges it on the two builds alone). An empty sample (no new returns) does
nothing. Prints one line: pages by source (have, local, archive, loc.gov, missing). Read IO should run
niced and idle (the unit sets Nice=19 and IOSchedulingClass=idle).
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
import tarfile
import time
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent
USER_AGENT = "usnewsmap-ocr-worker/stage-loc-texts (+https://usnewsmap.com)"
LOC_GAP = 5.0  # www.loc.gov Crawl-Delay
PAGE_RE = re.compile(r"^([a-z0-9]{1,16})/(\d{4})-(\d{2})-(\d{2})/ed-(\d+)/seq-(\d+)$")
MAX_BYTES = 16 * 1024 * 1024
SOURCES = ("have", "local", "archive", "loc.gov", "missing")


def load_check(path):
    spec = importlib.util.spec_from_file_location("native_crossbuild_check", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def log(msg):
    print(f"stage_loc_texts: {msg}", file=sys.stderr, flush=True)


def keep(path, data):
    """Write atomically; never overwrite a file already kept."""
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        return
    tmp = path.with_name(f".{path.name}.part")
    tmp.write_bytes(data)
    os.replace(tmp, path)


def member_name(page):
    sn, y, m, d, ed, seq = PAGE_RE.match(page).groups()
    return f"{sn}/{y}/{m}/{d}/ed-{ed}/seq-{seq}/ocr.txt"


def local_text(scans, page):
    """A text sidecar beside the scan in the keep store, if there is one."""
    sn, date, ed, seq = page["page"].split("/")
    rel = Path(page["batch"]) / sn / date / ed.removeprefix("ed-")
    s = seq.removeprefix("seq-")
    for tree in ("keep", "buffer"):
        for cand in (Path(scans) / tree / rel / f"{s}.txt", Path(scans) / tree / rel / s / "ocr.txt"):
            if cand.is_file():
                return cand.read_bytes()
    return None


def archive_for(archives, batch):
    """`<batch>.tar.bz2`, else the highest `<batch>_verNN.tar.bz2`."""
    exact = Path(archives) / f"{batch}.tar.bz2"
    if exact.is_file():
        return exact
    found = sorted(Path(archives).glob(f"{batch}_ver[0-9]*.tar.bz2"))
    return found[-1] if found else None


def from_archive(archive, wanted, deadline):
    """Stream `archive` once; return {member name: bytes} for the wanted names, stopping when all are found."""
    got = {}
    with tarfile.open(archive, mode="r|bz2") as tar:
        for info in tar:
            if time.monotonic() > deadline:
                log(f"{archive.name}: time budget spent, {len(wanted) - len(got)} page(s) not read")
                break
            if info.name in wanted and info.isfile() and info.size <= MAX_BYTES:
                got[info.name] = tar.extractfile(info).read()
                if len(got) == len(wanted):
                    break
    return got


class Pushback(Exception):
    pass


def pacer_blocked(path, now_ms=None):
    """Why the host's shared LoC budget (the fetcher's `USNM_LOC_GLOBAL_PACER` state file) says not to send
    LoC requests now, or None. A hold (`hold_until_ms`) or a slow-down after a push-back blocks this tier,
    and so does a file that can't be read. This step only reads the file: it never takes a slot or records
    a hold (the file belongs to the fetcher's user), and at most ~20 requests a day are not worth a slot.
    No path means no shared budget on this host."""
    if not path:
        return None
    now_ms = int(time.time() * 1000) if now_ms is None else now_ms
    try:
        state = json.loads(Path(path).read_text() or "{}")
    except (OSError, ValueError) as e:
        return f"the shared LoC pacer file can't be read ({type(e).__name__})"
    if int(state.get("hold_until_ms") or 0) > now_ms:
        return "the shared LoC pacer holds all requests"
    slowed = state.get("slowed")
    if slowed:
        return f"the shared LoC pacer is slowed after a push-back ({slowed.get('why', '?')})"
    return None


def fetch(url, timeout, sleep):
    """One GET with the project's User-Agent. 403, 429 and 503 end the tier."""
    sleep(LOC_GAP)
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.read(MAX_BYTES + 1)[:MAX_BYTES]
    except urllib.error.HTTPError as e:
        if e.code in (403, 429, 503):
            raise Pushback(f"HTTP {e.code} from {url.split('?')[0]}")
        raise


def from_loc_gov(page, out_dir, sleep=time.sleep):
    """(text bytes) for one page through the item JSON's fulltext_service. Raises Pushback."""
    sn, date, ed, seq = page["page"].split("/")
    n = int(seq.removeprefix("seq-"))
    item = fetch(f"https://www.loc.gov/item/{sn}/{date}/{ed}/?fo=json", 30, sleep)
    files = json.loads(item)["resources"][0]["files"]
    url = next((f["fulltext_service"] for f in files[n - 1] if "fulltext_service" in f), None)
    if url is None or not url.startswith("https://tile.loc.gov/"):
        return None
    answer = fetch(url, 60, sleep)
    text = next(iter(json.loads(answer).values()))["full_text"]
    keep(Path(out_dir) / "_loc-gov" / f"{page['page']}.json", answer)
    return text.encode("utf-8")


def stage(a, check=None, sleep=time.sleep, now=None):
    """Stage the sample, then pick it again at the current time and stage what is new: reading archives can
    take a while, and the check's 24 h window moves with the clock. At most 3 rounds; the check starts
    seconds after the last one."""
    check = check or load_check(a.check_script)
    counts, done = dict.fromkeys(SOURCES, 0), set()
    for _ in range(3):
        if not stage_round(a, check, sleep, now or time.time(), counts, done):
            break
        now = None
    return counts


def stage_round(a, check, sleep, now, counts, done):
    """One selection and staging pass; the number of pages newly picked."""
    date = time.strftime("%Y-%m-%d", time.gmtime(now))
    for needed in ("spool", "dispatch/jobs"):
        if not (a.handoff / needed).is_dir():
            log(f"{a.handoff / needed} is not readable: nothing staged")
            return 0
    state_file = a.state_dir / "state.json"
    state = json.loads(state_file.read_text()) if state_file.exists() else {}
    images = check.selected_images(a.trust_file, a.image)
    a.hosts = a.hosts or []
    pages = check.returned_pages(a.handoff, images, now, seen=set(state.get("checked_jobs", [])),
                                 hosts=set(a.hosts or ()))
    picked = [p for p in check.sample(pages, date, a.count)
              if "invalid" not in p and PAGE_RE.match(p.get("page", "")) and p["page"] not in done]
    if not picked:
        if not done:
            log(f"{date}: empty sample, nothing to stage")
        return 0
    done.update(p["page"] for p in picked)
    todo = []
    for p in picked:
        dest = a.out / f"{p['page']}.txt"
        if dest.is_file():
            counts["have"] += 1
            continue
        text = local_text(a.scans, p)
        if text is not None:
            keep(dest, text)
            counts["local"] += 1
        else:
            todo.append(p)
    by_archive, rest = {}, []
    for p in todo:
        arc = archive_for(a.archives, p["batch"])
        (by_archive.setdefault(arc, []) if arc else rest).append(p)
    deadline = time.monotonic() + a.archive_budget_secs
    for arc, group in by_archive.items():
        try:
            got = from_archive(arc, {member_name(p["page"]) for p in group}, deadline)
        except (OSError, EOFError, tarfile.TarError) as e:
            log(f"{arc.name}: {e}")
            got = {}
        for p in group:
            data = got.get(member_name(p["page"]))
            if data is None:
                rest.append(p)
            else:
                keep(a.out / f"{p['page']}.txt", data)
                counts["archive"] += 1
    pushed = a.no_loc_gov or bool(rest and pacer_blocked(getattr(a, "loc_pacer", None)))
    if rest and not a.no_loc_gov and pushed:
        log(f"loc.gov skipped: {pacer_blocked(getattr(a, 'loc_pacer', None))}")
    attempts = 0
    for p in rest:
        if pushed or attempts >= a.loc_max:
            counts["missing"] += 1
            continue
        attempts += 1
        try:
            text = from_loc_gov(p, a.out, sleep)
        except Pushback as e:
            log(f"loc.gov ended for this run: {e}")
            pushed = True
            text = None
        except (OSError, ValueError, KeyError, IndexError, StopIteration, urllib.error.URLError) as e:
            log(f"loc.gov {p['page']}: {type(e).__name__}: {str(e)[:100]}")
            text = None
        if text is None:
            counts["missing"] += 1
        else:
            keep(a.out / f"{p['page']}.txt", text)
            counts["loc.gov"] += 1
    return len(picked)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--handoff", type=Path, required=True, help="handoff directory (spool/, dispatch/jobs/)")
    p.add_argument("--scans", type=Path, required=True, help="scan tree holding keep/")
    p.add_argument("--state-dir", type=Path, required=True, help="the check's --out-dir (state.json: jobs already checked)")
    p.add_argument("--out", type=Path, required=True, help="LoC text directory: the check's XB_LOC_TEXTS")
    p.add_argument("--archives", type=Path, required=True, help="directory of LoC bulk OCR <batch>.tar.bz2")
    p.add_argument("--trust-file", type=Path, required=True)
    p.add_argument("--check-script", type=Path, default=HERE / "native_crossbuild_check.py")
    env = os.environ.get
    p.add_argument("--image", default=env("XB_IMAGE") or None,
                   help="digest prefix, as for the check (default: Windows builds; env XB_IMAGE)")
    p.add_argument("--host", dest="hosts", action="append", default=None,
                   help="only this host id; repeatable (env XB_HOSTS, comma separated, used only when no --host is given)")
    p.add_argument("--loc-pacer", type=Path, default=None,
                   help="the fetcher's shared LoC pacer file (USNM_LOC_GLOBAL_PACER): read-only; loc.gov is "
                        "skipped while it holds or is slowed")
    p.add_argument("--count", type=int, default=10, help="the check's --count")
    p.add_argument("--archive-budget-secs", type=float, default=1800, help="stop reading archives after this long")
    p.add_argument("--loc-max", type=int, default=10, help="most pages fetched from loc.gov in a run")
    p.add_argument("--no-loc-gov", action="store_true", help="never ask loc.gov")
    a = p.parse_args(argv)
    if a.hosts is None:
        a.hosts = [h for h in (env("XB_HOSTS") or "").split(",") if h]
    try:
        counts = stage(a)
    except Exception as e:  # fail soft: the check runs as before
        log(f"failed, nothing more staged: {type(e).__name__}: {str(e)[:200]}")
        counts = None
    if counts is not None:
        print("stage_loc_texts " + time.strftime("%Y-%m-%d", time.gmtime()) + " "
              + " ".join(f"{k}={counts[k]}" for k in SOURCES))
    return 0


if __name__ == "__main__":
    sys.exit(main())
