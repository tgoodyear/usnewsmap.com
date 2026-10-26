#!/usr/bin/env python3
"""Run every scripts/**/test_*.py unit test, so a new test file runs in CI with no list to edit.

`python3 -m unittest discover -s scripts` reads only scripts/ itself: there is no __init__.py
under scripts/, and discovery does not enter directories that are not packages (every test now
sits in a subdirectory, so it finds none). So this globs the files and runs each in its own
`python3 -m unittest` process from the repository root, as CI always ran them, which keeps one
file's imports and patches out of the next. Exits non-zero if any file fails or collects no
tests. `--list` prints what would run and what is left out. A file left out here must run in a CI
step of its own.
"""
import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# Path or directory (relative to the repository root) -> why the generic run leaves it out.
EXCLUDE = {
    "scripts/archive": "archived scripts are not maintained and not run by CI (scripts/archive/README.md)",
    "scripts/ocr-bench": "has its own workflow, .github/workflows/ocr-bench.yml",
    "scripts/ocr-survey": "needs Pillow; ci.yml's OCR gold set step runs it (test_transcription imports by directory)",
    "scripts/ocr-node/test_quality_jpeg_cache.py": "needs Pillow; ci.yml's OCR nodes step installs it",
    "scripts/ocr-node/test_quality_jpeg_list.py": "needs Pillow; ci.yml's OCR nodes step installs it",
    "scripts/ops/test_check_index_history.py": "needs fastjsonschema; ci.yml's ops job runs it in its hash-pinned venv",
    "scripts/ops/test_check_version_snapshots.py": "needs fastjsonschema; ci.yml's ops job runs it in its hash-pinned venv",
    "scripts/boinc/test_patch_server_gold_routing.py": "compiles C++ against nlohmann-json3-dev; ci.yml's contrib job installs it",
}
# Seconds in production run 40389690376; other/new modules have equal weight.
WEIGHTS = {
    "scripts/ci/test_changes.py": 185,
    "scripts/boinc/test_boinc_scripts.py": 109,
    "scripts/azure/test_env_scripts.py": 90,
    "scripts/ci/test_head_of_main.py": 19,
}


def selected():
    run, left_out = [], []
    for path in sorted(ROOT.joinpath("scripts").rglob("test_*.py")):
        rel = path.relative_to(ROOT).as_posix()
        why = next((r for p, r in EXCLUDE.items() if rel == p or rel.startswith(p + "/")), None)
        (left_out if why else run).append((rel, why))
    return run, left_out


def shard_files(files, index, count):
    shards = [[] for _ in range(count)]
    loads = [0] * count
    for item in sorted(files, key=lambda item: (-WEIGHTS.get(item[0], 1), item[0])):
        shard = min(range(count), key=lambda i: (loads[i], i))
        shards[shard].append(item)
        loads[shard] += WEIGHTS.get(item[0], 1)
    return sorted(shards[index])


def main(argv=None):
    parser = argparse.ArgumentParser(description="Run every scripts/**/test_*.py unit test.")
    parser.add_argument("--list", action="store_true", help="print the files to run and those left out, then stop")
    parser.add_argument("--shard-index", type=int, default=0, help="zero-based shard index")
    parser.add_argument("--shard-count", type=int, default=1, help="number of isolated CI shards")
    parser.add_argument("--timings", type=Path, help="write per-module durations and outcomes as JSON")
    args = parser.parse_args(argv)
    if args.shard_count < 1 or not 0 <= args.shard_index < args.shard_count:
        parser.error("require shard-count >= 1 and 0 <= shard-index < shard-count")
    run, left_out = selected()
    run = shard_files(run, args.shard_index, args.shard_count)
    if args.list:
        print("\n".join(f"run   {rel}" for rel, _ in run))
        print("\n".join(f"skip  {rel}  ({why})" for rel, why in left_out))
        return 0
    if not run:
        print("error: no scripts/**/test_*.py found", file=sys.stderr)
        return 1
    total, failed, timings = 0, [], []
    for rel, _ in run:
        print(f"=== {rel}", flush=True)
        started = time.monotonic()
        proc = subprocess.run([sys.executable, "-m", "unittest", "-v", rel], cwd=ROOT, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(proc.stdout, end="", flush=True)
        summaries = re.findall(r"^Ran (\d+) tests? in", proc.stdout, re.M)
        count = int(summaries[-1]) if summaries else 0
        timings.append({"file": rel, "seconds": round(time.monotonic() - started, 3),
                        "tests": count, "exitcode": proc.returncode})
        total += count
        if proc.returncode:
            failed.append((rel, "failed"))
        elif count == 0:
            # unittest exits 0 for a module with no tests; a file this runs must contribute some.
            failed.append((rel, "collected no tests"))
    print(f"\n{total} tests in {len(run)} files; {len(left_out)} files left out (see --list)")
    for rel, why in failed:
        print(f"FAILED: {rel} ({why})")
    print("\nSlowest modules:")
    for item in sorted(timings, key=lambda item: item["seconds"], reverse=True)[:10]:
        print(f'{item["seconds"]:8.3f}s {item["file"]}')
    if args.timings:
        args.timings.write_text(json.dumps(timings, indent=2) + "\n")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
