#!/usr/bin/env python3
"""Whether boinc/project/native-build-trust.json records a passing challenge for a native OCR build.

  check-build-trust.py --image sha256:... --base sha256:... --platform arm64-apple-darwin

Each native build digest is its own unit of trust (#1418, docs/boinc/apps/native-ocr.md). Before
a build is released, bound or trusted, a project machine runs the calibrated challenge through it
and the result is recorded in the reviewed file: the build's platform and engine.base, and each
run's edit and reference character totals, its challenge manifest and the threshold it was judged
against. This passes when the build is recorded for --platform with --base, and at least one run
is at or under its threshold. Prints what it found and exits 0, or says why not and exits 1.
usnm-boinc reads the same file (crates/usnm-boinc/src/trust/builds.rs) and also requires the run
to be on the trust scope's own challenge and threshold.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = ROOT / "boinc/project/native-build-trust.json"
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
HEX64 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
PLATFORMS = ("x86_64-pc-linux-gnu", "arm64-apple-darwin", "x86_64-apple-darwin", "windows_x86_64")
MAX_EVIDENCE = 512


def _line(text) -> bool:
    return (isinstance(text, str) and text.strip() != "" and len(text.encode()) <= MAX_EVIDENCE
            and not any(ord(c) < 32 or ord(c) == 127 for c in text))


def _count(value) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def validate(registry) -> None:
    """Raises ValueError unless `registry` reads as usnm-boinc's BuildTrust does."""
    if not isinstance(registry, dict) or set(registry) != {"builds", "equivalent", "v"} or registry["v"] != 1:
        raise ValueError("not a version 1 native-build-trust.json")
    builds = registry["builds"]
    if not isinstance(builds, dict):
        raise ValueError("builds must be an object")
    for image, build in builds.items():
        if not DIGEST.fullmatch(image):
            raise ValueError(f"invalid build digest {image}")
        if not isinstance(build, dict) or set(build) != {"challenges", "engine_base", "platform", "source"}:
            raise ValueError(f"{image} must have challenges, engine_base, platform and source")
        if not DIGEST.fullmatch(str(build["engine_base"])) or build["platform"] not in PLATFORMS:
            raise ValueError(f"invalid engine_base or platform for {image}")
        source = build["source"]
        if (not isinstance(source, dict) or set(source) != {"artifact_sha256", "commit", "run"}
                or not HEX64.fullmatch(str(source["artifact_sha256"]))
                or not COMMIT.fullmatch(str(source["commit"]))
                or not _count(source["run"]) or source["run"] == 0):
            raise ValueError(f"invalid source for {image}")
        if not isinstance(build["challenges"], list):
            raise ValueError(f"challenges for {image} must be a list")
        for run in build["challenges"]:
            keys = {"char_edits", "crops", "evidence", "manifest_sha256", "max_cer", "ref_chars"}
            if (not isinstance(run, dict) or set(run) != keys
                    or not _count(run["char_edits"]) or not _count(run["crops"]) or run["crops"] == 0
                    or not _count(run["ref_chars"]) or run["ref_chars"] == 0
                    or run["char_edits"] > 16 * run["ref_chars"]
                    or not HEX64.fullmatch(str(run["manifest_sha256"]))
                    or isinstance(run["max_cer"], bool) or not isinstance(run["max_cer"], (int, float))
                    or not 0 <= run["max_cer"] <= 1 or not _line(run["evidence"])):
                raise ValueError(f"invalid challenge for {image}")
    pairs = registry["equivalent"]
    if not isinstance(pairs, list):
        raise ValueError("equivalent must be a list")
    seen = set()
    for pair in pairs:
        if not isinstance(pair, dict) or set(pair) != {"digests", "evidence"}:
            raise ValueError("an equivalence has digests and evidence")
        digests = pair["digests"]
        if (not isinstance(digests, list) or len(digests) != 2
                or not all(isinstance(d, str) and DIGEST.fullmatch(d) for d in digests)
                or not digests[0] < digests[1]):
            raise ValueError("an equivalence names two distinct digests in ascending order")
        if not _line(pair["evidence"]):
            raise ValueError("an equivalence needs its evidence")
        if tuple(digests) in seen:
            raise ValueError(f"duplicate equivalence {digests}")
        seen.add(tuple(digests))


def load(path: Path) -> dict:
    registry = json.loads(path.read_text())
    validate(registry)
    return registry


def passes(run) -> bool:
    """A run at or under the threshold it was judged against."""
    return run["char_edits"] / run["ref_chars"] <= run["max_cer"]


def check(registry, image: str, base: str, platform: str) -> str:
    """What the registry records for the build, or ValueError if it can't be released."""
    build = registry["builds"].get(image)
    if build is None:
        raise ValueError(f"{image} is not recorded in native-build-trust.json: run the calibrated "
                         "challenge through it on a project machine and record the result first")
    if build["platform"] != platform or build["engine_base"] != base:
        raise ValueError(f"{image} is recorded for {build['platform']} with engine.base "
                         f"{build['engine_base']}, not {platform} with {base}")
    passing = [run for run in build["challenges"] if passes(run)]
    if not passing:
        raise ValueError(f"{image} has no challenge run at or under its threshold")
    run = passing[0]
    cer = run["char_edits"] / run["ref_chars"]
    return (f"challenge recorded for {image} on {platform}: CER {cer:.4f} on "
            f"{run['manifest_sha256'][:8]}, threshold {run['max_cer']}")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--image", required=True)
    parser.add_argument("--base", required=True)
    parser.add_argument("--platform", required=True, choices=PLATFORMS)
    parser.add_argument("--registry", type=Path, default=REGISTRY)
    args = parser.parse_args(argv)
    try:
        print(check(load(args.registry), args.image, args.base, args.platform))
    except (OSError, ValueError) as error:
        print(f"check-build-trust: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
