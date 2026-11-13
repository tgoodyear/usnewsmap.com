#!/usr/bin/env python3
"""Runs two usnm_tesseract builds on the same images, on this machine, and fails if any output
byte differs (#1835).

    compare-native-builds.py --old STOCK --new PATCHED --tessdata DIR --images FILE_OR_DIR [...]

DIR holds eng.traineddata (the locked model). The script writes the two output configs the worker
uses (`alto`, `txt`, as crates/usnm-boinc/src/native.rs lays them out) beside it in a temporary
copy, then runs each build as the worker's engine runs it:
`tesseract IMAGE page -l eng --psm 3 --dpi 300 txt alto`, one thread. It prints each build's
sha256 (the `engine.base` of a build) and, per image, whether the text and the ALTO are byte for
byte equal. Exit status 1 if any differs or if either build fails on an image the other reads.

Both builds must run on one machine: the same binary can differ in one word-confidence digit
between CPUs, so a comparison across machines proves nothing.
"""
import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile

SUFFIXES = (".png", ".jpg", ".jpeg", ".tif", ".tiff", ".jp2")
CONFIGS = {
    "alto": "tessedit_create_alto 1\n",
    "txt": "tessedit_create_txt 1\n",
}


def sha256(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def images(paths):
    found = []
    for p in paths:
        if os.path.isdir(p):
            for name in sorted(os.listdir(p)):
                if name.lower().endswith(SUFFIXES):
                    found.append(os.path.join(p, name))
        else:
            found.append(p)
    return found


def run(binary, tessdata, image, work):
    out = os.path.join(work, "page")
    for ext in ("txt", "xml"):
        if os.path.exists(f"{out}.{ext}"):
            os.remove(f"{out}.{ext}")
    env = dict(os.environ, OMP_THREAD_LIMIT="1", OMP_NUM_THREADS="1")
    cmd = [binary, image, out, "--tessdata-dir", tessdata, "-l", "eng", "--psm", "3", "--dpi", "300", "txt", "alto"]
    done = subprocess.run(cmd, env=env, capture_output=True, timeout=600)
    files = {}
    for ext in ("txt", "xml"):
        if os.path.exists(f"{out}.{ext}"):
            with open(f"{out}.{ext}", "rb") as f:
                files[ext] = f.read()
    return done.returncode, files, done.stderr.decode(errors='replace').strip()[-400:]


def compare(old, new, tessdata, image_paths, say=print):
    """Returns the number of images on which the builds differ or either fails."""
    old, new, tessdata = os.path.abspath(old), os.path.abspath(new), os.path.abspath(tessdata)
    old_sha, new_sha = sha256(old), sha256(new)
    say(f"old engine sha256:{old_sha}")
    say(f"new engine sha256:{new_sha}")
    if old_sha == new_sha:
        say("the two builds are the same file: nothing is compared")
        return 1
    imgs = images(image_paths)
    if not imgs:
        say("no images")
        return 1
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        data = os.path.join(tmp, "tessdata")
        os.makedirs(os.path.join(data, "configs"))
        shutil.copy(os.path.join(tessdata, "eng.traineddata"), data)
        for name, text in CONFIGS.items():
            with open(os.path.join(data, "configs", name), "w", newline="\n") as f:
                f.write(text)
        work = os.path.join(tmp, "run")
        os.makedirs(work)
        for image in imgs:
            a = run(old, data, os.path.abspath(image), work)
            b = run(new, data, os.path.abspath(image), work)
            same = a == b and a[0] == 0 and "txt" in a[1] and "xml" in a[1]
            sizes = {k: len(v) for k, v in b[1].items()}
            say(f"{'identical' if same else 'DIFFERENT'} {image} exit {a[0]}/{b[0]} bytes {sizes}")
            if not same:
                say(f"  old stderr: {a[2]!r}")
                say(f"  new stderr: {b[2]!r}")
            bad += not same
    say(f"{len(imgs) - bad} of {len(imgs)} images byte-identical, text and ALTO")
    return bad


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--old", required=True, help="the stock build's usnm_tesseract")
    ap.add_argument("--new", required=True, help="the patched build's usnm_tesseract")
    ap.add_argument("--tessdata", required=True, help="directory with eng.traineddata")
    ap.add_argument("--images", required=True, nargs="+", help="image files or directories of them")
    args = ap.parse_args(argv)
    return 1 if compare(args.old, args.new, args.tessdata, args.images) else 0


if __name__ == "__main__":
    sys.exit(main())
