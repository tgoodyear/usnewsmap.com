#!/usr/bin/env python3
"""Fetch and verify the checkpoint files listed by the EffOCR adapter lock."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tree_digest(files: list[dict[str, Any]]) -> str:
    payload = "".join(f"{row['path']} {row['sha256']}\n" for row in sorted(files, key=lambda row: row["path"]))
    return hashlib.sha256(payload.encode()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter-lock", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    lock = json.loads(args.adapter_lock.read_text(encoding="utf-8"))
    output = args.out_dir.resolve()

    from huggingface_hub import snapshot_download

    for name, model in lock["model_repositories"].items():
        target = output / name
        allowed = [row["path"] for row in model["files"]]
        snapshot_download(
            repo_id=model["repo"],
            revision=model["revision"],
            local_dir=target,
            allow_patterns=allowed,
        )
        for row in model["files"]:
            path = target / row["path"]
            if not path.is_file() or path.stat().st_size != row["bytes"] or sha256(path) != row["sha256"]:
                raise SystemExit(f"checkpoint hash mismatch: {name}/{row['path']}")
        if tree_digest(model["files"]) != model["file_tree_sha256"]:
            raise SystemExit(f"checkpoint tree digest mismatch: {name}")
        print(f"verified {model['repo']}@{model['revision']} ({model['file_tree_sha256']})")


if __name__ == "__main__":
    main()
