#!/usr/bin/env python3
"""Check the engine pins that are written in more than one place.

    scripts/ci/check-engine-pins.py [--root DIR]    (default: the repository)

The Surya worker image (#516). boinc/apps/surya/engine.lock is the one place its pins are
written: surya-ocr's version, the two llama.cpp image digests and the weights digest, as the
Containerfile's build arguments. Consumers read it: .github/actions/boinc-surya-build passes the
lines to `docker build`, and usnm-boinc compiles the version in (include_str!). What cannot read
it is checked here:

- the lock holds exactly those four keys, each well formed;
- SURYA_WEIGHTS is the sha256 of weights.lock's lines that are not comments, which is what the
  build computes and compares it with;
- the Containerfile declares each key as an ARG with no default, and holds no copy of a pin: no
  digest, no literal surya-ocr== or USNM_SURYA_VERSION=.

NDLOCR-Lite (#128). Its commit is pinned by ocr/engines/ndlocr-lite/models.lock, whose model URLs
and sha256s the image build checks, and written again where a Dockerfile, the Rust adapter or the
engine catalog cannot read the lock: both Dockerfiles that fetch the source archive (and its
sha256), the adapter's COMMIT, the notice, and the catalog's source, terms and model versions.
All must name one commit.

Standard library only. Exits 1 listing every finding.
"""

import argparse
import hashlib
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

SURYA = "boinc/apps/surya"
SURYA_KEYS = {
    "SURYA_VERSION": r"\d+\.\d+\.\d+",
    "LLAMA_DIGEST": r"sha256:[0-9a-f]{64}",
    "LLAMA_VULKAN_DIGEST": r"sha256:[0-9a-f]{64}",
    "SURYA_WEIGHTS": r"sha256:[0-9a-f]{64}",
}

NDLOCR_LOCK = "ocr/engines/ndlocr-lite/models.lock"
NDLOCR_IMAGES = ["ocr/engines/ndlocr-lite/Containerfile", "docker/Dockerfile.ja-ocr"]
NDLOCR_NOTICE = "ocr/engines/ndlocr-lite/NOTICE"
NDLOCR_ADAPTER = "crates/usnm-ocr-adapter/src/ndlocr/mod.rs"
CATALOG = "ocr/engines/engines.toml"
SHA1 = r"(?<![0-9a-f])[0-9a-f]{40}(?![0-9a-f])"


def read(root: Path, rel: str, out: list[str], raw: bool = False) -> str:
    """A file's text, with newlines as git stores them unless `raw` keeps any carriage return."""
    try:
        text = (root / rel).read_bytes().decode("utf-8")
        return text if raw else text.replace("\r\n", "\n")
    except (OSError, UnicodeDecodeError) as e:
        out.append(f"{rel}: not readable ({getattr(e, 'strerror', None) or type(e).__name__})")
        return ""


def code_lines(text: str) -> list[str]:
    """The lines of a Containerfile that are not comments."""
    return [line for line in text.splitlines() if not line.lstrip().startswith("#")]


def read_pins(text: str, rel: str, out: list[str]) -> dict[str, str]:
    """The KEY=value lines of a pin lock; comments and blank lines are skipped."""
    pins: dict[str, str] = {}
    # Split on \n alone: a carriage return left in a value would reach `docker build --build-arg`.
    for n, line in enumerate(text.split("\n"), 1):
        if not line.strip() or line.startswith("#"):
            continue
        key, eq, value = line.partition("=")
        if not eq or not re.fullmatch(r"[A-Z][A-Z_]*", key) or not value or value != value.strip():
            out.append(f"{rel}:{n}: not a KEY=value line: {line!r}")
        elif key in pins:
            out.append(f"{rel}:{n}: {key} is set twice")
        else:
            pins[key] = value
    return pins


def weights_digest(weights_lock: str) -> str:
    """What the Containerfile's weights stage computes: sha256 of the lock's non-comment lines."""
    body = "".join(line + "\n" for line in weights_lock.splitlines() if not line.startswith("#"))
    return "sha256:" + hashlib.sha256(body.encode()).hexdigest()


def surya_problems(root: Path) -> list[str]:
    out: list[str] = []
    lock_rel = f"{SURYA}/engine.lock"
    pins = read_pins(read(root, lock_rel, out, raw=True), lock_rel, out)
    for key, shape in SURYA_KEYS.items():
        if key not in pins:
            out.append(f"{lock_rel}: no {key}")
        elif not re.fullmatch(shape, pins[key]):
            out.append(f"{lock_rel}: {key}={pins[key]} is not {shape}")
    for key in sorted(set(pins) - set(SURYA_KEYS)):
        out.append(f"{lock_rel}: {key} is not a pin the Containerfile takes")

    weights = read(root, f"{SURYA}/weights.lock", out)
    if weights and "SURYA_WEIGHTS" in pins and pins["SURYA_WEIGHTS"] != weights_digest(weights):
        out.append(
            f"{lock_rel}: SURYA_WEIGHTS={pins['SURYA_WEIGHTS']} but the lines of {SURYA}/weights.lock "
            f"that are not comments hash to {weights_digest(weights)}"
        )

    containerfile_rel = f"{SURYA}/Containerfile"
    containerfile = read(root, containerfile_rel, out)
    code = "\n".join(code_lines(containerfile))
    for key in SURYA_KEYS:
        if re.search(rf"^ARG {key}=", code, re.M):
            out.append(f"{containerfile_rel}: ARG {key} has a default; the value comes from engine.lock")
        elif containerfile and not re.search(rf"^ARG {key}$", code, re.M):
            out.append(f"{containerfile_rel}: no `ARG {key}` for the pin in engine.lock")
    for what, pattern in [
        ("a digest", r"sha256:[0-9a-f]{64}"),
        ("a pinned surya-ocr version", r"surya-ocr==\d"),
        ("a pinned USNM_SURYA_VERSION", r"USNM_SURYA_VERSION=\d"),
    ]:
        if re.search(pattern, code):
            out.append(f"{containerfile_rel}: holds {what}; read it from engine.lock's build argument")
    return out


def ndlocr_problems(root: Path) -> list[str]:
    out: list[str] = []
    found: dict[str, set[str]] = {}  # commit -> where it is written

    def note(where: str, commits: list[str]) -> None:
        if not commits:
            out.append(f"{where}: names no commit")
        for commit in commits:
            found.setdefault(commit, set()).add(where)

    lock = read(root, NDLOCR_LOCK, out)
    note(f"{NDLOCR_LOCK} header", re.findall(rf"^# NDLOCR-Lite at ({SHA1});", lock, re.M))
    urls = [line.split()[1] for line in lock.splitlines() if line.strip() and not line.startswith("#")]
    note(f"{NDLOCR_LOCK} urls", [c for url in urls for c in re.findall(rf"/ndl-lab/ndlocr-lite/({SHA1})/", url)])
    note(NDLOCR_NOTICE, re.findall(rf"^Commit: ({SHA1})$", read(root, NDLOCR_NOTICE, out), re.M))
    note(NDLOCR_ADAPTER, re.findall(rf'pub const COMMIT: &str = "({SHA1})";', read(root, NDLOCR_ADAPTER, out)))

    checksums: dict[str, set[str]] = {}
    for rel in NDLOCR_IMAGES:
        code = "\n".join(code_lines(read(root, rel, out)))
        note(rel, re.findall(SHA1, code))
        for sha in re.findall(r"--checksum=sha256:([0-9a-f]{64})", code):
            checksums.setdefault(sha, set()).add(rel)
    if len(checksums) > 1:
        out.append(
            "the NDLOCR-Lite source archive's sha256 differs between the Dockerfiles: "
            + "; ".join(f"{sha} in {', '.join(sorted(where))}" for sha, where in sorted(checksums.items()))
        )

    try:
        catalog = tomllib.loads(read(root, CATALOG, out))
        engine = next(e for e in catalog["engine"] if e["id"] == "ndlocr-lite")
    except (tomllib.TOMLDecodeError, KeyError, StopIteration) as e:
        out.append(f"{CATALOG}: no readable ndlocr-lite engine ({type(e).__name__})")
    else:
        where = f"{CATALOG} (ndlocr-lite)"
        note(where, re.findall(rf"/ndlocr-lite/tree/({SHA1})$", engine.get("source", "")))
        note(where, re.findall(rf"/ndlocr-lite/blob/({SHA1})/", engine.get("terms", "")))
        for model in engine.get("model", []):
            note(where, [model.get("version", "")] if re.fullmatch(SHA1, model.get("version", "")) else [])
            note(where, re.findall(rf"/ndl-lab/ndlocr-lite/({SHA1})/", model.get("source", "")))
            # A model's licence link into ndlocr-lite carries the commit too; a licence that
            # lives elsewhere (DINOv3's) has its own revision and is not this pin.
            if "/ndl-lab/ndlocr-lite/" in model.get("terms", ""):
                note(where, re.findall(rf"/ndl-lab/ndlocr-lite/blob/({SHA1})/", model["terms"]))

    if len(found) > 1:
        out.append(
            "the NDLOCR-Lite commit is not one commit: "
            + "; ".join(f"{commit} in {', '.join(sorted(where))}" for commit, where in sorted(found.items()))
        )
    return out


def problems(root: Path) -> list[str]:
    return surya_problems(root) + ndlocr_problems(root)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", type=Path, default=ROOT, help="the repository (default: this one)")
    found = problems(ap.parse_args().root)
    for line in found:
        print(f"error: {line}", file=sys.stderr)
    if found:
        return 1
    print("engine pins agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
