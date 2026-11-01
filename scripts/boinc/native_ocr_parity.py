#!/usr/bin/env python3
"""Run the same pages through the container worker and the native build, and compare them (#1418).

`run` reads each page twice at the same moment, so both builds see the same load: once with
`usnm_ocr` in a slot directory, and once with `usnm-boinc work` from the worker image's unpacked
root filesystem under chroot, with the image's environment. Neither needs a container runtime on
the test host. `report` compares what they wrote. See docs/boinc-native-ocr.md.

Only public scans passed in are read. Nothing is fetched or submitted.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import time

CHROOT_ENV = {
    "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    "TESSDATA_PREFIX": "/opt/tessdata",
    "OMP_THREAD_LIMIT": "1",
    "LD_PRELOAD": "/usr/local/lib/libusnm-nofpe.so",
}
OUTPUTS = ("text.txt", "layout.xml", "result.json")


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def timed(argv, cwd, stderr):
    """Runs `argv` and gives its wall seconds and the CPU seconds of it and its children."""
    started = time.monotonic()
    with open(stderr, "wb") as err:
        child = subprocess.Popen(argv, cwd=cwd, stdin=subprocess.DEVNULL, stdout=err, stderr=err,
                                 env={"PATH": CHROOT_ENV["PATH"]})
    _, status, usage = os.wait4(child.pid, 0)
    child.returncode = os.waitstatus_to_exitcode(status)
    return child.returncode, time.monotonic() - started, usage.ru_utime + usage.ru_stime


def link_build(slot, build):
    """Puts the build in a slot as the BOINC client does: pointer files, not copies."""
    for item in build.iterdir():
        if item.name != "usnm_ocr":
            (slot / item.name).write_text(f"<soft_link>{item.resolve()}</soft_link>\n")


def read_page(page, a, images):
    name = Path(page["file"]).stem
    scan = a.pages / page["file"]
    out = a.work / name
    native, container = out / "native", a.rootfs / "parity" / name
    for slot in (out, container):
        if slot.exists():
            shutil.rmtree(slot)
        slot.mkdir(parents=True)
    native.mkdir()
    link_build(native, a.build)
    fixture = [str(a.fixture), "--scan", str(scan), "--language", page["language"],
               "--timeout-secs", str(a.timeout_secs)]
    subprocess.run([*fixture, "--image", images["native"], "--dir", str(native)], check=True)
    subprocess.run([*fixture, "--image", images["container"], "--dir", str(container)], check=True)
    chroot = ["chroot", str(a.rootfs), "/usr/bin/env", "-i", *(f"{k}={v}" for k, v in CHROOT_ENV.items()),
              f"USNM_TESSERACT_IMAGE={images['base']}",
              "/usr/local/bin/usnm-boinc", "work", "--dir", f"/parity/{name}"]
    with ThreadPoolExecutor(2) as both:
        n = both.submit(timed, [str(a.build / "usnm_ocr")], native, out / "native.log")
        c = both.submit(timed, chroot, a.work, out / "container.log")
    row = dict(page, native=dict(zip(("status", "wall", "cpu"), n.result())),
               container=dict(zip(("status", "wall", "cpu"), c.result())))
    keep = out / "container"
    keep.mkdir()
    for file in OUTPUTS:
        if (container / file).exists():
            shutil.copyfile(container / file, keep / file)
    shutil.rmtree(container)
    for item in native.iterdir():
        if item.name not in OUTPUTS:
            shutil.rmtree(item) if item.is_dir() else item.unlink()
    print(json.dumps(row), flush=True)
    return row


def run(a):
    # The workers run from their slots and under chroot.
    a.pages, a.build, a.fixture = a.pages.resolve(), a.build.resolve(), a.fixture.resolve()
    a.rootfs, a.work = a.rootfs.resolve(), a.work.resolve()
    pages = json.loads(a.manifest.read_text())
    build = json.loads((a.build / "usnm_native_build.json").read_text())
    images = {
        "native": "sha256:" + sha256(a.build / "usnm_native_build.json"),
        "container": a.container_image,
        "base": a.container_base,
    }
    (a.rootfs / "parity").mkdir(exist_ok=True)
    a.work.mkdir(parents=True, exist_ok=True)
    with ThreadPoolExecutor(a.parallel) as pool:
        rows = list(pool.map(lambda p: read_page(p, a, images), pages))
    (a.work / "rows.json").write_text(json.dumps(rows, indent=1))
    (a.work / "build.json").write_text(json.dumps({
        "engine_version": build["engine_version"], "images": images,
    }, indent=1))


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


def group_summary(rows):
    done = [r for r in rows if r["native"]["status"] == 0 and r["container"]["status"] == 0]
    wall = lambda arm: sum(r[arm]["wall"] for r in done)
    cpu = lambda arm: sum(r[arm]["cpu"] for r in done)
    return {
        "pages": len(rows),
        "both_finished": len(done),
        "text_identical": sum(r["text_identical"] for r in done),
        "alto_identical": sum(r["alto_identical"] for r in done),
        "cer_between_builds_mean": statistics.fmean(r["cer_between_builds"] for r in done) if done else None,
        "cer_between_builds_max": max((r["cer_between_builds"] for r in done), default=None),
        "pages_per_hour_native": len(done) / wall("native") * 3600 if done else None,
        "pages_per_hour_container": len(done) / wall("container") * 3600 if done else None,
        "cpu_seconds_per_page_native": cpu("native") / len(done) if done else None,
        "cpu_seconds_per_page_container": cpu("container") / len(done) if done else None,
    }


def report(a):
    rows = json.loads((a.work / "rows.json").read_text())
    for row in rows:
        out = a.work / Path(row["file"]).stem
        if row["native"]["status"] or row["container"]["status"]:
            continue
        text = [(out / arm / "text.txt").read_text() for arm in ("native", "container")]
        row["text_identical"] = text[0] == text[1]
        row["alto_identical"] = sha256(out / "native/layout.xml") == sha256(out / "container/layout.xml")
        between = 0 if row["text_identical"] else distance(text[0], text[1])
        row["cer_between_builds"] = between / max(len(text[1]), 1)
        reference = a.references / f"{row['page']}.txt" if a.references else None
        if reference and reference.exists():
            ref = reference.read_text()
            row["cer_vs_loc_native"] = distance(text[0], ref) / max(len(ref), 1)
            row["cer_vs_loc_container"] = (row["cer_vs_loc_native"] if row["text_identical"]
                                           else distance(text[1], ref) / max(len(ref), 1))
    summary = {"all": group_summary(rows)}
    for language in sorted({r["language"] for r in rows}):
        for kind in sorted({r["kind"] for r in rows}):
            group = [r for r in rows if r["language"] == language and r["kind"] == kind]
            if group:
                summary[f"{language}/{kind}"] = group_summary(group)
    paired = [r for r in rows if "cer_vs_loc_native" in r]
    if paired:
        summary["vs_loc_ocr"] = {
            "pages": len(paired),
            "native_mean": statistics.fmean(r["cer_vs_loc_native"] for r in paired),
            "container_mean": statistics.fmean(r["cer_vs_loc_container"] for r in paired),
        }
    print(json.dumps(summary, indent=1))
    if a.out:
        a.out.write_text(json.dumps({"summary": summary, "rows": rows}, indent=1))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    r = sub.add_parser("run")
    r.add_argument("--manifest", type=Path, required=True, help="JSON list of pages")
    r.add_argument("--pages", type=Path, required=True, help="directory of the scans")
    r.add_argument("--build", type=Path, required=True, help="directory of the native build")
    r.add_argument("--fixture", type=Path, required=True, help="the native_fixture example")
    r.add_argument("--rootfs", type=Path, required=True, help="the worker image's unpacked root")
    r.add_argument("--container-image", required=True)
    r.add_argument("--container-base", required=True)
    r.add_argument("--work", type=Path, required=True)
    r.add_argument("--parallel", type=int, default=1, help="pages at a time; each uses two CPUs")
    r.add_argument("--timeout-secs", type=int, default=1200, help="the jobs' deadline for one read")
    r.set_defaults(func=run)
    p = sub.add_parser("report")
    p.add_argument("--work", type=Path, required=True)
    p.add_argument("--references", type=Path, help="directory of <page>.txt to score against")
    p.add_argument("--out", type=Path)
    p.set_defaults(func=report)
    a = parser.parse_args()
    a.func(a)


if __name__ == "__main__":
    main()
