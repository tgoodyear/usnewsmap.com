#!/usr/bin/env python3
"""Fonts for the render-vs-scan test (#2103): install the pinned open-licence period serif, find a font, record it.

    fonts.py --fetch [--cache DIR]       install the pinned font package (needs npm) into the cache, checking its hashes
    fonts.py --show  [--cache DIR] [--font SCRIPT=PATH ...]

Latin script: Old Standard TT (OFL-1.1), from the pinned @fontsource/old-standard-tt package in fonts.lock.json.
The font is a dependency, not a committed file: `--fetch` runs `npm pack` for the exact version, checks the
package's sha512 and each font file's sha256 against the lock, and unpacks them under the cache
(`$RENDER_COMPARE_FONTS`, else `~/.cache/render_compare/fonts`).

Cherokee: the traced "Phoenix 1828" font (CC0). Its file is not in the repository (the recipe is, in
ocr/models/chr-phoenix/recipe/chr_trace_font.py, and needs the glyph bank), so Cherokee is skipped unless
`--font chr=PATH` names a built Phoenix1828.ttf.

Every distance row carries `font_family`, `font_version` (the font's own version string), `font_sha256` and
`font_package`, from `FontInfo.record()`.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import subprocess
import sys
import tarfile
import tempfile
from dataclasses import dataclass
from pathlib import Path

import paths

LOCK = json.loads((paths.HERE / "fonts.lock.json").read_text(encoding="utf-8"))
CHEROKEE = {"chr"}


def script_of_lang(lang: str) -> str:
    return "chr" if lang in CHEROKEE else "latin"


def cache_dir(cache: str | Path | None = None) -> Path:
    return Path(cache or os.environ.get("RENDER_COMPARE_FONTS") or Path.home() / ".cache" / "render_compare" / "fonts")


def package_dir(cache: Path) -> Path:
    lat = LOCK["latin"]
    return cache / f"{lat['package'].replace('/', '__')}-{lat['package_version']}"


def sha256_file(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def fetch(cache: Path) -> list[Path]:
    """Install the pinned Latin font package into `cache`; raises if a hash differs. Returns the font files."""
    lat = LOCK["latin"]
    dest = package_dir(cache)
    with tempfile.TemporaryDirectory() as tmp:
        spec = f"{lat['package']}@{lat['package_version']}"
        out = subprocess.run(["npm", "pack", spec, "--pack-destination", tmp, "--silent"], check=True,
                             capture_output=True, text=True).stdout.strip().splitlines()[-1]
        tgz = Path(tmp) / out
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(tgz.read_bytes()).digest()).decode()
        if integrity != lat["package_integrity"]:
            raise SystemExit(f"{spec}: package integrity {integrity} is not the locked {lat['package_integrity']}")
        with tarfile.open(tgz) as tf:
            members = {m.name: m for m in tf.getmembers()}
            dest.mkdir(parents=True, exist_ok=True)
            got = []
            for f in lat["files"]:
                m = members.get(f"package/{f['path']}")
                if m is None or not m.isfile():
                    raise SystemExit(f"{spec}: {f['path']} is not in the package")
                data = tf.extractfile(m).read()
                if hashlib.sha256(data).hexdigest() != f["sha256"]:
                    raise SystemExit(f"{f['path']}: sha256 differs from the lock")
                p = dest / Path(f["path"]).name
                p.write_bytes(data)
                got.append(p)
            lic = members.get("package/LICENSE")
            if lic is not None:
                (dest / "LICENSE").write_bytes(tf.extractfile(lic).read())
    return got


@dataclass
class FontInfo:
    path: Path
    family: str
    version: str
    sha256: str
    licence: str
    package: str | None

    def record(self) -> dict:
        """The fields every distance row and run record carries."""
        return {"font_family": self.family, "font_version": self.version, "font_sha256": self.sha256,
                "font_licence": self.licence, "font_package": self.package}


def describe(path: Path, licence: str, package: str | None) -> FontInfo:
    from fontTools.ttLib import TTFont

    names = TTFont(str(path))["name"]
    return FontInfo(path=path, family=names.getDebugName(1) or path.stem, version=names.getDebugName(5) or "unknown",
                    sha256=sha256_file(path), licence=licence, package=package)


def resolve(script: str, overrides: dict[str, str] | None = None, cache: str | Path | None = None) -> list[FontInfo]:
    """The fonts to set `script` in, first choice first; empty when there is none (Cherokee without --font chr=PATH,
    or the Latin package not fetched yet)."""
    if overrides and script in overrides:
        p = Path(overrides[script])
        lock = LOCK.get(script, {})
        return [describe(p, lock.get("licence", "unknown"), None)]
    if script != "latin":
        return []
    lat = LOCK["latin"]
    out = []
    for f in lat["files"]:
        p = package_dir(cache_dir(cache)) / Path(f["path"]).name
        if p.exists():
            if sha256_file(p) != f["sha256"]:
                raise SystemExit(f"{p}: sha256 differs from the lock; run fonts.py --fetch again")
            out.append(describe(p, lat["licence"], f"{lat['package']}@{lat['package_version']}"))
    return out


def unavailable_reason(script: str) -> str:
    if script == "chr":
        return ("the traced 'Phoenix 1828' font is not in the repository (only ocr/models/chr-phoenix/recipe/"
                "chr_trace_font.py is, and it needs the glyph bank); pass --font chr=PATH to a built Phoenix1828.ttf")
    return "the Latin font is not installed; run fonts.py --fetch"


def parse_overrides(items: list[str] | None) -> dict[str, str]:
    out = {}
    for it in items or []:
        k, sep, v = it.partition("=")
        if not sep:
            raise SystemExit("--font wants SCRIPT=PATH")
        out[k] = v
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--fetch", action="store_true", help="install the pinned Latin font package into the cache")
    ap.add_argument("--show", action="store_true", help="print the font records")
    ap.add_argument("--cache", help="font cache (default $RENDER_COMPARE_FONTS or ~/.cache/render_compare/fonts)")
    ap.add_argument("--font", action="append", metavar="SCRIPT=PATH", help="use this font file for a script (chr, latin)")
    a = ap.parse_args(argv)
    if a.fetch:
        for p in fetch(cache_dir(a.cache)):
            print(f"installed {p}")
    if a.show or not a.fetch:
        for script in ("latin", "chr"):
            fs = resolve(script, parse_overrides(a.font), a.cache)
            if not fs:
                print(f"{script}: skipped, {unavailable_reason(script)}")
            for f in fs:
                print(f"{script}: {json.dumps(f.record())}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
