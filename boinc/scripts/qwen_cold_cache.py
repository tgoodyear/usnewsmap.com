#!/usr/bin/env python3
"""CI-only cold/warm extraction of a locally built image in a NEW Docker store.

Never pulls, prunes, resets, or modifies an existing daemon. Registry compressed
sizes are checked separately by qwen_disk.py at release.
"""
from __future__ import annotations

import argparse
import contextlib
import gzip
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time

try:
    from qwen_disk import policy
except ModuleNotFoundError:
    from boinc.scripts.qwen_disk import policy


def daemon_command(state, host):
    # A second bridge driver can remove docker0 even with --bridge=none.
    # Keep its entire network stack separate from the build daemon.
    return [
        "sudo", "-n", "unshare", "--net", "dockerd",
        f"--data-root={state}/data", f"--exec-root={state}/exec",
        f"--pidfile={state}/daemon.pid", f"--host={host}",
        "--bridge=none", "--iptables=false", "--ip-masq=false", "--storage-driver=overlay2",
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--model", choices=("2b", "8b"), required=True)
    parser.add_argument("--state", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise SystemExit("Only run on a disposable GitHub Actions runner, never on a volunteer node")
    state = args.state.resolve()
    if not state.is_relative_to(Path.cwd().resolve()) or state.exists():
        raise SystemExit("--state must be a NEW directory beneath this checkout")
    subprocess.run(["docker", "image", "inspect", args.image], check=True, stdout=subprocess.DEVNULL)
    state.mkdir()
    env = dict(os.environ, DOCKER_HOST=f"unix://{state}/docker.sock")
    log = (state / "daemon.log").open("w")
    daemon = subprocess.Popen(daemon_command(state, env["DOCKER_HOST"]),
                              stdout=log, stderr=subprocess.STDOUT)
    p = policy()
    selected = p["models"][args.model]
    report = {"image": args.image, "model": args.model, "policy_version": p["version"],
              "registry_compressed_bytes": None,
              "peak_sample_interval_ms": 100,
              "note": "Local gzip docker-save stream, not registry transfer. Filesystem deltas include any concurrent runner activity; peak is sampled."}
    stop = threading.Event()
    sampler = None
    try:
        for _ in range(120):
            if subprocess.run(["sudo", "-n", "docker", "--host", env["DOCKER_HOST"], "info"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
                break
            if daemon.poll() is not None:
                raise RuntimeError("Isolated dockerd failed; inspect daemon.log")
            time.sleep(0.5)
        else:
            raise RuntimeError("Isolated dockerd startup timed out")

        def docker(*command):
            return ["sudo", "-n", "docker", "--host", env["DOCKER_HOST"], *command]

        assert subprocess.check_output(docker("image", "ls", "-q")).strip() == b""
        info = json.loads(subprocess.check_output(docker("info", "--format={{json .}}")))
        report["storage_driver"] = info["Driver"]
        report["docker_server_version"] = info["ServerVersion"]
        baseline = shutil.disk_usage(state).used
        peak = [baseline]

        def sample():
            while not stop.wait(0.1):
                peak[0] = max(peak[0], shutil.disk_usage(state).used)

        sampler = threading.Thread(target=sample)
        sampler.start()

        def load():
            # Stream from the existing build daemon: no second archive on disk.
            started = time.monotonic()
            source = subprocess.Popen(["docker", "save", args.image], stdout=subprocess.PIPE)
            with (state / "load.log").open("ab") as output:
                target = subprocess.Popen(docker("load"), stdin=subprocess.PIPE, stdout=output, stderr=output)
                count = 0

                class Counter:
                    def write(self, data):
                        nonlocal count
                        target.stdin.write(data)
                        count += len(data)

                    def flush(self):
                        target.stdin.flush()

                try:
                    with gzip.GzipFile(fileobj=Counter(), mode="wb", compresslevel=1, mtime=0) as compressed:
                        shutil.copyfileobj(source.stdout, compressed, length=1024 * 1024)
                    target.stdin.close()
                    if source.wait() != 0 or target.wait() != 0:
                        raise RuntimeError("docker save/load failed; inspect load.log")
                finally:
                    source.stdout.close()
                    with contextlib.suppress(BrokenPipeError):
                        target.stdin.close()
                    for process in (source, target):
                        if process.poll() is None:
                            process.terminate()
                            process.wait(timeout=30)
            return count, time.monotonic() - started

        report["local_gzip_stream_bytes"], report["cold_load_wall_secs"] = load()
        cold_used = shutil.disk_usage(state).used
        report["cold_filesystem_delta_bytes"] = max(0, cold_used - baseline)
        report["cold_graph_du_bytes"] = int(subprocess.check_output(
            ["sudo", "-n", "du", "-sk", str(state / "data")]).split()[0]) * 1024
        inspected = json.loads(subprocess.check_output(docker("image", "inspect", args.image)))[0]
        report["image_id"] = inspected["Id"]
        report["platform"] = f"{inspected['Os']}/{inspected['Architecture']}"
        report["source_revision"] = inspected["Config"]["Labels"]["org.opencontainers.image.revision"]
        report["logical_unpacked_bytes"] = inspected["Size"]
        payload = sum(selected["files"].values())
        unpacked_bound = p["base_unpacked_bound_bytes"] + p["image_overhead_bound_bytes"] + payload
        assert inspected["Size"] <= unpacked_bound, "Unpacked image exceeds reviewed envelope"
        assert report["local_gzip_stream_bytes"] <= p["base_compressed_bytes"] + payload + p["image_overhead_bound_bytes"]
        files = subprocess.check_output(docker(
            "run", "--rm", "--network=none", "--read-only", "--entrypoint=/bin/sh", args.image,
            "-c", "find /opt/qwen/models -name '*.gguf' -printf '%f %s\\n'")).decode().splitlines()
        sizes = {name: int(size) for name, size in (line.rsplit(" ", 1) for line in files)}
        assert sizes == selected["files"], "Payload contains wrong/extra models or unexpected byte sizes"
        report["payload_file_bytes"] = sizes
        _, report["warm_load_wall_secs"] = load()
        report["warm_filesystem_delta_bytes"] = max(0, shutil.disk_usage(state).used - cold_used)
        report["peak_filesystem_delta_bytes"] = max(0, peak[0] - baseline)
        assert report["peak_filesystem_delta_bytes"] + p["warm_incremental_bytes"] + p["reserve_bytes"] <= selected["container_cold_bytes"]
        assert report["warm_filesystem_delta_bytes"] <= p["warm_incremental_bytes"]
        stop.set()
        sampler.join()

        def task(slot, *command):
            cpus = 4 if args.model == "2b" else 2
            return docker(
                "run", "--rm", "--network=none", "--read-only", "--cap-drop=ALL",
                "--cap-add=DAC_OVERRIDE", "--security-opt=no-new-privileges",
                "--memory=6g", "--memory-swap=6g", "--pids-limit=1024", f"--cpus={cpus}",
                "--tmpfs=/tmp:rw,size=1g", "--volume", f"{slot}:/work", args.image, *command)

        invalid = state / "invalid-slot"
        invalid.mkdir(mode=0o777)
        invalid.chmod(0o777)
        (invalid / "qwen.json").write_text("{}")
        (invalid / "page.jpg").write_text("invalid page")
        subprocess.run(task(invalid), check=True, timeout=120)
        for name in ("readings.json", "result.json", "diag.txt"):
            assert (invalid / name).is_file(), f"Missing cold-store error output: {name}"
        assert json.loads((invalid / "result.json").read_text())["unit_error"]
        report["error_output_shapes_passed"] = True
        limits = subprocess.check_output(task(
            invalid, "/bin/sh", "-c",
            "cat /sys/fs/cgroup/memory.max /sys/fs/cgroup/memory.swap.max /sys/fs/cgroup/pids.max /sys/fs/cgroup/cpu.max",
        )).decode().splitlines()
        assert limits[:3] == ["6442450944", "0", "1024"], limits
        quota, period = map(int, limits[3].split())
        assert quota == period * (4 if args.model == "2b" else 2), limits
        report["kernel_limits"] = limits
        if args.model == "2b":
            slot = state / "cpu-slot"
            slot.mkdir(mode=0o777)
            slot.chmod(0o777)
            subprocess.run(task(slot, "usnm-boinc", "qwen", "local-unit", "--synthetic",
                                "--dir", "/work", "--k-crops", "2", "--timeout-secs", "1200"),
                           check=True, timeout=120)
            subprocess.run(task(slot), check=True, timeout=1500)
            result = json.loads((slot / "result.json").read_text())
            readings = json.loads((slot / "readings.json").read_text())
            assert (slot / "diag.txt").is_file()
            assert result["outcome"] == "ok" and result["device"] == "cpu" and result["model"] == "qwen3-vl-2b-q8"
            assert result["peak_rss_kib"] > 1000000, "llama-server child RSS missing"
            assert "newspaper" in " ".join(r["text"] for r in readings["readings"]).lower()
            report["cpu_result"] = {key: result.get(key) for key in
                                    ("outcome", "model", "device", "load_ms", "engine_ms", "peak_rss_kib")}
        report["passed"] = True
    finally:
        stop.set()
        if sampler is not None:
            sampler.join()
        (state / "measurement.json").write_text(json.dumps(report, indent=2) + "\n")
        pidfile = state / "daemon.pid"
        if pidfile.exists():
            subprocess.run(["sudo", "-n", "kill", pidfile.read_text().strip()], check=False)
        else:
            daemon.terminate()
        try:
            daemon.wait(timeout=30)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        log.close()


if __name__ == "__main__":
    main()
