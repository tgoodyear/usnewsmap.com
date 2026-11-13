#!/usr/bin/env python3
"""Daily cross-build check of native OCR (#1418): re-read Windows pages with the Linux build.

Samples pages that Windows native hosts returned in the last 24 hours (seeded random, stratified by
host), reads each scan again with the Linux build's `usnm_tesseract` and the worker's arguments, and
compares the two texts. Per page: CER of the Windows text against the Linux text (edits over the
Linux text's length) and the character-count ratio Windows/Linux. A page is flagged when the CER is
over 1% or the ratio is outside 0.9-1.1, or when its scan is not the bytes the job named.

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
import subprocess
import tempfile

CER_LIMIT = 0.01
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


def returned_pages(handoff, images, now, window=WINDOW_SECONDS):
    """Pages a Windows build returned in the window, each as a dict, oldest first."""
    out = []
    spool = Path(handoff) / "spool"
    for job in sorted(spool.iterdir()) if spool.is_dir() else []:
        try:
            returned = max(f.stat().st_mtime for f in job.iterdir())
        except (OSError, ValueError):
            continue
        if not now - window <= returned <= now:
            continue
        try:
            result = json.loads((job / f"usnm_{job.name}__file_result.json").read_text())
            if result["engine"]["image"] not in images:
                continue
            request = json.loads((Path(handoff) / "dispatch/jobs" / job.name / "request.json").read_text())
            out.append({
                "job": job.name,
                "host": (job / "host_id").read_text().strip(),
                "page": request["message"]["page"],
                "batch": request["message"]["batch"],
                "language": result["engine"]["profile"],
                "engine": result["engine"],
                "scan_sha256": request["scan"]["sha256"],
                "scan_url": request["scan"]["url"],
                "text": job / f"usnm_{job.name}__file_text.txt",
                "returned": returned,
            })
        except (OSError, KeyError, ValueError):
            continue  # not a native result, or not readable yet
    return out


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


def judge(windows, linux, cer_limit=CER_LIMIT):
    """CER of `windows` against `linux`, the length ratio, and whether either limit is passed."""
    cer = distance(windows, linux) / max(len(linux), 1)
    ratio = len(windows) / max(len(linux), 1)
    flagged = cer > cer_limit or not RATIO_RANGE[0] <= ratio <= RATIO_RANGE[1]
    return {"cer": round(cer, 5), "ratio": round(ratio, 4), "windows_chars": len(windows),
            "linux_chars": len(linux), "flagged": flagged}


def check_page(page, a):
    row = {k: page[k] for k in ("job", "host", "page", "language")}
    scan = scan_path(a.scans, page)
    try:
        if hashlib.sha256(scan.read_bytes()).hexdigest() != page["scan_sha256"]:
            return dict(row, error="scan is not the bytes the job named", flagged=True)
        windows = page["text"].read_text(errors="replace")
        with tempfile.TemporaryDirectory(dir=a.work_dir) as work:
            linux = read_linux(a.tesseract, a.tessdata, scan, page["language"], work)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as e:
        return dict(row, error=str(e)[:300], flagged=True)
    return dict(row, **judge(windows, linux, a.cer_limit))


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
            f"{s['flagged']} flagged, max CER {s['max_cer']}, ratio {s['min_ratio']}-{s['max_ratio']}")
    if report["consecutive_flag_days"] >= ESCALATE_DAYS:
        line += f" PAUSE-REVIEW ({report['consecutive_flag_days']} days in a row)"
    return line


def run(a, now=None):
    now = now or dt.datetime.now(dt.timezone.utc).timestamp()
    date = dt.datetime.fromtimestamp(now, dt.timezone.utc).date().isoformat()
    images = windows_images(a.trust_file)
    pages = returned_pages(a.handoff, images, now)
    picked = sample(pages, date, a.count)
    with ThreadPoolExecutor(a.threads) as pool:
        rows = list(pool.map(lambda p: check_page(p, a), picked))
    measured = [r for r in rows if "cer" in r]
    flagged = [r for r in rows if r["flagged"]]
    a.out_dir.mkdir(parents=True, exist_ok=True)
    state_file = a.out_dir / "state.json"
    state = json.loads(state_file.read_text()) if state_file.exists() else {}
    consecutive, days = consecutive_days(state, date, bool(flagged))
    report = {
        "date": date,
        "linux_tesseract_sha256": hashlib.sha256(Path(a.tesseract).read_bytes()).hexdigest(),
        "windows_images": images,
        "returned_in_window": len(pages),
        "summary": {
            "pages": len(rows), "hosts": len({r["host"] for r in rows}), "flagged": len(flagged),
            "max_cer": max((r["cer"] for r in measured), default=None),
            "min_ratio": min((r["ratio"] for r in measured), default=None),
            "max_ratio": max((r["ratio"] for r in measured), default=None),
        },
        "consecutive_flag_days": consecutive,
        "rows": rows,
    }
    (a.out_dir / f"crossbuild-{date}.json").write_text(json.dumps(report, indent=1) + "\n")
    state_file.write_text(json.dumps({"flagged_days": dict(sorted(days.items())[-30:])}, indent=1) + "\n")
    line = summary_line(report)
    print(line)
    if flagged and a.issue:
        body = line + "\n\n" + "\n".join(
            f"- {r['page']} (host {r['host']}, job {r['job']}): "
            + (r["error"] if "error" in r else f"CER {r['cer']}, ratio {r['ratio']}")
            for r in flagged)
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
    p.add_argument("--trust-file", type=Path, default=REPO / "boinc/project/native-build-trust.json")
    p.add_argument("--work-dir", type=Path, default=None, help="scratch directory (default: the system's)")
    p.add_argument("--cer-limit", type=float, default=CER_LIMIT,
                   help="flag a page above this CER (default 0.01; see the README on what real pages give)")
    p.add_argument("--count", type=int, default=10)
    p.add_argument("--threads", type=int, default=2, choices=(1, 2), help="pages read at once, one thread each")
    p.add_argument("--issue", type=int, help="tracking issue to append to on a flagged day")
    p.add_argument("--repo", default="tgoodyear/usnewsmap.com")
    return run(p.parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main())
