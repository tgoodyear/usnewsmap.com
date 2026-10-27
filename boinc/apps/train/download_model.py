#!/usr/bin/env python3
"""Fetch only pinned model data, verifying upstream Git/LFS identities, never model code."""
import argparse
import hashlib
import json
from pathlib import Path
from urllib.request import urlopen

from worker import BASE_REVISION

FILES = {
    "README.md": (7136, "13afcb1d2c695b57ebe4ee740db48671069566ce"),
    "chat_template.json": (5502, "e49e75bdda104c8de941eb20e0b3765184fb27bc"),
    "config.json": (1505, "0cd8c646eb55794594b14fefcd1d3acea5cc000f"),
    "generation_config.json": (269, "7bb37933d2d86e4ceaabcc6540be989658f238e9"),
    "merges.txt": (1671839, "20024bfe7c83998e9aeaf98a0cd6a2ce6306c2f0"),
    "model.safetensors": (4255140312, "7de1838c87a5349b016c26a1c3f7d2bc400a3d485f95ef39a7059ffd734977a0"),
    "preprocessor_config.json": (390, "2ea84a437d448ff71b08df68fdd949d5cc4ebb64"),
    "tokenizer.json": (7032403, "c6cc1014128b19d1fc46b1d30a23e3b1d35db421"),
    "tokenizer_config.json": (10868, "d3d3763207692c78780f4bf42d4dadf49a5c8012"),
    "video_preprocessor_config.json": (385, "3ba673a5ad7d4d13f54155ecd38b2a94a6dac8fe"),
    "vocab.json": (2776833, "4783fe10ac3adce15ac8f358ef5462739852c569"),
}


def download(out: Path, processor_only: bool = False) -> None:
    out.mkdir(parents=True, exist_ok=True)
    for name, (size, expected) in FILES.items():
        if processor_only and name == "model.safetensors":
            continue
        hasher = hashlib.sha256() if len(expected) == 64 else hashlib.sha1()
        if len(expected) == 40:
            hasher.update(f"blob {size}\0".encode())
        url = f"https://huggingface.co/Qwen/Qwen3-VL-2B-Instruct/resolve/{BASE_REVISION}/{name}"
        count = 0
        with urlopen(url, timeout=120) as source, (out / name).open("wb") as dest:
            while chunk := source.read(8 * 1024 * 1024):
                count += len(chunk)
                if count > size:
                    raise ValueError(f"{name}: model file exceeds pinned size")
                hasher.update(chunk)
                dest.write(chunk)
        if count != size or hasher.hexdigest() != expected:
            raise ValueError(f"{name}: model file differs from pinned identity")
    (out / "revision.json").write_text(json.dumps({"revision": BASE_REVISION}) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--processor-only", action="store_true")
    args = parser.parse_args()
    download(args.out, args.processor_only)
