#!/usr/bin/env python3
"""Check a native OCR build (usnm_ocr_native, usnm_gold_native; docs/boinc-native-ocr.md) before a
release stages and signs it, and print the identity its jobs and the Gold binding name it by.

  check-native-ocr-build.py --binary usnm_ocr --extra-file usnm_native_build.json=PATH \
    --extra-file usnm_tesseract=PATH --extra-file tessdata_eng.traineddata=PATH ...

The build must read as the container worker does: the manifest's engine_version is the Tesseract
that ocr/engines/tesseract/Containerfile installs, its models are exactly
ocr/engines/tesseract/models.lock's, and every file given is the one the manifest lists, by size
and sha256. The worker, which only --binary gives, and the engine must be x86-64 ELF executables
or position-independent executables.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import sys

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "usnm_native_build.json"
WORKER = "usnm_ocr"
ENGINE = "usnm_tesseract"
MODEL_PREFIX = "tessdata_"
PLATFORM = "x86_64-pc-linux-gnu"
EM_X86_64 = 0x3E
ET_EXEC, ET_DYN = 2, 3


def locked_models(lock: Path) -> dict[str, str]:
    """The locked models by the build's names, as crates/usnm-boinc/src/native.rs reads them."""
    models = {}
    for line in lock.read_text().splitlines():
        fields = line.split()
        if line.startswith("#") or len(fields) != 3:
            continue
        if not fields[0].startswith("tessdata/"):
            raise ValueError(f"model outside tessdata in {lock}: {fields[0]}")
        if not re.fullmatch(r"[0-9a-f]{64}", fields[2]):
            raise ValueError(f"not a sha256 in {lock}: {fields[0]} {fields[2]}")
        models[MODEL_PREFIX + fields[0].removeprefix("tessdata/")] = fields[2]
    if not models:
        raise ValueError(f"no models in {lock}")
    return models


def locked_engine(containerfile: Path) -> str:
    """The upstream Tesseract version the container worker's engine image installs."""
    found = set(re.findall(r"\btesseract-ocr=([0-9]+\.[0-9]+\.[0-9]+)-", containerfile.read_text()))
    if len(found) != 1:
        raise ValueError(f"no single tesseract-ocr version in {containerfile}")
    return found.pop()


def well_formed(manifest) -> bool:
    """Whether the worker reads this as a build manifest (crates/usnm-boinc/src/native.rs,
    Manifest and BuildFile): exactly their fields, with integers that are JSON integers."""
    def integer(x, bits):
        return type(x) is int and 0 <= x < 2 ** bits
    return (isinstance(manifest, dict) and set(manifest) == {"engine_version", "files", "platform", "v"}
            and integer(manifest["v"], 32) and isinstance(manifest["engine_version"], str)
            and isinstance(manifest["platform"], str) and isinstance(manifest["files"], dict)
            and all(isinstance(f, dict) and set(f) == {"bytes", "sha256"} and integer(f["bytes"], 64)
                    and isinstance(f["sha256"], str) for f in manifest["files"].values()))


def entry(path: Path) -> dict:
    digest, size = hashlib.sha256(), 0
    with path.open("rb") as f:
        while chunk := f.read(1 << 20):
            digest.update(chunk)
            size += len(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}


def is_x86_64_elf(path: Path) -> bool:
    with path.open("rb") as f:
        head = f.read(20)
    if len(head) != 20 or head[:6] != b"\x7fELF\x02\x01":
        return False
    e_type, e_machine = struct.unpack_from("<HH", head, 16)
    return e_type in (ET_EXEC, ET_DYN) and e_machine == EM_X86_64


def check(binary: Path, extras: dict[str, Path], lock: Path, containerfile: Path) -> tuple[str, str]:
    """Returns the build's (engine.image, engine.base)."""
    if WORKER in extras:
        raise ValueError(f"the worker is --binary, not --extra-file {WORKER}")
    if MANIFEST not in extras:
        raise ValueError(f"a native OCR build requires --extra-file {MANIFEST}=PATH")
    raw = extras[MANIFEST].read_bytes()
    try:
        manifest = json.loads(raw)
    except ValueError:
        manifest = None
    if not well_formed(manifest) or manifest["v"] != 1 or manifest["platform"] != PLATFORM:
        raise ValueError(f"{MANIFEST} is not a build manifest for {PLATFORM}")
    files, version = manifest["files"], manifest["engine_version"]
    engine = locked_engine(containerfile)
    if version != engine:
        raise ValueError(f"the build's Tesseract is {version}, the container worker's is {engine}")
    models = locked_models(lock)
    if set(files) != {WORKER, ENGINE, *models}:
        raise ValueError(f"{MANIFEST} doesn't list the worker, the engine and exactly the locked models")
    for name, sha256 in models.items():
        if files[name]["sha256"] != sha256:
            raise ValueError(f"the build's {name} isn't the locked model")
    given = {WORKER: binary, **{k: v for k, v in extras.items() if k != MANIFEST}}
    if set(given) != set(files):
        missing = sorted(set(files) - set(given))
        unlisted = sorted(set(given) - set(files))
        raise ValueError("the files given aren't the build's: "
                         + "; ".join(f"{what}: {', '.join(names)}" for what, names in
                                     (("missing", missing), ("not in the manifest", unlisted)) if names))
    for name, path in given.items():
        if entry(path) != files[name]:
            raise ValueError(f"{name} differs from the build manifest")
    for name in (WORKER, ENGINE):
        if not is_x86_64_elf(given[name]):
            raise ValueError(f"{name} is not an x86-64 ELF executable")
    return f"sha256:{hashlib.sha256(raw).hexdigest()}", f"sha256:{files[ENGINE]['sha256']}"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", required=True, type=Path)
    ap.add_argument("--extra-file", action="append", default=[], metavar="LOGICAL=PATH")
    ap.add_argument("--models-lock", type=Path, default=ROOT / "ocr/engines/tesseract/models.lock")
    ap.add_argument("--engine-containerfile", type=Path, default=ROOT / "ocr/engines/tesseract/Containerfile")
    a = ap.parse_args(argv)
    extras = {}
    for spec in a.extra_file:
        logical, sep, path = spec.partition("=")
        if not sep or logical in extras:
            ap.error(f"--extra-file must be LOGICAL=PATH, each logical name once: {spec}")
        extras[logical] = Path(path)
    try:
        image, base = check(a.binary, extras, a.models_lock, a.engine_containerfile)
    except (OSError, ValueError) as e:
        print(f"check-native-ocr-build: {e}", file=sys.stderr)
        return 1
    print(f"native OCR build: engine.image {image}, engine.base {base}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
