"""List the loaded check-site tasks that were cut from a merged line (#1978), measured from LoC's ALTO.

A task's line clip was cut from one ALTO TextLine box. When that box covers two printed lines the clip and the word crop
show both (merged_lines.py has the rule: box height more than FACTOR times the median TextLine height of the page, no
flagging on a page with under MIN_PAGE_LINES lines). The build inputs of the loaded sets were not kept, but LoC's ALTO for every
page is in the bulk OCR archives, so this reads the task bodies, finds each task's TextLine in the page's ALTO and applies the rule.

Task bodies: --dump FILE (a /admin/dump response, as recompute_marks.py reads) or --vault NAME (the same dump from --site with the
admin token from Key Vault; read-only, the token stays in memory). The dump also has the answers, which the report counts.

Which TextLine: the task id is `<lang>/<set>/<line id>~<word>`.
  chr, chr-lines, chr-rare: the line id ends in the ALTO TextLine ID (`..._seq-3_P3_TL00018`).
  yi, sr: the line id ends in `_l<N>`, the line's index among the page's TextLines in TextBlock order (yiddish_lines.py's alto_lines).
For sr, reading a is LoC's ALTO and so is the box used here; reading b's own ALTO (the held BOINC layout) is not read.

Where the ALTO is: --archives DIR holds the `<batch>_verNN.tar.bz2` files (members `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`).
Which archive holds a page: --pages FILE (JSON lines with `page`, `batch`, `batch_version`, as docs/reports/2026-11-cherokee/
phoenix-pages.jsonl) and/or --archive-map FILE (TSV `doc_id<TAB>archive`, as `quality.loc_alto` has them; a page may list several
archives and the first one that holds it wins). Only the archives that hold a wanted page are opened, each is read once as a
stream and the read stops when its wanted pages are found, --workers archives at a time. Nothing is fetched from loc.gov.

A task whose page is not found in the archives (no archive listed, the archive missing or lacking the page, the line not in the ALTO)
falls back to its line clip's pixel height: --clips DIR holds the clips at their keys (`<DIR>/<body.line>`), and --clips-url
fetches the missing ones from the check site's public /clips/<key>, one at a time, --delay seconds apart. A clip is cut with padding
proportional to the box height (phoenix_lines.py, yiddish_lines.py, serbian_lines.py: 15% each side, a 4-pixel minimum on Cherokee
lines) and build_tasks.py saves it unscaled, so clip heights keep the ratio of box heights, except where a line sits at the
page edge and the padding was cut short. The median for a clip is over the clips of the page's distinct lines in the same set (scan scales differ between
sets), not the page's lines, so it needs MIN_PAGE_LINES distinct lines of that page in the set. Tasks measured this way are counted apart in the report. A task with
neither measure is listed as unmeasured.

Writes, in --out-dir: affected-task-ids.txt (one id per line, `#` header lines; the file confusable.sh --tasks reads),
flagged-lines.jsonl (one record per flagged line: its page, height, page median, ratio and how it was measured), unmeasured-tasks.txt,
report.json and report.txt (per set: tasks, how many measured each way, flagged lines, tasks, answers, settled tasks by outcome,
and how many were already marked).

  python3 merged_lines_loaded.py --dump dump.jsonl --archives /path/to/bulk-ocr --pages phoenix-pages.jsonl \\
    --archive-map doc-archives.tsv --workers 4 --out-dir OUT [--lang chr --lang yi ...] [--factor 1.6] [--min-page-lines 5]
Standard library only.
"""
import argparse
import collections
import concurrent.futures
import io
import json
import os
import re
import struct
import sys
import tarfile
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import merged_lines as ml  # noqa: E402
import recompute_marks as rm  # noqa: E402  (the admin-route client; stdlib only)

LANGS = ("chr", "chr-lines", "chr-rare", "sr", "yi")
INDEX_LINE = re.compile(r"^l(\d+)$")


class Refused(Exception):
    pass


# ---------------------------------------------------------------- tasks and dumps

def split_task_id(task_id: str) -> tuple:
    """`chr/phoenix-nt-v1/sn83020866_1828-05-28_ed-1_seq-3_P3_TL00018~0` -> ("chr", "phoenix-nt-v1", "<line id>", "0")."""
    lang, set_name, rest = task_id.split("/", 2)
    line_id, _, word = rest.rpartition("~")
    return lang, set_name, line_id, word


def line_ref(page: str, line_id: str):
    """("index", N) or ("id", ALTO TextLine ID) for a task's line, or None when the id does not carry the page's name."""
    stem = page.replace("/", "_") + "_"
    if not line_id.startswith(stem):
        return None
    suffix = line_id[len(stem):]
    m = INDEX_LINE.match(suffix)
    if m:
        return ("index", int(m.group(1)))
    return ("id", suffix) if suffix else None


def read_dump(text: str, langs) -> tuple:
    """({task id: row}, {task id: active answers}, {task id: set-aside answers}) for the tasks of `langs`."""
    tasks, active, aside = {}, collections.Counter(), collections.Counter()
    for raw in text.splitlines():
        if not raw.strip():
            continue
        line = json.loads(raw)
        row = line.get("row", {})
        if line.get("table") == "tasks" and row.get("lang") in langs:
            tasks[row["id"]] = {**row, "body": json.loads(row["body"])}
        elif line.get("table") == "answers":
            active[row["task_id"]] += 1
        elif line.get("table") == "answers_excluded":
            aside[row["task_id"]] += 1
    return tasks, active, aside


# ---------------------------------------------------------------- ALTO in the archives

def _tag(e) -> str:
    return e.tag.rsplit("}", 1)[-1]


def alto_heights(data: bytes) -> dict:
    """{"all": [(TextLine ID, height)] in document order, "indexed": [height] in TextBlock order} of one page's ALTO."""
    root = ET.parse(io.BytesIO(data)).getroot()
    height = lambda tl: int(float(tl.get("HEIGHT") or 0))
    every = [(tl.get("ID"), height(tl)) for tl in root.iter() if _tag(tl) == "TextLine"]
    indexed = [height(tl) for block in root.iter() if _tag(block) == "TextBlock" for tl in block.iter() if _tag(tl) == "TextLine"]
    return {"all": every, "indexed": indexed}


def archive_member(page: str) -> str:
    """`lccn/date/ed-N/seq-N` -> `lccn/yyyy/mm/dd/ed-N/seq-N/ocr.xml`, the member name in LoC's archives."""
    lccn, date, ed, seq = page.split("/")
    y, m, d = date.split("-")
    return f"{lccn}/{y}/{m}/{d}/{ed}/{seq}/ocr.xml"


def scan_archive(job: tuple) -> dict:
    """Stream one archive and return {member: alto_heights} for the wanted members, stopping when all are found."""
    path, wanted = job
    found, left = {}, set(wanted)
    with tarfile.open(path, "r|bz2") as tf:
        for m in tf:
            if not left:
                break
            name = m.name[2:] if m.name.startswith("./") else m.name
            key = name if name in left else "/".join(name.split("/")[-7:])
            if key in left and m.isfile():
                try:
                    found[key] = alto_heights(tf.extractfile(m).read())
                except (ET.ParseError, ValueError):  # an unreadable page falls back to its clips; the others go on
                    pass
                left.discard(key)
    return found


def read_pages_file(path: str) -> dict:
    """{page: [archive file names]} from phoenix-pages.jsonl-style rows."""
    out = {}
    for raw in open(path, encoding="utf-8"):
        if raw.strip():
            r = json.loads(raw)
            out.setdefault(r["page"], []).append(f"{r['batch']}_ver{int(r['batch_version']):02d}.tar.bz2")
    return out


def read_archive_map(path: str) -> dict:
    """{doc_id: [archive file names]} from a TSV of doc_id and archive (a header row and extra columns are ignored)."""
    out = {}
    for raw in open(path, encoding="utf-8"):
        cols = raw.rstrip("\n").split("\t")
        if len(cols) < 2 or cols[0] == "doc_id":
            continue
        name = os.path.basename(cols[1].strip())
        out.setdefault(cols[0].strip(), []).append(name if name.endswith(".tar.bz2") else name + ".tar.bz2")
    return out


def plan_archives(pages, archive_dir: str, page_archives: dict, doc_archives: dict) -> tuple:
    """({archive file: set of members}, {page: reason it has no archive}, {member: [archive files that exist], in preference order})."""
    jobs, prefs, why = collections.defaultdict(set), {}, {}
    for page in sorted(pages):
        names = page_archives.get(page, []) + doc_archives.get(page.replace("/", "_"), [])
        names = [n for n in dict.fromkeys(names)]
        if not names:
            why[page] = "no archive listed for the page"
            continue
        have = [n for n in names if os.path.isfile(os.path.join(archive_dir, n))]
        if not have:
            why[page] = "archive not in --archives: " + ", ".join(names)
            continue
        prefs[archive_member(page)] = have
        for n in have:
            jobs[n].add(archive_member(page))
    return dict(jobs), why, prefs


def read_alto(jobs: dict, prefs: dict, archive_dir: str, workers: int, progress=None) -> dict:
    """{page member: alto_heights}. A page listed in several archives takes the first of its preferred archives that holds it."""
    found_in = {}
    items = [(os.path.join(archive_dir, name), tuple(sorted(members))) for name, members in sorted(jobs.items())]
    with concurrent.futures.ProcessPoolExecutor(max_workers=max(1, workers)) as pool:
        for (path, _), found in zip(items, pool.map(scan_archive, items)):
            found_in[os.path.basename(path)] = found
            if progress:
                progress(path, len(found))
    out = {}
    for member, names in prefs.items():
        for name in names:
            if member in found_in.get(name, {}):
                out[member] = found_in[name][member]
                break
    return out


# ---------------------------------------------------------------- clips

def image_size(data: bytes):
    """(width, height) of a PNG or JPEG from its header bytes, else None."""
    if data[:8] == b"\x89PNG\r\n\x1a\n" and len(data) >= 24:
        return struct.unpack(">II", data[16:24])
    if data[:2] == b"\xff\xd8":
        i = 2
        while i + 9 < len(data):
            if data[i] != 0xFF:
                i += 1
                continue
            marker = data[i + 1]
            if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7:
                i += 2
                continue
            (length,) = struct.unpack(">H", data[i + 2:i + 4])
            if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
                h, w = struct.unpack(">HH", data[i + 5:i + 9])
                return w, h
            i += 2 + length
    return None


class Clips:
    """Line clips by key: from a local directory, else (with --clips-url) fetched once from the site's public /clips/<key>."""

    def __init__(self, directory, url, delay):
        self.directory, self.url, self.delay = directory, url.rstrip("/") if url else None, delay
        self.fetched = 0

    def height(self, key: str):
        if not key or ".." in key or key.startswith("/"):
            return None
        data = None
        if self.directory:
            path = os.path.join(self.directory, key)
            if os.path.isfile(path):
                with open(path, "rb") as f:
                    data = f.read()
        if data is None and self.url:
            req = urllib.request.Request(self.url + "/clips/" + urllib.parse.quote(key), headers={"user-agent": "usnm-check-merged-lines/1"})
            try:
                with urllib.request.urlopen(req, timeout=60) as r:
                    data = r.read()
            except (urllib.error.URLError, OSError):
                data = None
            self.fetched += 1
            time.sleep(self.delay)
            if data and self.directory:
                path = os.path.join(self.directory, key)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "wb") as f:
                    f.write(data)
        size = image_size(data) if data else None
        return size[1] if size else None


# ---------------------------------------------------------------- measuring

def measure(tasks: dict, alto: dict, clips: Clips, factor: float, min_lines: int) -> dict:
    """{task id: result}. A result is {"how": "alto" | "clip" | "unmeasured", "flagged": bool, and for a measured one
    "page", "line", "height", "median", "ratio"; for an unmeasured one "why"}."""
    results = {}
    by_page = collections.defaultdict(list)
    for tid, t in tasks.items():
        lang, set_name, line_id, _ = split_task_id(tid)
        page = t["body"].get("page") or ""
        ref = line_ref(page, line_id) if page else None
        res = {"page": page, "line": line_id, "set": f"{lang}/{set_name}", "lang": lang}
        results[tid] = res
        if ref is None:
            res.update(how="unmeasured", flagged=False, why="the task id does not name its line")
            continue
        page_alto = alto.get(archive_member(page)) if page.count("/") == 3 else None
        height = median = None
        if page_alto is not None:
            if ref[0] == "index":
                heights = page_alto["indexed"]
                height = heights[ref[1]] if ref[1] < len(heights) else None
                median = ml.page_median(heights, min_lines)
            else:
                heights = [h for _, h in page_alto["all"]]
                height = next((h for i, h in page_alto["all"] if i == ref[1]), None)
                median = ml.page_median(heights, min_lines)
            if height is not None:
                res.update(how="alto", height=height, median=median, line_ref=ref[1])
                continue
        by_page[page].append((tid, ref))
    # The fallback: line clips. A scan's scale differs between sets, so the median is over one page of one set, and over every
    # distinct source line of it that has a clip, whether or not the ALTO measured it. Only the tasks that need the fallback take
    # their result from it.
    groups = collections.defaultdict(list)
    for page, items in by_page.items():
        for tid, ref in items:
            groups[(page, results[tid]["set"])].append((tid, ref))
    everyone = collections.defaultdict(list)
    for tid, res in results.items():
        if (res["page"], res["set"]) in groups:
            everyone[(res["page"], res["set"])].append(tid)
    for (page, set_name), items in groups.items():
        got = {}
        for tid in everyone[(page, set_name)]:
            key = tasks[tid]["body"].get("line")
            if key not in got:
                got[key] = clips.height(key)
        lines = {}  # by the source line, not the clip key: one line is several tasks
        for tid in everyone[(page, set_name)]:
            ref = line_ref(page, split_task_id(tid)[2])
            h = got.get(tasks[tid]["body"].get("line"))
            if h and ref is not None:
                lines.setdefault(ref, h)
        median = ml.page_median(list(lines.values()), min_lines)
        for tid, ref in items:
            h = got.get(tasks[tid]["body"].get("line"))
            if h:
                results[tid].update(how="clip", height=h, median=median)
            else:
                results[tid].update(how="unmeasured", flagged=False, why="page not in the archives and no line clip" if page else "no page in the body")
    for tid, res in results.items():
        if res["how"] != "unmeasured":
            res["flagged"] = ml.is_merged(res["height"], res["median"], factor)
            if res["median"]:
                res["ratio"] = round(res["height"] / res["median"], 3)
    return results


def build_report(tasks, results, active, aside, factor, min_lines, why_pages, how_found):
    sets = collections.defaultdict(lambda: collections.Counter())
    flagged_lines = {}
    for tid, res in sorted(results.items()):
        s = sets[res["set"]]
        s["tasks"] += 1
        s["measured_" + res["how"]] += 1
        t = tasks[tid]
        if res["how"] != "unmeasured" and res.get("median") is None:
            s["page_too_few_lines"] += 1
        if not res["flagged"]:
            continue
        s["tasks_flagged"] += 1
        flagged_lines[(res["set"], res["page"], res["line"])] = res
        s["answers_active"] += active[tid]
        s["answers_set_aside"] += aside[tid]
        s["answers"] += active[tid] + aside[tid]
        if t.get("settled"):
            s["settled"] += 1
            s["settled_" + str(t.get("outcome") or "none")] += 1
        if t.get("held") or t.get("confusable_display") or t.get("mark_reason"):
            s["already_marked"] += 1
    for res in flagged_lines.values():
        sets[res["set"]]["lines_flagged"] += 1
    return {"factor": factor, "min_page_lines": min_lines, "sets": {k: dict(sorted(v.items())) for k, v in sorted(sets.items())},
            "pages_without_archive": why_pages, "pages_read_from_archives": how_found}


def report_text(rep: dict) -> str:
    out = [f"merged lines among loaded tasks: box or clip taller than {rep['factor']} times the page median (pages with under {rep['min_page_lines']} lines not checked)"]
    for name, s in rep["sets"].items():
        out.append(f"{name}: {s.get('tasks', 0)} tasks; measured from ALTO {s.get('measured_alto', 0)}, from clips {s.get('measured_clip', 0)}, "
                   f"unmeasured {s.get('measured_unmeasured', 0)}; flagged {s.get('lines_flagged', 0)} lines, {s.get('tasks_flagged', 0)} tasks, "
                   f"{s.get('answers', 0)} answers ({s.get('answers_set_aside', 0)} already set aside), {s.get('settled', 0)} settled, "
                   f"{s.get('already_marked', 0)} already marked")
    out.append(f"{len(rep['pages_without_archive'])} pages had no archive to read")
    return "\n".join(out) + "\n"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--dump", help="a /admin/dump response (JSON lines)")
    src.add_argument("--vault", help="Key Vault holding the admin token: read the dump from --site (read-only)")
    ap.add_argument("--site", default=rm.DEFAULT_SITE)
    ap.add_argument("--lang", action="append", help=f"a language to measure; repeatable (default: {' '.join(LANGS)})")
    ap.add_argument("--archives", required=True, help="folder of the bulk OCR archives, <batch>_verNN.tar.bz2")
    ap.add_argument("--pages", help="phoenix-pages.jsonl: page, batch and batch_version for the Cherokee pages")
    ap.add_argument("--archive-map", help="TSV of doc_id and archive file, from quality.loc_alto")
    ap.add_argument("--workers", type=int, default=4, help="archives read at once")
    ap.add_argument("--clips", help="folder of line clips at their keys, for pages not found in the archives")
    ap.add_argument("--clips-url", help="fetch missing clips from this site's public /clips/<key> (e.g. https://check.usnewsmap.com)")
    ap.add_argument("--delay", type=float, default=1.0, help="seconds between clip fetches")
    ap.add_argument("--factor", type=float, default=ml.FACTOR)
    ap.add_argument("--min-page-lines", type=int, default=ml.MIN_PAGE_LINES)
    ap.add_argument("--out-dir", required=True)
    a = ap.parse_args(argv)

    langs = tuple(a.lang or LANGS)
    if a.vault:
        text = rm.Site(a.site, rm.vault_token(a.vault))._request("/admin/dump").decode("utf-8")
    else:
        text = open(a.dump, encoding="utf-8").read()
    tasks, active, aside = read_dump(text, langs)
    if not tasks:
        raise Refused("no tasks of " + ", ".join(langs) + " in the dump")
    pages = {t["body"]["page"] for t in tasks.values() if str(t["body"].get("page", "")).count("/") == 3}
    jobs, why_pages, prefs = plan_archives(pages, a.archives, read_pages_file(a.pages) if a.pages else {}, read_archive_map(a.archive_map) if a.archive_map else {})
    print(f"{len(tasks)} tasks on {len(pages)} pages; {len(jobs)} archives to read, {len(why_pages)} pages without one", file=sys.stderr, flush=True)
    alto = read_alto(jobs, prefs, a.archives, a.workers, lambda path, n: print(f"{os.path.basename(path)}: {n} pages", file=sys.stderr, flush=True))
    clips = Clips(a.clips, a.clips_url, a.delay)
    results = measure(tasks, alto, clips, a.factor, a.min_page_lines)
    rep = build_report(tasks, results, active, aside, a.factor, a.min_page_lines, why_pages, len(alto))

    os.makedirs(a.out_dir, exist_ok=True)
    flagged = sorted(t for t, r in results.items() if r["flagged"])
    with open(os.path.join(a.out_dir, "affected-task-ids.txt"), "w", encoding="utf-8") as f:
        f.write(f"# Tasks cut from a merged line (#1978): box or clip taller than {a.factor} times the page median.\n")
        f.write("".join(t + "\n" for t in flagged))
    with open(os.path.join(a.out_dir, "unmeasured-tasks.txt"), "w", encoding="utf-8") as f:
        f.write("".join(f"{t}\t{r['why']}\n" for t, r in sorted(results.items()) if r["how"] == "unmeasured"))
    seen = set()
    with open(os.path.join(a.out_dir, "flagged-lines.jsonl"), "w", encoding="utf-8") as f:
        for t in flagged:
            r = results[t]
            if (r["set"], r["page"], r["line"]) in seen:
                continue
            seen.add((r["set"], r["page"], r["line"]))
            f.write(json.dumps({"set": r["set"], "page": r["page"], "line": r["line"], "measured": r["how"], "height": r["height"],
                                "median": r["median"], "ratio": r["ratio"], "factor": a.factor,
                                "tasks": [x for x in flagged if (results[x]["set"], results[x]["page"], results[x]["line"]) == (r["set"], r["page"], r["line"])]},
                               ensure_ascii=False, sort_keys=True) + "\n")
    json.dump(rep, open(os.path.join(a.out_dir, "report.json"), "w", encoding="utf-8"), indent=1, sort_keys=True)
    open(os.path.join(a.out_dir, "report.txt"), "w", encoding="utf-8").write(report_text(rep))
    sys.stdout.write(report_text(rep))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (Refused, rm.Refused) as e:
        sys.exit(f"merged_lines_loaded.py: {e}")
