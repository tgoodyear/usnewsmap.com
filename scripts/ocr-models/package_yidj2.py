#!/usr/bin/env python3
"""Build and check the release assets of the yidj2 Tesseract model (#1479).

    scripts/ocr-models/package_yidj2.py build --model FILE --base-license FILE \\
        --training-split FILE --recipe-dir DIR --out DIR [--update-sums]
    scripts/ocr-models/package_yidj2.py verify --dir DIR

`ocr/models/yidj2/` holds what the release publishes besides the model: the
notice, the provenance record and SHA256SUMS. `build` checks each input
against `provenance.json` by sha256, copies the inputs and those files under
their asset names, packs the Study 10 recipe into a reproducible tar, and
fails unless the result matches the committed SHA256SUMS. `--update-sums`
rewrites the committed SHA256SUMS instead, for a deliberate change to the
notice or provenance. `verify` checks a downloaded release: exactly the
listed assets, each with its committed sha256.

The inputs live outside the repository: the model and the training-split
hash list are kept with Study 10's runs, the base licence is tessdata_best's
LICENSE and the recipe is #1428's scripts/yiddish/ (provenance.json pins
all of them). Standard library only.
"""

import argparse
import hashlib
import io
import json
import shutil
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODEL_DIR = ROOT / "ocr/models/yidj2"
SUMS = "SHA256SUMS"
RECIPE_TAR = "recipe-study10.tar"
RECIPE_PREFIX = "recipe-study10"
EXECUTABLE = {"study10.sh", "cut-lines.py"}


class PackageError(Exception):
    pass


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def file_sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_provenance(model_dir):
    path = model_dir / "provenance.json"
    text = path.read_text(encoding="utf-8")
    doc = json.loads(text)
    canonical = json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    if text != canonical:
        raise PackageError(f"{path} is not canonical JSON (indent 2, sorted keys, final newline)")
    return doc


def parse_sums(text):
    sums = {}
    for n, line in enumerate(text.splitlines(), 1):
        digest, sep, name = line.partition("  ")
        if not sep or len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest) or not name:
            raise PackageError(f"{SUMS} line {n}: want '<sha256>  <name>'")
        if name in sums:
            raise PackageError(f"{SUMS} lists {name} twice")
        sums[name] = digest
    return sums


def format_sums(sums):
    return "".join(f"{sums[name]}  {name}\n" for name in sorted(sums))


def recipe_tar(recipe_dir, files):
    """A tar that depends only on the files' names and bytes."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name in sorted(files):
            data = (recipe_dir / name).read_bytes()
            info = tarfile.TarInfo(f"{RECIPE_PREFIX}/{name}")
            info.size = len(data)
            info.mode = 0o755 if name in EXECUTABLE else 0o644
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def expect(path, want, what):
    got = file_sha256(path)
    if got != want:
        raise PackageError(f"{what} {path}: sha256 {got}, provenance.json pins {want}")


def build(model, base_license, training_split, recipe_dir, out, model_dir=MODEL_DIR, update_sums=False):
    prov = load_provenance(model_dir)
    m, base, data, recipe = prov["model"], prov["start_model"], prov["training_data"], prov["recipe"]
    expect(model, m["sha256"], "model")
    if model.stat().st_size != m["bytes"]:
        raise PackageError(f"model {model}: {model.stat().st_size} bytes, provenance.json says {m['bytes']}")
    expect(base_license, base["license_file"]["sha256"], "base licence")
    expect(training_split, data["page_list"]["sha256"], "training split")
    for name, want in recipe["files"].items():
        expect(recipe_dir / name, want, "recipe file")
    extra = sorted(p.name for p in recipe_dir.iterdir() if p.name not in recipe["files"])
    if extra:
        raise PackageError(f"recipe dir {recipe_dir} has files provenance.json doesn't pin: {', '.join(extra)}")

    if out.exists() and any(out.iterdir()):
        raise PackageError(f"{out} is not empty")
    out.mkdir(parents=True, exist_ok=True)
    copies = {
        m["file"]: model,
        base["license_file"]["asset"]: base_license,
        data["page_list"]["asset"]: training_split,
        "NOTICE.md": model_dir / "NOTICE.md",
        "provenance.json": model_dir / "provenance.json",
    }
    for name, src in copies.items():
        shutil.copyfile(src, out / name)
    (out / recipe["asset"]).write_bytes(recipe_tar(recipe_dir, recipe["files"]))

    sums = {name: file_sha256(out / name) for name in [*copies, recipe["asset"]]}
    text = format_sums(sums)
    want_assets = sorted(prov["release"]["assets"])
    if sorted([*sums, SUMS]) != want_assets:
        raise PackageError(f"built {sorted([*sums, SUMS])}, provenance.json lists {want_assets}")
    committed = model_dir / SUMS
    if update_sums:
        committed.write_text(text, encoding="utf-8")
    elif committed.read_text(encoding="utf-8") != text:
        raise PackageError(f"the build doesn't match {committed}; a deliberate change needs --update-sums")
    (out / SUMS).write_text(text, encoding="utf-8")
    return sums


def verify(directory, model_dir=MODEL_DIR):
    prov = load_provenance(model_dir)
    committed = (model_dir / SUMS).read_text(encoding="utf-8")
    want = parse_sums(committed)
    names = sorted(p.name for p in directory.iterdir())
    if names != sorted(prov["release"]["assets"]):
        raise PackageError(f"{directory} holds {names}, want {sorted(prov['release']['assets'])}")
    if (directory / SUMS).read_text(encoding="utf-8") != committed:
        raise PackageError(f"{directory / SUMS} differs from {model_dir / SUMS}")
    for name, digest in want.items():
        got = file_sha256(directory / name)
        if got != digest:
            raise PackageError(f"{name}: sha256 {got}, want {digest}")
    if want[prov["model"]["file"]] != prov["model"]["sha256"]:
        raise PackageError("SHA256SUMS and provenance.json disagree on the model")
    return want


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("build", help="build the release assets into an empty directory")
    b.add_argument("--model", type=Path, required=True, help="yidj2.traineddata")
    b.add_argument("--base-license", type=Path, required=True, help="tessdata_best's LICENSE at the pinned commit")
    b.add_argument("--training-split", type=Path, required=True, help="Study 10's training-split hash list")
    b.add_argument("--recipe-dir", type=Path, required=True, help="#1428's scripts/yiddish/ files that provenance.json pins")
    b.add_argument("--out", type=Path, required=True, help="output directory, absent or empty")
    b.add_argument("--update-sums", action="store_true", help=f"rewrite the committed {SUMS} instead of checking it")
    v = sub.add_parser("verify", help="check a downloaded release against the committed SHA256SUMS")
    v.add_argument("--dir", type=Path, required=True, help="directory holding exactly the release's assets")
    args = parser.parse_args(argv)
    try:
        if args.command == "build":
            sums = build(args.model, args.base_license, args.training_split, args.recipe_dir, args.out,
                         update_sums=args.update_sums)
        else:
            sums = verify(args.dir)
    except (PackageError, OSError, KeyError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    print(format_sums(sums), end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
