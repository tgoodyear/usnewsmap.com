#!/usr/bin/env python3
"""Install the Japanese OCR runtime without containers, using the image's pinned recipe.

--check is offline and performs no installation. Installation creates a new prefix;
it never activates services, changes an existing runtime, or claims production work.
"""

import argparse
import hashlib
import json
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def recipe(root):
    dockerfile = root / "docker/Dockerfile.ja-ocr"
    text = dockerfile.read_text().replace("\\\n", "")
    commits = re.findall(r"^ARG NDLOCR_COMMIT=([0-9a-f]{40})$", text, re.M)
    archives = re.findall(
        r"^ADD --checksum=sha256:([0-9a-f]{64})\s+"
        r"https://github.com/ndl-lab/ndlocr-lite/archive/\$\{NDLOCR_COMMIT\}\.tar\.gz "
        r"/tmp/ndlocr-lite\.tar\.gz$", text, re.M)
    if len(commits) != 1 or len(archives) != 1:
        raise ValueError("Dockerfile must have one pinned NDLOCR source archive")
    commit = commits[0]
    lock = (root / "ocr/engines/ndlocr-lite/models.lock").read_text()
    if not lock.startswith(f"# NDLOCR-Lite at {commit};"):
        raise ValueError("Dockerfile NDLOCR commit differs from models.lock")
    for line in lock.splitlines():
        if line.strip() and not line.startswith("#"):
            fields = line.split()
            if len(fields) != 3 or f"/ndlocr-lite/{commit}/" not in fields[1]:
                raise ValueError("models.lock does not pin the same NDLOCR commit")
    requirements = "ocr/ja-ocr/requirements.txt"
    required = [
        f"COPY {requirements} /opt/jaocr/requirements.txt",
        "grep -v '^flet' /opt/ndlocr-lite/requirements.txt > /tmp/ndlocr-requirements.txt",
        "pip install --no-cache-dir -r /tmp/ndlocr-requirements.txt -r /opt/jaocr/requirements.txt",
    ]
    if not all(line in text for line in required):
        raise ValueError("Dockerfile requirements recipe changed; review native parity")
    copies = re.findall(r"^COPY (.+) /opt/jaocr/$", text, re.M)
    if len(copies) != 1:
        raise ValueError("Dockerfile must name one explicit Japanese Python source set")
    sources = shlex.split(copies[0])
    if not sources or any(
        not name.startswith("ocr/ja-ocr/") or not name.endswith(".py")
        or ".." in PurePosixPath(name).parts or not (root / name).is_file()
        for name in sources
    ):
        raise ValueError("invalid Japanese Python source set")
    if "COPY ocr/ja-ocr/wordlists/*.txt /opt/jaocr/wordlists/" not in text:
        raise ValueError("Dockerfile wordlist recipe changed; review native parity")
    return {
        "commit": commit, "archive_sha256": archives[0],
        "archive_url": f"https://github.com/ndl-lab/ndlocr-lite/archive/{commit}.tar.gz",
        "requirements": requirements, "sources": sources,
        "dockerfile_sha256": digest(dockerfile),
        "requirements_sha256": digest(root / requirements),
        "models_lock_sha256": digest(root / "ocr/engines/ndlocr-lite/models.lock"),
    }


def extract(archive, destination, commit):
    base = f"ndlocr-lite-{commit}"
    with tarfile.open(archive, "r:gz") as source:
        for member in source:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or path.parts[0] != base:
                raise ValueError("NDLOCR archive contains an unsafe path")
            if not member.isdir() and not member.isfile():
                raise ValueError("NDLOCR archive contains a link or special file")
            relative = Path(*path.parts[1:])
            if not relative.parts:
                continue
            target = destination / relative
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as data, target.open("xb") as output:
                    shutil.copyfileobj(data, output)


def install(root, prefix, plan):
    prefix.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="usnm-ja-source-") as work:
        archive = Path(work) / "source.tar.gz"
        with urllib.request.urlopen(plan["archive_url"], timeout=120) as source:
            with archive.open("xb") as output:
                shutil.copyfileobj(source, output)
        if digest(archive) != plan["archive_sha256"]:
            raise ValueError("NDLOCR source archive checksum mismatch")
        engine = prefix / "ndlocr-lite"
        extract(archive, engine, plan["commit"])
    for line in (root / "ocr/engines/ndlocr-lite/models.lock").read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            name, _, expected = line.split()
            if digest(engine / name) != expected:
                raise ValueError(f"NDLOCR model checksum mismatch: {name}")
    upstream = prefix / "ndlocr-requirements.txt"
    upstream.write_text("".join(
        line + "\n" for line in (engine / "requirements.txt").read_text().splitlines()
        if not line.startswith("flet")
    ))
    app = prefix / "jaocr"
    app.mkdir()
    shutil.copyfile(root / plan["requirements"], app / "requirements.txt")
    for name in plan["sources"]:
        shutil.copyfile(root / name, app / Path(name).name)
    shutil.copytree(root / "ocr/ja-ocr/wordlists", app / "wordlists")
    subprocess.run([sys.executable, "-m", "venv", str(prefix / "venv")], check=True)
    python = str(prefix / "venv/bin/python")
    subprocess.run([
        python, "-m", "pip", "install", "--no-cache-dir",
        "-r", str(upstream), "-r", str(app / "requirements.txt"),
    ], check=True)
    (prefix / "runtime.env").write_text(
        f"NDLOCR_ROOT={engine}\nNDLOCR_COMMIT={plan['commit']}\n")
    subprocess.run([python, "-m", "pip", "check"], check=True)
    subprocess.run([
        python, "-c",
        "import onnxruntime, cv2, pyarrow, azure.identity, azure.storage.blob, wordfreq",
    ], check=True)
    installed = {
        **plan, "python": sys.version, "prefix": str(prefix),
        "files": {str(path.relative_to(prefix)): digest(path)
                  for path in sorted(app.rglob("*")) if path.is_file()},
    }
    (prefix / "installation.json").write_text(json.dumps(installed, indent=2) + "\n")
    print(json.dumps({"installed": str(prefix), "commit": plan["commit"]}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--prefix", type=Path)
    args = parser.parse_args()
    plan = recipe(args.root)
    if args.check:
        print(json.dumps(plan, indent=2))
    else:
        if not args.prefix.is_absolute():
            parser.error("--prefix must be absolute")
        install(args.root, args.prefix, plan)


if __name__ == "__main__":
    main()
