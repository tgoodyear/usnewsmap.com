#!/usr/bin/env python3
"""Check that every place that installs Rust uses the version in rust-toolchain.toml.

    scripts/ci/check-rust-toolchain.py [--root DIR]    (default: the repository)

rust-toolchain.toml names one exact release, such as "1.100.0", and the rustfmt and clippy
components. A new stable release then cannot change what CI builds until someone edits that file.
This checks what could drift from it:

- the channel is an exact x.y.z release, not stable, beta or nightly, and the components include
  rustfmt and clippy;
- every use of dtolnay/rust-toolchain in .github/workflows and .github/actions takes its version
  from the step with id rust-toolchain that precedes it, which reads rust-toolchain.toml, or
  names the pinned version itself;
- a Dockerfile or Containerfile whose Rust image tag names a release (rust:1.100.0-bookworm) names
  the pinned one. Floating tags (rust:1-bookworm) are left to rustup, which installs the version
  in the copied rust-toolchain.toml; a digest is not read.

Standard library only. Exits 1 listing every finding.
"""

import argparse
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
VERSION = re.compile(r"\d+\.\d+\.\d+")
ACTION = re.compile(r"^(\s*)-\s+uses:\s*dtolnay/rust-toolchain@")
EXPRESSION = "${{ steps.rust-toolchain.outputs.channel }}"
IMAGE_TAG = re.compile(r"\brust:(\d+\.\d+(?:\.\d+)?)(?=[-@\s]|$)")


def read_pin(root: Path, findings: list[str]) -> str | None:
    try:
        toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]
    except (OSError, KeyError, tomllib.TOMLDecodeError) as error:
        findings.append(f"rust-toolchain.toml: cannot read [toolchain]: {error}")
        return None
    channel = toolchain.get("channel", "")
    if not VERSION.fullmatch(channel):
        findings.append(f'rust-toolchain.toml: channel {channel!r} is not an exact release such as "1.100.0"')
        channel = None
    for component in ("rustfmt", "clippy"):
        if component not in toolchain.get("components", []):
            findings.append(f"rust-toolchain.toml: components lacks {component}")
    return channel


def check_workflow(rel: str, text: str, pin: str | None, findings: list[str]) -> None:
    lines = text.splitlines()
    reads_file = False
    for number, line in enumerate(lines, 1):
        if re.match(r"^\s*(-\s+)?id:\s*rust-toolchain\s*$", line):
            # The step's own lines, up to the next step at the same indent.
            block = []
            for follow in lines[number:]:
                if re.match(r"^\s*-\s", follow) and len(follow) - len(follow.lstrip()) <= len(line) - len(line.lstrip()) - 2:
                    break
                block.append(follow)
            reads_file = "rust-toolchain.toml" in " ".join(block[:6])
        match = ACTION.match(line)
        if not match:
            continue
        indent = len(match.group(1))
        value = None
        for follow in lines[number:]:
            if follow.strip() and len(follow) - len(follow.lstrip()) <= indent:
                break
            found = re.match(r"^\s*toolchain:\s*(.*?)\s*$", follow)
            if found:
                value = found.group(1)
                break
        where = f"{rel}:{number}"
        if value is None:
            findings.append(f"{where}: dtolnay/rust-toolchain without a toolchain input")
        elif value == EXPRESSION:
            if not reads_file:
                findings.append(f"{where}: toolchain reads steps.rust-toolchain, but no earlier step with id rust-toolchain reads rust-toolchain.toml")
        elif pin is None or value != pin:
            findings.append(f"{where}: toolchain {value!r} is not {EXPRESSION} or the pinned {pin}")


def check_image(rel: str, text: str, pin: str | None, findings: list[str]) -> None:
    for number, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        for match in IMAGE_TAG.finditer(line):
            if match.group(1) != pin:
                findings.append(f"{rel}:{number}: Rust image {match.group(1)} is not the pinned {pin}")


def check(root: Path) -> list[str]:
    findings: list[str] = []
    pin = read_pin(root, findings)
    patterns = [".github/workflows/*.yml", ".github/actions/*/action.yml"]
    for pattern in patterns:
        for path in sorted(root.glob(pattern)):
            check_workflow(path.relative_to(root).as_posix(), path.read_text(), pin, findings)
    images = [*root.glob("**/Dockerfile*"), *root.glob("**/*Containerfile*")]
    for path in sorted(set(images)):
        rel = path.relative_to(root)
        if "target" in rel.parts or "node_modules" in rel.parts or ".git" in rel.parts:
            continue
        check_image(rel.as_posix(), path.read_text(errors="replace"), pin, findings)
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--root", type=Path, default=ROOT, help="repository root (default: this repository)")
    args = parser.parse_args()
    findings = check(args.root)
    for finding in findings:
        print(f"check-rust-toolchain: {finding}", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
