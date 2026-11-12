#!/usr/bin/env python3
"""Tesseract on every crop of a gold.py manifest, as scorer hypotheses (#968).

    tesseract_crops.py --manifest DIR/manifest.jsonl --images DIR --out NEW_DIR
                       --lang eng --psm 6 --tessdata TESSDATA [--jobs 2] [--timeout 1800]

The flags are the GPU studies' (`tesseract-local.sh`): `-l LANG --psm N --dpi 300`, one OpenMP
thread per process, the default LSTM engine. Writes, into --out (which must not exist):

  outputs.jsonl      one row {"crop_id", "id", "text"} per manifest crop, in manifest order; a crop
                     whose run failed or timed out has text "" (it stays in every denominator)
  tesseract-run.json `tesseract --version`, the language, PSM, traineddata SHA-256 per language,
                     the SHA-256 of the manifest and of outputs.jsonl, and a failed-crop list
"""

from __future__ import annotations

import argparse
import concurrent.futures as cf
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path


def read_jsonl(p: Path) -> list[dict]:
    return [json.loads(x) for x in p.read_text(encoding="utf-8").splitlines() if x.strip()]


def sha256_file(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def read_crop(image: Path, lang: str, psm: int, tessdata: Path, timeout: int) -> tuple[str, str | None]:
    env = {**os.environ, "TESSDATA_PREFIX": str(tessdata), "OMP_THREAD_LIMIT": "1"}
    try:
        out = subprocess.run(["tesseract", str(image), "-", "-l", lang, "--psm", str(psm), "--dpi", "300"],
                             capture_output=True, env=env, timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        return "", "timeout"
    if out.returncode != 0:
        return "", f"exit {out.returncode}"
    return out.stdout.decode("utf-8", errors="replace"), None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--manifest", type=Path, required=True)
    ap.add_argument("--images", type=Path, required=True, help="the directory the manifest's `image` paths are under")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--lang", required=True, help="tesseract -l value, e.g. eng or fra")
    ap.add_argument("--psm", type=int, default=6)
    ap.add_argument("--tessdata", type=Path, required=True)
    ap.add_argument("--jobs", type=int, default=2)
    ap.add_argument("--timeout", type=int, default=1800)
    a = ap.parse_args(argv)
    if a.out.exists():
        print(f"--out must not exist: {a.out}", file=sys.stderr)
        return 2
    rows = read_jsonl(a.manifest)
    version = subprocess.run(["tesseract", "--version"], capture_output=True, text=True, check=True)
    version_text = (version.stdout or version.stderr).strip()
    models = {}
    for lang in a.lang.split("+"):
        models[lang] = sha256_file(a.tessdata / f"{lang}.traineddata")
    with cf.ThreadPoolExecutor(max_workers=a.jobs) as pool:
        results = list(pool.map(
            lambda r: read_crop(a.images / r["image"], a.lang, a.psm, a.tessdata, a.timeout), rows))
    outputs = [{"crop_id": r["crop_id"], "id": r["crop_id"], "text": t} for r, (t, _) in zip(rows, results)]
    failed = [{"crop_id": r["crop_id"], "reason": why} for r, (_, why) in zip(rows, results) if why]
    a.out.mkdir(parents=True)
    out_bytes = "".join(json.dumps(o, ensure_ascii=False) + "\n" for o in outputs).encode("utf-8")
    (a.out / "outputs.jsonl").write_bytes(out_bytes)
    report = {"format": "usnm-tesseract-crops/1", "reader": "tesseract", "tesseract_version": version_text,
              "lang": a.lang, "psm": a.psm, "dpi": 300, "traineddata_sha256": models,
              "manifest_sha256": hashlib.sha256(a.manifest.read_bytes()).hexdigest(),
              "outputs_sha256": hashlib.sha256(out_bytes).hexdigest(),
              "crops": len(rows), "empty_outputs": sum(1 for o in outputs if not o["text"].strip()),
              "failed": failed}
    (a.out / "tesseract-run.json").write_text(json.dumps(report, indent=1, ensure_ascii=False) + "\n",
                                              encoding="utf-8")
    print(json.dumps({"crops": len(rows), "failed": len(failed), "empty": report["empty_outputs"]}))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
