#!/usr/bin/env python3
"""Dry run of the paired deu_latf/deu Fraktur pilot (#325) on a frozen manifest. Runs nothing.

    plan.py --pages ocr/fraktur-pilot/pages.jsonl --sample ger \\
        --tesseract-image sha256:<image id> --scans-root /tank/ocr/scans --work-root <new dir> \\
        [--variants deu_latf,deu] [--verify-scans] [--commands-out run.sh] [--jobs-out jobs.jsonl]

Prints what the run would be (pages, jobs per variant and decade, scan bytes, the models'
pinned digests) and, with --commands-out, writes the shell commands an OCR operator would
run: per page and variant, copy the cached scan into a new work directory and read it with
the production Tesseract image the way the on-prem worker does (podman with no network, a
read-only root, no capabilities, `-l <variant> --psm 3 --dpi 300 txt alto`). Each job keeps
stdout, stderr, the exit status (`copy-failed` when the scan could not be staged) and its
start and end times beside the outputs, so failures and empty outputs stay in the record, and
the scan's copy is removed after it. timeout stops a job after --timeout seconds and kills it
30 seconds later; the job's container, named for the page, variant and work directory, is
then removed, and on an interrupt too. The script claims its work root with mkdir and stops
if it exists, so one run's outputs and containers are never mixed with another's, and needs
bash 5 (for EPOCHREALTIME) and timeout. Each page's variants are adjacent, so a
partial run stays paired. --jobs-out writes the same jobs as JSONL. --verify-scans re-reads
each scan under --scans-root and checks its size and sha256 against the manifest. Both roots
must be absolute paths.

It enqueues nothing, starts no container and makes no network request. Running the commands
is the operator's step, after the compute is authorized.
"""

import argparse
import hashlib
import json
import re
import shlex
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODELS_LOCK = ROOT / "ocr" / "engines" / "tesseract" / "models.lock"
IMAGE_ID = re.compile(r"^sha256:[0-9a-f]{64}$")
# The on-prem worker's Tesseract defaults (crates/usnm-ocr-worker/src/engine/tesseract.rs).
PSM, DPI, TIMEOUT = 3, 300, 600


def models(lock=MODELS_LOCK):
    """Model name -> pinned sha256, from the Tesseract image's models.lock."""
    pinned = {}
    for line in lock.read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.startswith("#"):
            name, _url, digest = line.split()
            pinned[Path(name).name.removesuffix(".traineddata")] = digest
    return pinned


def read_pages(path, sample):
    pages = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                r = json.loads(line)
                if r["sample"] == sample:
                    pages.append(r)
    return pages


def verify(pages, scans_root):
    """Pages whose cached scan is missing or differs from the manifest, with the reason."""
    bad = []
    for p in pages:
        path = Path(scans_root) / p["scan_path"]
        try:
            size = path.stat().st_size
        except OSError:
            bad.append((p["doc_id"], "missing"))
            continue
        if size != p["scan_bytes"]:
            bad.append((p["doc_id"], f"{size} bytes, not {p['scan_bytes']}"))
            continue
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        if h.hexdigest() != p["scan_sha256"]:
            bad.append((p["doc_id"], "sha256 differs"))
    return bad


def jobs(pages, variants, scans_root, work_root):
    for p in pages:
        ext = Path(p["scan_path"]).suffix
        for v in variants:
            work = Path(work_root) / p["sample"] / p["doc_id"] / v.replace("+", "_")
            yield {
                "decade": p["decade"],
                "doc_id": p["doc_id"],
                "input": f"input{ext}",
                "job_id": f"{p['doc_id']}:{v}",
                "sample": p["sample"],
                "scan": str(Path(scans_root) / p["scan_path"]),
                "scan_sha256": p["scan_sha256"],
                "variant": v,
                "work": str(work),
            }


# Kill an engine that ignores SIGTERM this long after the timeout.
KILL_AFTER = 30

# The generated script's prologue. Each job gets a fresh work directory; staging failures,
# engine failures and timeouts are all recorded in its exit file, and its container is removed
# even when timeout had to kill the podman client or the run is interrupted.
JOB = r"""current=
trap 'if [ -n "$current" ]; then podman rm --force --ignore --time 0 "$current" > /dev/null 2>&1; fi; exit 130' INT TERM
# job <work dir> <scan> <input name> <container name> <podman run arguments...>
job() {
  local work=$1 scan=$2 input=$3 name=$4 t0 status
  shift 4
  mkdir -p -- "${work%/*}" && mkdir -- "$work" || { echo "cannot create $work" >&2; exit 1; }
  t0=$EPOCHREALTIME
  if cp -- "$scan" "$work/$input" 2> "$work/stderr"; then
    current=$name
    timeout --kill-after="$KILL_AFTER" "$TIMEOUT" podman run --name "$name" "$@" > "$work/stdout" 2> "$work/stderr"
    status=$?
    podman rm --force --ignore --time 0 "$name" > /dev/null 2>&1
    current=
  else
    status=copy-failed
  fi
  echo "$status" > "$work/exit"
  echo "$t0 $EPOCHREALTIME" > "$work/time"
  rm -f -- "$work/$input"
}"""


def container_name(job):
    # The work directory's digest makes the name unique to this run: each run claims its own
    # work root, so cleanup by name can never remove another run's container.
    run = hashlib.sha256(job["work"].encode()).hexdigest()[:12]
    page = re.sub(r"[^A-Za-z0-9_.-]", "_", f"{job['sample']}-{job['doc_id']}-{job['variant']}")
    return f"fraktur-pilot-{run}-{page}"


def command(job, image, threads):
    podman = [
        "--rm", "--pull=never", "--network=none", "--read-only",
        "--cap-drop=all", "--security-opt=no-new-privileges", "--log-driver=none",
        "--env", f"OMP_THREAD_LIMIT={threads}", "--entrypoint", "tesseract",
        "--userns=keep-id", "--user", "$user",
        "--volume", f"{job['work']}:/work:Z", "--workdir", "/work", image,
        job["input"], "page", "-l", job["variant"], "--psm", str(PSM), "--dpi", str(DPI), "txt", "alto",
    ]
    head = [job["work"], job["scan"], job["input"], container_name(job)]
    return "job " + " ".join('"$user"' if a == "$user" else shlex.quote(a) for a in head + podman)


def script(job_list, image, threads, timeout, pages_path, pages_sha256, work_root):
    q = shlex.quote
    root = str(work_root)
    lines = [
        "#!/usr/bin/env bash",
        "# Written by scripts/fraktur-pilot/plan.py, a dry run: nothing here has run. The Fraktur",
        "# pilot (#325) runs only after its compute is authorized. Every job keeps its stdout,",
        "# stderr, exit status and start and end times beside page.txt and page.xml, failures included.",
        f"# Pages: {pages_path} (sha256 {pages_sha256}); image {image}.",
        "set -u",
        '[ -n "${EPOCHREALTIME:-}" ] || { echo "run this with bash 5 or later (EPOCHREALTIME)" >&2; exit 1; }',
        f"TIMEOUT={timeout} KILL_AFTER={KILL_AFTER} user=$(id -u):$(id -g)",
        JOB,
        f"mkdir -p -- {q(str(Path(root).parent))} || exit 1",
        f"mkdir -- {q(root)} || {{ echo {q(root + ': a run needs a new work root of its own')} >&2; exit 1; }}",
        f"podman run --rm --pull=never --network=none --entrypoint tesseract {q(image)} --version "
        f"> {q(root + '/engine-version')} 2>&1",
    ]
    lines += [command(j, image, threads) for j in job_list]
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description="Dry run of the paired deu_latf/deu Fraktur pilot (#325).")
    parser.add_argument("--pages", required=True, type=Path, help="the frozen pages.jsonl")
    parser.add_argument("--sample", default="ger", help="which sample to plan (default ger)")
    parser.add_argument("--variants", default="deu_latf,deu", help="comma-separated -l values, paired per page")
    parser.add_argument("--tesseract-image", required=True, help="the production Tesseract image, sha256:<id>")
    parser.add_argument("--scans-root", required=True, type=Path, help="the scan cache root holding keep/")
    parser.add_argument("--work-root", required=True, type=Path, help="where the run would write its outputs")
    parser.add_argument("--threads", type=int, default=1, help="OMP_THREAD_LIMIT per job (default 1)")
    parser.add_argument("--timeout", type=int, default=TIMEOUT, help="seconds per job (default 600, the worker's)")
    parser.add_argument("--verify-scans", action="store_true", help="re-hash each scan under --scans-root")
    parser.add_argument("--commands-out", type=Path, help="write the run's shell commands here")
    parser.add_argument("--jobs-out", type=Path, help="write the jobs as JSONL here")
    args = parser.parse_args(argv)
    for flag, path in (("--scans-root", args.scans_root), ("--work-root", args.work_root)):
        if not path.is_absolute():
            parser.error(f"{flag} must be an absolute path: podman mounts the work directories by it")
    if not IMAGE_ID.match(args.tesseract_image):
        parser.error("--tesseract-image must be an image id, sha256:<64 hex>, as the worker requires")
    variants = [v for v in args.variants.split(",") if v]
    pinned = models()
    unknown = sorted({m for v in variants for m in v.split("+")} - set(pinned))
    if len(variants) < 2 or len(set(variants)) != len(variants) or unknown:
        parser.error(f"--variants needs two or more distinct pinned models (unknown: {', '.join(unknown) or 'none'})")
    pages = read_pages(args.pages, args.sample)
    if not pages:
        parser.error(f"no pages of sample {args.sample} in {args.pages}")
    job_list = list(jobs(pages, variants, args.scans_root, args.work_root))
    by_decade = Counter(p["decade"] for p in pages)
    pages_sha256 = hashlib.sha256(args.pages.read_bytes()).hexdigest()
    print(f"dry run: nothing is run or queued. {args.pages} (sha256 {pages_sha256}), sample {args.sample}")
    print(f"pages {len(pages)}, titles {len({p['lccn'] for p in pages})}, scan bytes {sum(p['scan_bytes'] for p in pages)}")
    print("by decade: " + ", ".join(f"{d}s {n}" for d, n in sorted(by_decade.items())))
    print("formats: " + ", ".join(f"{k} {n}" for k, n in sorted(Counter(p["scan_format"] for p in pages).items())))
    for v in variants:
        digests = ", ".join(f"{m} {pinned[m][:12]}" for m in v.split("+"))
        print(f"variant -l {v}: {sum(j['variant'] == v for j in job_list)} jobs ({digests})")
    print(f"image {args.tesseract_image}, psm {PSM}, dpi {DPI}, threads {args.threads}, timeout {args.timeout}s")
    status = 0
    if args.verify_scans:
        bad = verify(pages, args.scans_root)
        for doc_id, why in bad:
            print(f"error: scan of {doc_id}: {why}", file=sys.stderr)
        print(f"scans verified: {len(pages) - len(bad)} of {len(pages)}")
        status = 1 if bad else 0
    if args.jobs_out:
        args.jobs_out.write_text("".join(json.dumps(j, sort_keys=True) + "\n" for j in job_list), encoding="utf-8")
        print(f"jobs: {args.jobs_out}")
    if args.commands_out:
        body = script(
            job_list, args.tesseract_image, args.threads, args.timeout, args.pages, pages_sha256, args.work_root
        )
        args.commands_out.write_text(body, encoding="utf-8")
        print(f"commands: {args.commands_out} (not run)")
    return status


if __name__ == "__main__":
    sys.exit(main())
