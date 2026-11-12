#!/usr/bin/env python3
"""Check a native OCR build (usnm_ocr_native, usnm_gold_native; docs/boinc/apps/native-ocr.md) before a
release stages and signs it, and print the identity its jobs and the Gold binding name it by.

  check-native-ocr-build.py --platform x86_64-pc-linux-gnu --binary usnm_ocr \
    --extra-file usnm_native_build.json=PATH --extra-file usnm_tesseract=PATH \
    --extra-file tessdata_eng.traineddata=PATH ...

The build must read as the container worker does: the manifest's engine_version is the Tesseract
that ocr/engines/tesseract/Containerfile installs, its models are exactly
ocr/engines/tesseract/models.lock's, and every file given is the one the manifest lists, by size
and sha256. The manifest must be for --platform. The worker, which only --binary gives, and the
engine must be that platform's executables: x86-64 ELF executables or position-independent
executables on Linux, thin 64-bit Mach-O executables of the platform's architecture on macOS, and
PE32+ x86-64 executables (not DLLs) on Windows.
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
NOTICE_PREFIX = "notice_"
# The models whose releases carry a notice and licence, and their files (crates/usnm-boinc/src/
# native.rs, NOTICES): checked against the release's SHA256SUMS, the repository's copy, which
# ocr/engines/tesseract/Containerfile pins by hash (ARG CHR_PHOENIX_SUMS).
NOTICES = {"chr_phoenix": (ROOT / "ocr/models/chr-phoenix/SHA256SUMS", "CHR_PHOENIX_SUMS",
                           ("NOTICE.md", "LICENSE-Apache-2.0.txt"))}
# Each platform is its own build, digest and trust (docs/boinc/apps/native-ocr.md).
PLATFORMS = ("x86_64-pc-linux-gnu", "arm64-apple-darwin", "x86_64-apple-darwin", "windows_x86_64")
EM_X86_64 = 0x3E
ET_EXEC, ET_DYN = 2, 3
MH_MAGIC_64 = 0xFEEDFACF
MH_EXECUTE = 2
CPU_TYPE_X86_64, CPU_TYPE_ARM64 = 0x01000007, 0x0100000C
IMAGE_FILE_MACHINE_AMD64 = 0x8664
PE_OPTIONAL_MAGIC_64 = 0x20B
PE32_PLUS_FIXED_SIZE, SECTION_HEADER_SIZE = 112, 40
IMAGE_FILE_EXECUTABLE_IMAGE, IMAGE_FILE_DLL = 0x2, 0x2000
MACH_O_CPU = {"arm64-apple-darwin": CPU_TYPE_ARM64, "x86_64-apple-darwin": CPU_TYPE_X86_64}


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
        name = MODEL_PREFIX + fields[0].removeprefix("tessdata/")
        if name in models:
            raise ValueError(f"model locked twice in {lock}: {fields[0]}")
        models[name] = fields[2]
    if not models:
        raise ValueError(f"no models in {lock}")
    return models


def locked_notices(containerfile: Path) -> dict[str, str]:
    """The locked notices by the build's names (notice_<model>_<file>), from each release's
    SHA256SUMS, itself checked against the hash the engine's Containerfile pins."""
    text = containerfile.read_text()
    out = {}
    for model, (sums, arg, files) in NOTICES.items():
        raw = sums.read_bytes()
        pinned = re.findall(rf"^ARG {arg}=([0-9a-f]{{64}})$", text, re.M)
        if pinned != [hashlib.sha256(raw).hexdigest()]:
            raise ValueError(f"{sums} isn't the SHA256SUMS {containerfile} pins")
        listed = dict(reversed(line.split("  ", 1)) for line in raw.decode().splitlines() if "  " in line)
        for name in files:
            if not re.fullmatch(r"[0-9a-f]{64}", listed.get(name, "")):
                raise ValueError(f"{sums} has no sha256 for {name}")
            out[f"{NOTICE_PREFIX}{model}_{name}"] = listed[name]
    return out


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


def is_mach_o_executable(path: Path, cpu: int) -> bool:
    """A thin 64-bit Mach-O executable for `cpu`: not a universal file, a library or an object."""
    with path.open("rb") as f:
        head = f.read(16)
    if len(head) != 16:
        return False
    magic, cputype, _, filetype = struct.unpack("<IIII", head)
    return magic == MH_MAGIC_64 and cputype == cpu and filetype == MH_EXECUTE


def is_x86_64_pe_executable(path: Path) -> bool:
    """A PE32+ x86-64 executable: not a DLL, a 32-bit image or an object file."""
    with path.open("rb") as f:
        size = f.seek(0, 2)
        f.seek(0)
        dos = f.read(64)
        if len(dos) != 64 or dos[:2] != b"MZ":
            return False
        (pe_offset,) = struct.unpack_from("<I", dos, 60)
        f.seek(pe_offset)
        head = f.read(4 + 20 + 2)
    if len(head) != 26 or head[:4] != b"PE\0\0":
        return False
    machine, sections, _, _, _, optional_size, characteristics = struct.unpack_from("<HHIIIHH", head, 4)
    (magic,) = struct.unpack_from("<H", head, 24)
    # The optional header (at least PE32+'s fixed part) and the section table must be in the file.
    if (sections == 0 or optional_size < PE32_PLUS_FIXED_SIZE
            or size < pe_offset + 24 + optional_size + SECTION_HEADER_SIZE * sections):
        return False
    return (machine == IMAGE_FILE_MACHINE_AMD64 and magic == PE_OPTIONAL_MAGIC_64
            and bool(characteristics & IMAGE_FILE_EXECUTABLE_IMAGE) and not characteristics & IMAGE_FILE_DLL)


def is_executable(path: Path, platform: str) -> bool:
    if platform == "windows_x86_64":
        return is_x86_64_pe_executable(path)
    if platform in MACH_O_CPU:
        return is_mach_o_executable(path, MACH_O_CPU[platform])
    return is_x86_64_elf(path)


def executable_kind(platform: str) -> str:
    if platform == "windows_x86_64":
        return "x86-64 PE32+ executable"
    if platform in MACH_O_CPU:
        return f"{platform.split('-')[0]} Mach-O executable"
    return "x86-64 ELF executable"



def _no_duplicate_fields(pairs):
    """object_pairs_hook: refuse an object that names a field twice, as the worker's decoder does."""
    fields = {}
    for key, value in pairs:
        if key in fields:
            raise ValueError(f"duplicate field {key!r}")
        fields[key] = value
    return fields

def check(binary: Path, extras: dict[str, Path], lock: Path, containerfile: Path,
          platform: str = "x86_64-pc-linux-gnu") -> tuple[str, str]:
    """Returns the build's (engine.image, engine.base)."""
    if platform not in PLATFORMS:
        raise ValueError(f"no native OCR build for {platform}: one of {', '.join(PLATFORMS)}")
    if WORKER in extras:
        raise ValueError(f"the worker is --binary, not --extra-file {WORKER}")
    if MANIFEST not in extras:
        raise ValueError(f"a native OCR build requires --extra-file {MANIFEST}=PATH")
    raw = extras[MANIFEST].read_bytes()
    try:
        manifest = json.loads(raw, object_pairs_hook=_no_duplicate_fields)
    except ValueError:
        manifest = None
    if not well_formed(manifest) or manifest["v"] != 1 or manifest["platform"] != platform:
        raise ValueError(f"{MANIFEST} is not a build manifest for {platform}")
    files, version = manifest["files"], manifest["engine_version"]
    engine = locked_engine(containerfile)
    if version != engine:
        raise ValueError(f"the build's Tesseract is {version}, the container worker's is {engine}")
    models = locked_models(lock)
    notices = locked_notices(containerfile)
    if set(files) != {WORKER, ENGINE, *models, *notices}:
        raise ValueError(f"{MANIFEST} doesn't list the worker, the engine and exactly the locked models"
                         " and notices")
    for name, sha256 in models.items():
        if files[name]["sha256"] != sha256:
            raise ValueError(f"the build's {name} isn't the locked model")
    for name, sha256 in notices.items():
        if files[name]["sha256"] != sha256:
            raise ValueError(f"the build's {name} isn't the model release's notice")
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
        if not is_executable(given[name], platform):
            raise ValueError(f"{name} is not an {executable_kind(platform)}")
    return f"sha256:{hashlib.sha256(raw).hexdigest()}", f"sha256:{files[ENGINE]['sha256']}"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--platform", default="x86_64-pc-linux-gnu", choices=PLATFORMS)
    ap.add_argument("--binary", required=True, type=Path)
    ap.add_argument("--extra-file", action="append", default=[], metavar="LOGICAL=PATH")
    ap.add_argument("--models-lock", type=Path, default=ROOT / "ocr/engines/tesseract/models.lock")
    ap.add_argument("--engine-containerfile", type=Path, default=ROOT / "ocr/engines/tesseract/Containerfile")
    a = ap.parse_args(argv)
    extras = {}
    for spec in a.extra_file:
        logical, sep, path = spec.partition("=")
        if not sep or not logical or not path or logical in extras:
            ap.error(f"--extra-file must be LOGICAL=PATH, both non-empty, each logical name once: {spec}")
        extras[logical] = Path(path)
    try:
        image, base = check(a.binary, extras, a.models_lock, a.engine_containerfile, a.platform)
    except (OSError, ValueError) as e:
        print(f"check-native-ocr-build: {e}", file=sys.stderr)
        return 1
    print(f"native OCR build for {a.platform}: engine.image {image}, engine.base {base}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
