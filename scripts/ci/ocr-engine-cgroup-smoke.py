#!/usr/bin/env python3
"""Exercise worker container limits and timeout cleanup on Linux/Podman."""

import argparse
import json
from pathlib import Path
import signal
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[2]
MEMORY_MIB = 256
PIDS_LIMIT = 64


def run(argv, *, check=True, **kwargs):
    return subprocess.run(argv, check=check, text=True, capture_output=True, timeout=30, **kwargs)


def cgroup_for(pid):
    entries = Path(f"/proc/{pid}/cgroup").read_text().splitlines()
    unified = next((line.split(":", 2)[2] for line in entries if line.startswith("0::")), None)
    if unified is None:
        raise RuntimeError(f"container PID {pid} has no cgroup v2 entry")
    return Path("/sys/fs/cgroup") / unified.lstrip("/")


def inspect_containers(podman, url):
    ids = run(
        [podman, "--remote", "--url", url, "ps", "-aq", "--filter", "label=usnm.engine=contract"]
    ).stdout.split()
    found = []
    for container_id in ids:
        inspected = run(
            [podman, "--remote", "--url", url, "inspect", "--format", "json", container_id],
            check=False,
        )
        if inspected.returncode:
            continue
        raw = inspected.stdout
        doc = json.loads(raw)
        if isinstance(doc, list):
            doc = doc[0]
        pid = int(doc["State"]["Pid"])
        if pid <= 0:
            continue
        try:
            group = cgroup_for(pid)
            memory = (group / "memory.max").read_text().strip()
            pids = (group / "pids.max").read_text().strip()
            processes = (group / "cgroup.procs").read_text().split()
        except FileNotFoundError:
            continue
        found.append((container_id, group, memory, pids, processes))
    return found


def validate_case_output(stdout):
    cases = []
    summary = None
    for line in stdout.splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "case" in item:
            cases.append(item)
        elif "passed" in item:
            summary = item
    by_name = {case["case"]: case for case in cases}
    if summary is None or not summary["passed"] or summary["failed"]:
        raise RuntimeError(f"worker conformance failed:\n{stdout}")
    expected = {
        "hello",
        "text_line",
        "tesseract_dpi",
        "blank_page",
        "corrupt_image",
        "unknown_profile",
        "deadline",
        "deadline_cgroup",
        "protocol",
        "stdout_noise",
        "oversized_output",
        "stdin_eof",
        "contract_n_minus_1",
    }
    if set(by_name) != expected:
        raise RuntimeError(
            f"unexpected conformance cases: {sorted(by_name)}; expected {sorted(expected)}"
        )
    skipped = [
        case["case"]
        for case in cases
        if case["case"] not in {"deadline_cgroup", "contract_n_minus_1"}
        and case["outcome"] != "pass"
    ]
    if skipped:
        raise RuntimeError(f"conformance cases did not pass: {skipped}\n{stdout}")
    if by_name.get("deadline", {}).get("outcome") != "pass":
        raise RuntimeError(f"the deadline/next-page case did not pass:\n{stdout}")
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--base-image", required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    args = parser.parse_args()

    if not sys.platform.startswith("linux"):
        raise RuntimeError("this acceptance test requires Linux cgroup v2")
    if not (ROOT / "ocr/engines/tesseract/contract-test-adapter.sh").is_file():
        raise RuntimeError("the contract test adapter image source is missing")

    work = (ROOT / args.work_dir).resolve()
    if ROOT not in work.parents:
        raise RuntimeError(f"work directory must be inside the checkout: {work}")
    work.parent.mkdir(parents=True, exist_ok=True)
    work.mkdir()
    sock = work / "podman.sock"
    url = f"unix://{sock}"
    scratch = work / "scratch"
    scratch.mkdir(exist_ok=True)
    logfile = (work / "podman-service.log").open("wb")
    service = None
    worker = None
    try:
        service = subprocess.Popen(
            ["podman", "system", "service", "--time=0", url],
            stdout=logfile,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        for _ in range(60):
            if service.poll() is not None:
                raise RuntimeError(f"Podman API service exited; see {logfile.name}")
            ready = run(
                ["podman", "--remote", "--url", url, "info", "--format", "json"],
                check=False,
            )
            if ready.returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError(f"Podman API service did not become ready; see {logfile.name}")

        image_tag = "localhost/usnm-ocr/tesseract-contract-check:ci"
        run(
            [
                "podman",
                "build",
                "--pull=never",
                "--build-arg",
                f"BASE_IMAGE={args.base_image}",
                "-f",
                str(ROOT / "ocr/engines/tesseract/ContractTest.Containerfile"),
                "-t",
                image_tag,
                str(ROOT / "ocr/engines/tesseract"),
            ],
            cwd=ROOT,
        )
        image_id = run(
            ["podman", "image", "inspect", "--format", "{{.Id}}", image_tag]
        ).stdout.strip()
        if not image_id.startswith("sha256:") or len(image_id) != 71:
            raise RuntimeError(f"unexpected test image id: {image_id!r}")

        command = [
            str(args.worker.resolve()),
            "engines",
            "conformance",
            "--engine",
            "contract",
            "--engine-image",
            image_id,
            "--podman-url",
            url,
            "--scratch-dir",
            str(scratch),
            "--engine-memory-mib",
            str(MEMORY_MIB),
            "--engine-pids",
            str(PIDS_LIMIT),
            "--engine-cpus",
            "1",
            "--engine-start-timeout-secs",
            "30",
        ]
        output_path = work / "conformance.log"
        with output_path.open("w") as output_file:
            worker = subprocess.Popen(
                command,
                cwd=ROOT,
                text=True,
                stdout=output_file,
                stderr=subprocess.STDOUT,
            )
            observed = {}
            max_processes = {}
            while worker.poll() is None:
                for container_id, group, memory, pids, processes in inspect_containers(
                    "podman", url
                ):
                    observed[container_id] = (group, memory, pids)
                    max_processes[container_id] = max(
                        max_processes.get(container_id, 0), len(processes)
                    )
                time.sleep(0.2)
            returncode = worker.returncode
        stdout = output_path.read_text()
        if returncode:
            raise RuntimeError(f"worker conformance exited {returncode}:\n{stdout}")
        cases = validate_case_output(stdout)
        if not observed:
            raise RuntimeError("no live engine cgroup was observed")
        for container_id, (_, memory, pids) in observed.items():
            if memory == "max" or int(memory) > MEMORY_MIB * 1024 * 1024:
                raise RuntimeError(f"{container_id} effective memory.max is {memory!r}")
            if pids == "max" or int(pids) > PIDS_LIMIT:
                raise RuntimeError(f"{container_id} effective pids.max is {pids!r}")
        timed_out = [container_id for container_id, count in max_processes.items() if count >= 2]
        if not timed_out:
            raise RuntimeError("no timed-out test container with a child process was observed")
        for container_id in timed_out:
            group = observed[container_id][0]
            remaining = (group / "cgroup.procs").read_text().split() if (group / "cgroup.procs").exists() else []
            if remaining:
                raise RuntimeError(f"{container_id} left processes in {group}: {remaining}")
        for _ in range(100):
            remaining = run(
                ["podman", "--remote", "--url", url, "ps", "-aq", "--filter", "label=usnm.engine=contract"]
            ).stdout.split()
            if not remaining:
                break
            time.sleep(0.2)
        if remaining:
            raise RuntimeError(f"contract containers remain after conformance: {remaining}")

        for case in cases:
            if case["case"] != "deadline_cgroup":
                print(json.dumps(case, sort_keys=True))
        print(
            json.dumps(
                {
                    "case": "deadline_cgroup",
                    "outcome": "pass",
                    "detail": (
                        f"Linux cgroup v2 enforced memory.max <= {MEMORY_MIB} MiB and "
                        f"pids.max <= {PIDS_LIMIT}; timed-out container child processes "
                        "were gone and its cgroup was empty or removed"
                    ),
                },
                sort_keys=True,
            )
        )
        print(json.dumps({"engine": "tesseract-5.5.0 cgroup test adapter", "passed": True, "failed": []}))
    finally:
        if worker is not None and worker.poll() is None:
            worker.terminate()
            try:
                worker.wait(timeout=10)
            except subprocess.TimeoutExpired:
                worker.kill()
                worker.wait()
        if service is not None and service.poll() is None:
            ids = run(
                ["podman", "--remote", "--url", url, "ps", "-aq", "--filter", "label=usnm.engine=contract"],
                check=False,
            )
            for container_id in ids.stdout.split():
                run(
                    ["podman", "--remote", "--url", url, "rm", "--force", "--ignore", "--time", "0", container_id],
                    check=False,
                )
            run(
                ["podman", "rmi", "--force", "localhost/usnm-ocr/tesseract-contract-check:ci"],
                check=False,
            )
            service.send_signal(signal.SIGTERM)
            try:
                service.wait(timeout=10)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait()
        logfile.close()
        import shutil

        shutil.rmtree(work)


if __name__ == "__main__":
    main()
