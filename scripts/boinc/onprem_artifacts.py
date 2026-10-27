#!/usr/bin/env python3
"""Root-local immutable BOINC artifact builder/installer (invoked by the pipeline)."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys

IMAGE = r"sha256:[0-9a-f]{64}"


def image_id(value):
    if not isinstance(value, str) or not re.fullmatch(r"(sha256:)?[0-9a-f]{64}", value):
        raise ValueError("invalid local image identity")
    return value if value.startswith("sha256:") else "sha256:" + value


def call(*argv):
    return subprocess.check_output(argv, text=True, timeout=14400).strip()


def inspect(image, source=None):
    value = json.loads(call("podman", "image", "inspect", image))[0]
    if image_id(value["Id"]) != image_id(image):
        raise ValueError("local image changed")
    if source and value.get("Labels", {}).get("org.usnewsmap.source") != source:
        raise ValueError("local image source differs from manifest")
    return image_id(value["Id"])


def validate(value):
    required = {"schema", "env", "host", "ct", "source_commit", "release", "images", "worker"}
    if set(value) not in (required, required | {"signing_source"}):
        raise ValueError("invalid on-prem manifest fields")
    if value["schema"] != 1 or value["env"] not in ("prod", "dev"):
        raise ValueError("invalid on-prem manifest schema/environment")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]*", value["host"]):
        raise ValueError("invalid manifest host")
    if not re.fullmatch(r"[1-9][0-9]{2,8}", value["ct"]):
        raise ValueError("invalid manifest container")
    if not re.fullmatch(r"[0-9a-f]{40}", value["source_commit"]):
        raise ValueError("invalid manifest source")
    images = value["images"]
    keys = {"base", "bin", "server", "bridge", "worker"}
    if set(images) not in (keys, keys | {"appver"}):
        raise ValueError("invalid manifest images")
    for key in ("base", "bin", "server", "bridge"):
        image_id(images[key])
    release = value["release"]
    if set(release) != {"app", "version", "platform", "plan_class", "kind"}:
        raise ValueError("invalid manifest release")
    if release["kind"] == "container":
        if not re.fullmatch(r"ghcr\.io/tgoodyear/usnm-boinc-worker@sha256:[0-9a-f]{64}", images["worker"]):
            raise ValueError("invalid public worker")
        worker = value["worker"]
        if (set(worker) != {"source_commit", "run", "attempt", "receipt_sha256"}
                or not re.fullmatch(r"[0-9a-f]{40}", worker["source_commit"])
                or not re.fullmatch(r"[0-9a-f]{64}", worker["receipt_sha256"])
                or any(type(worker[k]) is not int or worker[k] < 1 for k in ("run", "attempt"))):
            raise ValueError("invalid public worker provenance")
    elif release["kind"] == "native":
        if images["worker"] is not None or not isinstance(value["worker"], dict):
            raise ValueError("native release cannot consume a container worker")
        worker = value["worker"]
        if set(worker) != {"sha256", "source_commit", "extra_files"} or any(
                not re.fullmatch(pattern, worker[key]) for key, pattern in
                (("sha256", r"[0-9a-f]{64}"), ("source_commit", r"[0-9a-f]{40}"))):
            raise ValueError("invalid native worker provenance")
        if not isinstance(worker["extra_files"], dict):
            raise ValueError("invalid native extra-file provenance")
        for logical, entry in worker["extra_files"].items():
            if (not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", logical)
                    or not isinstance(entry, dict) or set(entry) != {"sha256", "source_commit"}
                    or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"])
                    or not re.fullmatch(r"[0-9a-f]{40}", entry["source_commit"])):
                raise ValueError("invalid native extra-file provenance")
    else:
        raise ValueError("unsupported release kind")
    if "appver" in images:
        image_id(images["appver"])
        if not re.fullmatch(r"[0-9a-f]{40}", value.get("signing_source", "")):
            raise ValueError("missing signing source")
    return value


def host_image(conf):
    lines = (conf / "server.env").read_text().splitlines()
    pins = [line.partition("=")[2] for line in lines if line.startswith("SERVER_IMAGE=")]
    if len(pins) != 1:
        raise ValueError("server.env must have exactly one SERVER_IMAGE")
    return image_id(pins[0]), lines


def verify_runtime(value, conf=Path("/etc/usnm-boinc")):
    validate(value)
    for key in ("base", "bin", "server", "bridge"):
        inspect(value["images"][key], value["source_commit"])
    if "appver" in value["images"]:
        inspect(value["images"]["appver"], value["signing_source"])
    current, _ = host_image(conf)
    if current != value["images"]["bridge"]:
        raise ValueError("server.env differs from reviewed bridge image")
    running = json.loads(call("podman", "container", "inspect", "usnm-boinc-server"))[0]
    if image_id(running["Image"]) != current or not running["State"]["Running"]:
        raise ValueError("running server differs from reviewed bridge image")


def install(value, conf=Path("/etc/usnm-boinc")):
    validate(value)
    for key in ("base", "bin", "server", "bridge"):
        inspect(value["images"][key], value["source_commit"])
    current, lines = host_image(conf)
    desired = value["images"]["bridge"]
    if current != desired:
        # Keep the old pin as an operator rollback receipt; never auto-rollback on failure.
        backup = conf / ("server.env.before-" + value["source_commit"])
        original = (conf / "server.env").read_bytes()
        if backup.exists() and backup.read_bytes() != original:
            raise ValueError("rollback receipt already exists with different contents")
        if not backup.exists():
            fd = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "wb") as stream:
                stream.write(original)
                stream.flush()
                os.fsync(stream.fileno())
        staged = conf / "server.env.bootstrap-new"
        fd = os.open(staged, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as stream:
            stream.write("\n".join("SERVER_IMAGE=" + desired if line.startswith("SERVER_IMAGE=")
                                   else line for line in lines) + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(staged, conf / "server.env")
    # Also reconciles interruption after env replacement but before restart.
    call("systemctl", "restart", "usnm-boinc-server.service")
    verify_runtime(value, conf)


def build(source, commit):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("invalid build source")
    images = {}
    for key, directory, args in (
            ("base", "base", []), ("bin", "bridge-bin", []),
            ("server", "server", ["BASE_IMAGE"]),
            ("bridge", "bridge", ["SERVER_IMAGE", "BIN_IMAGE"])):
        iid = source.parent / (key + ".iid")
        if iid.exists():
            images[key] = inspect(image_id(iid.read_text().strip()), commit)
            continue
        argv = ["podman", "build", "--platform", "linux/amd64", "--label",
                "org.usnewsmap.source=" + commit, "--iidfile", str(iid),
                "--file", str(source / "contrib-compute/images" / directory / "Containerfile")]
        for name in args:
            argv += ["--build-arg", name + "=" + images[{"BASE_IMAGE": "base", "SERVER_IMAGE": "server", "BIN_IMAGE": "bin"}[name]]]
        subprocess.run([*argv, str(source)], check=True, stdout=sys.stderr, timeout=14400)
        images[key] = inspect(image_id(iid.read_text().strip()), commit)
    return images


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--operation", choices=("build", "install", "verify"), required=True)
    parser.add_argument("--payload", required=True)
    args = parser.parse_args()
    value = json.loads(args.payload)
    with open("/run/usnm-boinc-step.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.operation == "build":
            result = build(Path(value["source"]), value["commit"])
        elif args.operation == "install":
            install(value)
            result = {"installed": value["images"]["bridge"]}
        elif args.operation == "verify":
            verify_runtime(value)
            result = {"verified": value["images"]["bridge"]}
        else:
            raise ValueError("unknown artifact operation")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
