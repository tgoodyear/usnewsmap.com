#!/usr/bin/env python3
"""Daily cross-build check of native OCR (#1418): re-read Windows pages with the Linux build.

Samples pages that Windows native hosts returned in the last 24 hours (seeded random, stratified by
host), reads each scan again with the Linux build's `usnm_tesseract` and the worker's arguments, and
compares the two texts. Per page: CER of the Windows text against the Linux text (edits over the
Linux text's length), the character-count ratio Windows/Linux and the word-multiset F1. A page is
flagged when the CER is over 10%, the F1 under 0.9 or the ratio outside 0.9-1.1, or when its scan is not the bytes the job named.

Inputs are read only: the handoff directory (`spool/<job>/` with `host_id`,
`usnm_<job>__file_result.json` and `usnm_<job>__file_text.txt`, and `dispatch/jobs/<job>/request.json`)
and the scan tree (`keep/<batch>/<sn>/<date>/<ed>/<seq>.jp2`). Nothing is fetched or submitted.

Writes `crossbuild-<date>.json` and `state.json` to --out-dir and prints one summary line. Exit 0 when
no page is flagged, 3 when any is. `state.json` counts consecutive flagged days; at 2 the summary says
`PAUSE-REVIEW`, which is the cue to ask the Compute Coordinator to pause the plan. Nothing here pauses
anything. With --issue, a flagged day is also appended to that GitHub issue through `gh`.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import random
import re
import collections
import statistics
import subprocess
import tempfile

CER_LIMIT = 0.10
F1_LIMIT = 0.90
RATIO_RANGE = (0.9, 1.1)
WINDOW_SECONDS = 24 * 3600
ESCALATE_DAYS = 2
REPO = Path(__file__).resolve().parents[2]


def distance(a, b):
    """Levenshtein distance by Myers's bit-vector algorithm, on Python's big integers."""
    if not b:
        return len(a)
    peq = {}
    for i, c in enumerate(b):
        peq[c] = peq.get(c, 0) | (1 << i)
    mask, top = (1 << len(b)) - 1, 1 << (len(b) - 1)
    pv, mv, score = mask, 0, len(b)
    for c in a:
        eq = peq.get(c, 0)
        xv = eq | mv
        xh = (((eq & pv) + pv) ^ pv) | eq
        ph = mv | ~(xh | pv) & mask
        mh = pv & xh
        score += bool(ph & top) - bool(mh & top)
        ph = ((ph << 1) | 1) & mask
        mh = (mh << 1) & mask
        pv = mh | ~(xv | ph) & mask
        mv = ph & xv
    return score


def windows_images(trust_file):
    """Build digests that `native-build-trust.json` records for the Windows platform."""
    builds = json.loads(Path(trust_file).read_text())["builds"]
    return sorted(d for d, b in builds.items() if b["platform"] == "windows_x86_64")


class InvalidInput(Exception):
    """A returned result that can't be compared: reported apart from build differences."""


def returned_pages(handoff, images, now, window=WINDOW_SECONDS, seen=()):
    """Windows pages returned in the window, oldest first, each as a dict.

    The window is the result manifest's `completed_at`, not a file time (a refreshed copy of the hand-off
    rewrites files), and a job already checked on an earlier day is skipped. A result that doesn't match
    its dispatch request (job id, scan sha256, engine) or its own text checksum, or whose text isn't UTF-8,
    comes back with an `invalid` reason instead of a text.
    """
    out = []
    spool, jobs = Path(handoff) / "spool", Path(handoff) / "dispatch/jobs"
    for job in sorted(spool.iterdir()):
        if job.name in seen:
            continue
        try:
            result = json.loads((job / f"usnm_{job.name}__file_result.json").read_text())
            if result["engine"]["image"] not in images:
                continue
            completed = dt.datetime.fromisoformat(result["completed_at"]).timestamp()
        except (OSError, KeyError, ValueError):
            continue  # not a native result, or not complete yet
        if not now - window <= completed <= now:
            continue
        page = {"job": job.name, "completed": completed, "engine": result["engine"]}
        try:
            request = json.loads((jobs / job.name / "request.json").read_text())
            page.update(host=(job / "host_id").read_text().strip(), page=request["message"]["page"],
                        batch=request["message"]["batch"], language=result["engine"]["profile"],
                        scan_sha256=request["scan"]["sha256"], scan_url=request["scan"]["url"])
            raw = (job / f"usnm_{job.name}__file_text.txt").read_bytes()
            if not (result["job_id"] == job.name == request["id"]
                    and result["scan_sha256"] == request["scan"]["sha256"]
                    and result["engine"] == request["engine"]):
                page["invalid"] = "result does not match its dispatch request"
            elif hashlib.sha256(raw).hexdigest() != result["text_sha256"]:
                page["invalid"] = "text does not match its checksum"
            else:
                page["windows_text"] = raw.decode("utf-8")
        except UnicodeDecodeError:
            page["invalid"] = "text is not UTF-8"
        except (OSError, KeyError, ValueError) as e:
            page.update(host=page.get("host", "?"), page=page.get("page", job.name), batch="", language="",
                        scan_sha256="", scan_url="", invalid=f"unreadable: {str(e)[:100]}")
        out.append(page)
    return sorted(out, key=lambda p: p["completed"])


def sample(pages, date, count):
    """`count` pages, spread evenly over hosts, drawn with a generator seeded by date and host."""
    by_host = {}
    for page in pages:
        by_host.setdefault(page["host"], []).append(page)
    hosts = sorted(by_host)
    shuffled = {h: random.Random(f"{date}:{h}").sample(sorted(by_host[h], key=lambda p: p["job"]),
                                                       len(by_host[h])) for h in hosts}
    picked = []
    while len(picked) < count and any(shuffled.values()):
        for host in hosts:
            if shuffled[host] and len(picked) < count:
                picked.append(shuffled[host].pop())
    return picked


def scan_path(scans, page):
    """`keep/<batch>/<sn>/<date>/<ed>/<seq>.<ext>` for `<sn>/<date>/ed-<n>/seq-<m>`."""
    sn, date, ed, seq = page["page"].split("/")
    ext = Path(page["scan_url"].split("?")[0]).suffix or ".jp2"
    return Path(scans) / "keep" / page["batch"] / sn / date / ed.removeprefix("ed-") / f"{seq.removeprefix('seq-')}{ext}"


def read_linux(tesseract, tessdata, scan, language, work, psm=3, dpi=300):
    """The worker's invocation (`usnm_ocr` native, `assess::work::ocr`): one thread, txt and alto."""
    env = {"PATH": "/usr/bin:/bin", "TESSDATA_PREFIX": str(tessdata), "OMP_THREAD_LIMIT": "1"}
    base = Path(work) / "page"
    done = subprocess.run([str(tesseract), str(scan), str(base), "-l", language, "--psm", str(psm),
                           "--dpi", str(dpi), "txt", "alto"], env=env, stdin=subprocess.DEVNULL,
                          stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=3600)
    if done.returncode:
        raise RuntimeError(f"tesseract exited {done.returncode}: {done.stderr[-200:].decode(errors='replace')}")
    return base.with_suffix(".txt").read_text(errors="replace")


def word_f1(a, b):
    """F1 of the lower-cased word multisets: insensitive to reading order."""
    x, y = (collections.Counter(re.findall(r"\w+", t.lower())) for t in (a, b))
    hit = sum((x & y).values())
    if not hit:
        return 1.0 if not x and not y else 0.0
    p, r = hit / sum(x.values()), hit / sum(y.values())
    return 2 * p * r / (p + r)


def judge(windows, linux, cer_limit=CER_LIMIT, f1_limit=F1_LIMIT):
    """CER of `windows` against `linux`, the length ratio, word F1, and whether a limit is passed."""
    cer = distance(windows, linux) / max(len(linux), 1)
    ratio = len(windows) / max(len(linux), 1)
    f1 = word_f1(windows, linux)
    flagged = cer > cer_limit or f1 < f1_limit or not RATIO_RANGE[0] <= ratio <= RATIO_RANGE[1]
    return {"cer": round(cer, 5), "ratio": round(ratio, 4), "word_f1": round(f1, 4),
            "windows_chars": len(windows), "linux_chars": len(linux), "flagged": flagged}


def distribution(values):
    """Min, quartiles, mean and max, so limits can be tightened from the daily files."""
    if not values:
        return None
    v = sorted(values)
    q = statistics.quantiles(v, n=4, method="inclusive") if len(v) > 1 else [v[0]] * 3
    return {"min": v[0], "q1": round(q[0], 5), "median": round(q[1], 5), "q3": round(q[2], 5),
            "mean": round(statistics.fmean(v), 5), "max": v[-1]}


def check_page(page, a):
    row = {k: page[k] for k in ("job", "host", "page", "language")}
    if "invalid" in page:
        return dict(row, invalid=page["invalid"], flagged=False)
    scan = scan_path(a.scans, page)
    try:
        if hashlib.sha256(scan.read_bytes()).hexdigest() != page["scan_sha256"]:
            return dict(row, invalid="scan is not the bytes the job named", flagged=False)
        with tempfile.TemporaryDirectory(dir=a.work_dir) as work:
            linux = read_linux(a.tesseract, a.tessdata, scan, page["language"], work)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as e:
        return dict(row, error=str(e)[:300], flagged=True)
    return dict(row, **judge(page["windows_text"], linux, a.cer_limit, a.f1_limit))


def lock_hashes(lock_file):
    """`tessdata/<model>.traineddata` -> sha256, from models.lock."""
    out = {}
    for line in Path(lock_file).read_text().splitlines():
        fields = line.split()
        if len(fields) == 3 and not line.startswith("#"):
            out[Path(fields[0]).name] = fields[2]
    return out


def verify_engine(a, languages):
    """Refuse to run with a Linux engine that isn't a recorded build's, or models that aren't locked."""
    trust = json.loads(Path(a.trust_file).read_text())["builds"]
    bases = {b["engine_base"] for b in trust.values() if b["platform"] == "x86_64-pc-linux-gnu"}
    got = "sha256:" + hashlib.sha256(Path(a.tesseract).read_bytes()).hexdigest()
    if got not in bases:
        raise SystemExit(f"{a.tesseract} ({got}) is not a recorded Linux build's engine_base")
    locked = lock_hashes(a.models_lock)
    for language in sorted(languages):
        name = f"{language}.traineddata"
        have = hashlib.sha256((Path(a.tessdata) / name).read_bytes()).hexdigest()
        if locked.get(name) != have:
            raise SystemExit(f"{a.tessdata}/{name} is not the locked model")


def consecutive_days(state, date, flagged):
    """Days in a row, ending at `date`, on which a page was flagged; a repeat run of a date is idempotent."""
    days = dict(state.get("flagged_days", {}))
    days[date] = flagged
    run, day = 0, dt.date.fromisoformat(date)
    while days.get(day.isoformat()):
        run += 1
        day -= dt.timedelta(days=1)
    return run, days


def summary_line(report):
    s = report["summary"]
    line = (f"crossbuild {report['date']}: {s['pages']} pages from {s['hosts']} hosts, "
            f"{s['flagged']} flagged, {s['invalid']} invalid, max CER {s['max_cer']}, ratio {s['min_ratio']}-{s['max_ratio']}, "
            f"min word F1 {s['min_word_f1']}")
    if report["consecutive_flag_days"] >= ESCALATE_DAYS:
        line += f" PAUSE-REVIEW ({report['consecutive_flag_days']} days in a row)"
    return line


def run(a, now=None):
    now = now or dt.datetime.now(dt.timezone.utc).timestamp()
    date = dt.datetime.fromtimestamp(now, dt.timezone.utc).date().isoformat()
    images = windows_images(a.trust_file)
    for needed in ("spool", "dispatch/jobs"):
        if not (a.handoff / needed).is_dir():
            raise SystemExit(f"{a.handoff / needed} is not readable: nothing recorded")
    a.out_dir.mkdir(parents=True, exist_ok=True)
    state_file = a.out_dir / "state.json"
    state = json.loads(state_file.read_text()) if state_file.exists() else {}
    pages = returned_pages(a.handoff, images, now, seen=set(state.get("checked_jobs", [])))
    picked = sample(pages, date, a.count)
    verify_engine(a, {p["language"] for p in picked if "invalid" not in p})
    with ThreadPoolExecutor(a.threads) as pool:
        rows = list(pool.map(lambda p: check_page(p, a), picked))
    measured = [r for r in rows if "cer" in r]
    flagged = [r for r in rows if r["flagged"]]
    consecutive, days = consecutive_days(state, date, bool(flagged))
    report = {
        "date": date,
        "linux_tesseract_sha256": hashlib.sha256(Path(a.tesseract).read_bytes()).hexdigest(),
        "windows_images": images,
        "returned_in_window": len(pages),
        "summary": {
            "pages": len(rows), "invalid": sum("invalid" in r for r in rows), "hosts": len({r["host"] for r in rows}), "flagged": len(flagged),
            "max_cer": max((r["cer"] for r in measured), default=None),
            "min_ratio": min((r["ratio"] for r in measured), default=None),
            "max_ratio": max((r["ratio"] for r in measured), default=None),
            "min_word_f1": min((r["word_f1"] for r in measured), default=None),
            "cer_distribution": distribution([r["cer"] for r in measured]),
            "word_f1_distribution": distribution([r["word_f1"] for r in measured]),
            "ratio_distribution": distribution([r["ratio"] for r in measured]),
        },
        "consecutive_flag_days": consecutive,
        "rows": rows,
    }
    (a.out_dir / f"crossbuild-{date}.json").write_text(json.dumps(report, indent=1) + "\n")
    state_file.write_text(json.dumps({"flagged_days": dict(sorted(days.items())[-30:]),
                                       "checked_jobs": (state.get("checked_jobs", []) + [r["job"] for r in rows])[-20000:]}, indent=1) + "\n")
    line = summary_line(report)
    print(line)
    reportable = [r for r in rows if r["flagged"] or "invalid" in r]
    if reportable and a.issue:
        body = line + "\n\n" + "\n".join(
            f"- {r['page']} (host {r['host']}, job {r['job']}): "
            + (r["error"] if "error" in r else r["invalid"] if "invalid" in r else f"CER {r['cer']}, ratio {r['ratio']}, word F1 {r['word_f1']}")
            for r in reportable)
        try:
            subprocess.run(["gh", "issue", "comment", str(a.issue), "--repo", a.repo, "--body", body],
                           check=True, timeout=120)
        except (OSError, subprocess.SubprocessError) as e:
            print(f"could not comment on issue {a.issue}: {e}")
    return 3 if flagged else 0


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--handoff", type=Path, required=True, help="handoff directory (spool/, dispatch/jobs/)")
    p.add_argument("--scans", type=Path, required=True, help="scan tree holding keep/")
    p.add_argument("--tesseract", type=Path, required=True, help="the Linux build's usnm_tesseract")
    p.add_argument("--tessdata", type=Path, required=True, help="directory with the locked models and configs/")
    p.add_argument("--out-dir", type=Path, required=True)
    p.add_argument("--models-lock", type=Path, default=REPO / "ocr/engines/tesseract/models.lock")
    p.add_argument("--trust-file", type=Path, default=REPO / "boinc/project/native-build-trust.json")
    p.add_argument("--work-dir", type=Path, default=None, help="scratch directory (default: the system's)")
    p.add_argument("--cer-limit", type=float, default=CER_LIMIT,
                   help="flag a page above this CER (default 0.10)")
    p.add_argument("--f1-limit", type=float, default=F1_LIMIT, help="flag a page below this word F1 (default 0.90)")
    p.add_argument("--count", type=int, default=10)
    p.add_argument("--threads", type=int, default=2, choices=(1, 2), help="pages read at once, one thread each")
    p.add_argument("--issue", type=int, help="tracking issue to append to on a flagged day")
    p.add_argument("--repo", default="tgoodyear/usnewsmap.com")
    return run(p.parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main())
