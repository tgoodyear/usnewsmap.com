#!/usr/bin/env python3
"""Bounded read-only worker capture transport and replayable catalog ingestion."""

import argparse
import base64
import fcntl
import hashlib
import heapq
import os
from pathlib import Path
import re
import resource
import stat
import subprocess
import sys

if __package__:
    from . import data_catalog as catalog
else:
    import data_catalog as catalog


NAME = re.compile(r"[0-9a-f]{64}\.json\Z")
MAX_FILES = 100
MAX_RESPONSE = 10 * 1024 * 1024


def immutable(path, raw):
    """Publish durable bytes without replacing an existing identity."""
    if path.exists():
        catalog.require(path.read_bytes() == raw, "retained evidence identity conflict")
        return
    pending = path.with_name("." + path.name + ".pending")
    # A process killed before publication may leave only this unpublished sibling.
    with pending.open("wb") as stream:
        os.chmod(pending, 0o600)
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    os.link(pending, path)
    pending.unlink()
    with open_directory(path.parent) as directory:
        os.fsync(directory)


class open_directory:
    def __init__(self, path):
        self.fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)

    def __enter__(self):
        return self.fd

    def __exit__(self, *_):
        os.close(self.fd)


def export(root, after):
    catalog.require(not after or NAME.fullmatch(after), "invalid export cursor")
    root = Path(root)
    catalog.require(root.is_dir() and not root.is_symlink(), "invalid capture directory")
    # scandir avoids materializing the ever-growing spool; select the next bounded page.
    with os.scandir(root) as entries:
        names = heapq.nsmallest(MAX_FILES + 1, (
            entry.name for entry in entries if NAME.fullmatch(entry.name) and entry.name > after
        ))
    files = []
    for name in names[:MAX_FILES]:
        fd = os.open(root / name, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            catalog.require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "non-regular capture")
            raw = stream.read(catalog.MAX_LINE + 1)
        catalog.require(0 < len(raw) <= catalog.MAX_LINE, "capture exceeds adapter limit 64 KiB")
        metadata = catalog.load_json(raw)
        catalog.require(name == hashlib.sha256(metadata["run_id"].encode()).hexdigest() + ".json",
                        "capture filename does not match run identity")
        files.append({"name": name, "sha256": hashlib.sha256(raw).hexdigest(),
                      "bytes": base64.b64encode(raw).decode("ascii")})
    result = {"schema": 1, "files": files,
              "next": names[MAX_FILES - 1] if len(names) > MAX_FILES else ""}
    catalog.require(len(catalog.canonical(result).encode()) <= MAX_RESPONSE, "export too large")
    return result


def acknowledge(root, receipts):
    """Move checksum-matched captures aside only after the receiver retained/imported them."""
    root = Path(root)
    archived = root / "acknowledged"
    archived.mkdir(mode=0o700, exist_ok=True)
    catalog.require(not archived.is_symlink(), "invalid acknowledgement directory")
    for name, checksum in receipts:
        catalog.require(NAME.fullmatch(name), "invalid acknowledgement filename")
        catalog.sha(checksum)
        path = root / name
        destination = archived / name
        current = path if path.exists() else destination
        fd = os.open(current, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            catalog.require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "invalid capture")
            raw = stream.read(catalog.MAX_LINE + 1)
        catalog.require(len(raw) <= catalog.MAX_LINE and hashlib.sha256(raw).hexdigest() == checksum,
                        "acknowledgement checksum conflict")
        if path.exists():
            catalog.require(not destination.exists(), "duplicate acknowledgement destination")
            os.rename(path, destination)
    for directory in (root, archived):
        with open_directory(directory) as fd:
            os.fsync(fd)
    return {"acknowledged": len(receipts)}


def receive(db, root, source, bundle):
    catalog.token(source)
    catalog.obj(bundle, ("schema", "files", "next"))
    catalog.require(type(bundle["schema"]) is int and bundle["schema"] == 1, "unsupported export")
    catalog.require(isinstance(bundle["files"], list) and len(bundle["files"]) <= MAX_FILES,
                    "oversized export")
    cursor = bundle["next"]
    catalog.require(isinstance(cursor, str) and (not cursor or NAME.fullmatch(cursor)),
                    "invalid next cursor")
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    imported = 0
    names = []
    for item in bundle["files"]:
        catalog.obj(item, ("name", "sha256", "bytes"))
        name = item["name"]
        catalog.require(isinstance(name, str) and NAME.fullmatch(name), "invalid capture name")
        catalog.sha(item["sha256"])
        raw = base64.b64decode(item["bytes"], validate=True)
        catalog.require(0 < len(raw) <= catalog.MAX_LINE and
                        hashlib.sha256(raw).hexdigest() == item["sha256"], "capture checksum mismatch")
        metadata = catalog.load_json(raw)
        catalog.require(isinstance(metadata, dict) and isinstance(metadata.get("run_id"), str),
                        "invalid capture identity")
        catalog.require(name == hashlib.sha256(metadata["run_id"].encode()).hexdigest() + ".json",
                        "capture identity mismatch")
        catalog.adapt_worker(metadata, "loc:" + metadata["doc_id"])
        names.append(name)
        capture = root / name
        evidence = root / (name[:-5] + ".jsonl")
        immutable(capture, raw)
        if evidence.exists():
            # Rebuild/retry from immutable evidence, never regenerate with a new timestamp.
            result = catalog.ingest(db, evidence)
        else:
            result = catalog.ingest_worker(db, capture, evidence, "worker-" + name[:-5], source)
        imported += result["inserted"]
    catalog.require(names == sorted(set(names)), "export filenames must be ordered and unique")
    catalog.require(not cursor or (len(names) == MAX_FILES and cursor == names[-1]),
                    "next cursor does not match export")
    return {"files": len(names), "inserted": imported, "next": cursor}


def sync(args):
    root = Path(args.state_dir)
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (root / "sync.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        catalog.require(re.fullmatch(r"[A-Za-z0-9_.-]+@[A-Za-z0-9][A-Za-z0-9.-]*", args.host),
                        "expected a fixed user@hostname SSH destination")
        command = ["ssh", "-oBatchMode=yes", "-oStrictHostKeyChecking=yes",
                   "-oConnectTimeout=10", "-oIdentitiesOnly=yes", "-i", args.identity,
                   "-oUserKnownHostsFile=" + str(root / "known_hosts"), args.host]
        # Redirect to a bounded local file rather than retaining unbounded subprocess output.
        def limits():
            resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_RESPONSE + 1, MAX_RESPONSE + 1))
        with (root / "response.pending").open("w+b") as response:
            subprocess.run(command + ["after -"], stdout=response, check=True,
                           timeout=90, preexec_fn=limits)
            catalog.require(response.tell() <= MAX_RESPONSE, "response exceeds transport limit")
            response.seek(0)
            bundle = catalog.load_json(response.read(MAX_RESPONSE + 1))
        result = receive(args.db, root / "retained" / args.source, args.source, bundle)
        catalog.atomic_snapshot(args.snapshot, catalog.snapshot(args.db), args.db)
        if bundle["files"]:
            receipts = " ".join(i["name"] + ":" + i["sha256"] for i in bundle["files"])
            result["acknowledgement"] = subprocess.check_output(
                command + ["ack " + receipts], timeout=30, text=True).strip()
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("export", help="forced SSH command: only after <hash.json|-> is accepted")
    p.add_argument("--root", required=True)
    p = sub.add_parser("sync")
    for flag in ("db", "state-dir", "source", "host", "identity", "snapshot"):
        p.add_argument("--" + flag, required=True)
    args = parser.parse_args()
    try:
        if args.command == "export":
            original = os.environ.get("SSH_ORIGINAL_COMMAND", "")
            match = re.fullmatch(r"after (-|[0-9a-f]{64}\.json)", original)
            if match:
                result = export(args.root, "" if match[1] == "-" else match[1])
            else:
                catalog.require(re.fullmatch(
                    r"ack [0-9a-f]{64}\.json:[0-9a-f]{64}(?: [0-9a-f]{64}\.json:[0-9a-f]{64}){0,99}",
                    original), "unsupported evidence export command")
                result = acknowledge(args.root, [part.split(":") for part in original[4:].split()])
        else:
            result = sync(args)
        print(catalog.canonical(result))
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        print(catalog.canonical({"error": str(exc)}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
