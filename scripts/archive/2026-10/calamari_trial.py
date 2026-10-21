#!/usr/bin/env python3
"""Run a pinned Calamari line recognizer on explicitly segmented benchmark units."""

from __future__ import annotations

import argparse
import hashlib
from importlib.metadata import version as package_version
import json
import os
import platform
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from PIL import Image


EXPECTED_MODEL_TREE_SHA256 = "c8d3b6e7819f8300e308016bf4e959fdaefd71836ee3b9d7d2f164ca9412d628"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", type=Path, required=True, help="Verified release directory")
    parser.add_argument("--subset", required=True)
    parser.add_argument("--segments", type=Path, required=True, help="Pinned image hashes and cut rows")
    parser.add_argument("--checkpoint-dir", type=Path, required=True, help="fraktur_historical model directory")
    parser.add_argument("--out-dir", type=Path, required=True, help="New output directory")
    parser.add_argument("--calamari-predict", default="calamari-predict")
    parser.add_argument("--nlbin", required=True, help="Pinned OCRopus nlbin CLI")
    parser.add_argument("--timeout-seconds", type=int, default=600)
    return parser.parse_args()


def run() -> None:
    args = parse_args()
    release = args.release.resolve()
    checkpoint_dir = args.checkpoint_dir.resolve()
    segments_path = args.segments.resolve()
    out_dir = args.out_dir.resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"refusing nonempty output directory: {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)

    manifest = json.loads((release / "manifest.json").read_text(encoding="utf-8"))
    subset = manifest["subsets"].get(args.subset)
    if subset is None:
        raise SystemExit(f"unknown subset: {args.subset}")
    segment_lock = json.loads(segments_path.read_text(encoding="utf-8"))
    if segment_lock.get("release") != manifest["release"] or segment_lock.get("subset") != args.subset:
        raise SystemExit("segmentation lock does not match the benchmark release and subset")
    locks = {row["id"]: row for row in segment_lock["samples"]}
    member_ids = subset["members"]
    if len(locks) != len(segment_lock["samples"]) or set(locks) != set(member_ids):
        raise SystemExit("segmentation lock must contain exactly the immutable subset members")
    samples = {sample["id"]: sample for sample in manifest["samples"]}
    subset_lines = "".join(
        f"{sample_id} {samples[sample_id]['image']['sha256']} {samples[sample_id]['reference']['sha256']}\n"
        for sample_id in sorted(member_ids)
    )
    if hashlib.sha256(subset_lines.encode()).hexdigest() != subset["digest"]:
        raise SystemExit("manifest subset digest does not match its image/reference locks")

    line_dir = out_dir / "line-inputs"
    binary_dir = out_dir / "nlbin-binary"
    prediction_dir = out_dir / "predictions"
    line_dir.mkdir()
    binary_dir.mkdir()
    prediction_dir.mkdir()
    segmentation_started = time.monotonic()
    line_rows: list[dict[str, Any]] = []
    model_files: list[dict[str, str]] = []
    for path in sorted(checkpoint_dir.rglob("*")):
        if path.is_file():
            model_files.append({"path": path.relative_to(checkpoint_dir).as_posix(), "sha256": sha256(path)})
    if not model_files or any(not (checkpoint_dir / f"{voter}.ckpt.json").is_file() for voter in range(5)):
        raise SystemExit("expected the five-voter fraktur_historical checkpoint")
    model_tree = "".join(f"{row['path']} {row['sha256']}\n" for row in model_files).encode()
    model_tree_sha256 = hashlib.sha256(model_tree).hexdigest()
    if model_tree_sha256 != EXPECTED_MODEL_TREE_SHA256:
        raise SystemExit(
            f"model tree digest mismatch: expected {EXPECTED_MODEL_TREE_SHA256}, got {model_tree_sha256}"
        )

    nlbin_executable = shutil.which(args.nlbin)
    if nlbin_executable is None:
        raise SystemExit(f"nlbin executable not found: {args.nlbin}")
    nlbin_path = Path(nlbin_executable).resolve()
    nlbin_executable_sha256 = sha256(nlbin_path)

    for sample_id in member_ids:
        sample = samples[sample_id]
        image_path = release / sample["image"]["path"]
        image_hash = sha256(image_path)
        lock = locks[sample_id]
        if image_hash != sample["image"]["sha256"] or image_hash != lock["image_sha256"]:
            raise SystemExit(f"input image checksum mismatch: {sample_id}")
        reference_path = release / sample["reference"]["path"]
        if sha256(reference_path) != sample["reference"]["sha256"]:
            raise SystemExit(f"reference checksum mismatch: {sample_id}")
        lock["reference_sha256"] = sample["reference"]["sha256"]
        with Image.open(image_path) as source:
            image = source.convert("L")
        width, height = image.size
        if [width, height] != lock["image_size"]:
            raise SystemExit(f"input image dimensions changed: {sample_id}")
        cuts = lock["cuts_y"]
        if len(cuts) < 1 or cuts != sorted(set(cuts)) or any(cut < 1 or cut >= height for cut in cuts):
            raise SystemExit(f"invalid segmentation cuts: {sample_id}")
        bounds = [0, *cuts, height]
        line_files: list[Path] = []
        for index, (top, bottom) in enumerate(zip(bounds, bounds[1:])):
            if bottom - top < 12:
                raise SystemExit(f"segmentation produced a short line: {sample_id}")
            line_path = line_dir / f"{sample_id}-line-{index:02d}.png"
            image.crop((0, top, width, bottom)).save(line_path, format="PNG")
            line_files.append(line_path)
            line_rows.append(
                {
                    "sample_id": sample_id,
                    "line_index": index,
                    "y_start": top,
                    "y_end_exclusive": bottom,
                    "image_sha256": sha256(line_path),
                    "image": line_path.name,
                }
            )
        lock["image_width"] = width
        lock["image_height"] = height
        lock["line_count"] = len(line_files)
    segmentation_wall_seconds = time.monotonic() - segmentation_started

    environment = os.environ.copy()
    environment.update(
        {
            "CUDA_VISIBLE_DEVICES": "",
            "OMP_NUM_THREADS": "2",
            "TF_CPP_MIN_LOG_LEVEL": "2",
            "TF_NUM_INTEROP_THREADS": "1",
            "TF_NUM_INTRAOP_THREADS": "2",
        }
    )
    preprocessing_started = time.monotonic()
    try:
        preprocessed = subprocess.run(
            [
                args.nlbin,
                "--device",
                "cpu",
                "--bin",
                "--output",
                str(binary_dir),
                *(str(line_dir / row["image"]) for row in line_rows),
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=args.timeout_seconds,
            env=environment,
        )
        (out_dir / "nlbin.log").write_text(preprocessed.stdout, encoding="utf-8")
    except subprocess.TimeoutExpired as error:
        output = error.stdout or ""
        if isinstance(output, bytes):
            output = output.decode("utf-8", errors="replace")
        (out_dir / "nlbin.log").write_text(output, encoding="utf-8")
        raise SystemExit(f"nlbin timed out after {args.timeout_seconds}s") from error
    preprocessing_wall_seconds = time.monotonic() - preprocessing_started
    if preprocessed.returncode != 0:
        raise SystemExit(f"nlbin failed with exit code {preprocessed.returncode}; see {out_dir / 'nlbin.log'}")
    for row in line_rows:
        source_name = Path(row["image"])
        binary_path = binary_dir / f"{source_name.stem}.ocropus.bin.png"
        if not binary_path.is_file():
            raise SystemExit(f"nlbin output missing for {source_name}")
        row["preprocessed_image"] = binary_path.name
        row["preprocessed_image_sha256"] = sha256(binary_path)

    checkpoint_paths = [checkpoint_dir / f"{voter}.ckpt" for voter in range(5)]
    command = [
        args.calamari_predict,
        "--checkpoint",
        *(str(path) for path in checkpoint_paths),
        "--data.images",
        *(str(binary_dir / row["preprocessed_image"]) for row in line_rows),
        "--output_dir",
        str(prediction_dir),
        "--data.pred_extension",
        ".pred.txt",
        "--data.preload",
        "false",
        "--pipeline.batch_size",
        "1",
        "--pipeline.num_processes",
        "1",
        "--pipeline.prefetch",
        "1",
        "--predictor.progress_bar",
        "false",
        "--verbose",
        "false",
    ]
    started = time.monotonic()
    try:
        completed = subprocess.run(
            command,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=args.timeout_seconds,
            env=environment,
        )
        (out_dir / "calamari.log").write_text(completed.stdout, encoding="utf-8")
    except subprocess.TimeoutExpired as error:
        output = error.stdout or ""
        if isinstance(output, bytes):
            output = output.decode("utf-8", errors="replace")
        (out_dir / "calamari.log").write_text(output, encoding="utf-8")
        raise SystemExit(f"Calamari timed out after {args.timeout_seconds}s") from error
    wall_seconds = time.monotonic() - started
    if completed.returncode != 0:
        raise SystemExit(
            f"Calamari failed with exit code {completed.returncode}; see {out_dir / 'calamari.log'}"
        )

    line_texts: dict[tuple[str, int], str] = {}
    for row in line_rows:
        basename = Path(row["image"]).name
        prediction = prediction_dir / f"{basename}.pred.txt"
        if not prediction.is_file():
            alternatives = sorted(prediction_dir.glob(f"{Path(basename).stem}*.pred.txt"))
            if len(alternatives) != 1:
                raise SystemExit(f"missing or ambiguous Calamari output for {basename}")
            prediction = alternatives[0]
        line_texts[(row["sample_id"], row["line_index"])] = prediction.read_text(
            encoding="utf-8"
        ).rstrip("\r\n")

    outputs = []
    for sample_id in member_ids:
        count = locks[sample_id]["line_count"]
        outputs.append(
            {
                "id": sample_id,
                "text": "\n".join(line_texts[(sample_id, index)] for index in range(count)),
            }
        )
    output_path = out_dir / "outputs.jsonl"
    output_path.write_text(
        "".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in outputs),
        encoding="utf-8",
    )
    write_json(
        out_dir / "run.json",
        {
            "format": "usnm-ocr-bench-calamari-trial-v1",
            "release": manifest["release"],
            "release_content_digest": hashlib.sha256((release / "SHA256SUMS").read_bytes()).hexdigest(),
            "subset": args.subset,
            "subset_digest": subset["digest"],
            "segmentation_lock_sha256": sha256(segments_path),
            "model": "Calamari-OCR/calamari_models/fraktur_historical",
            "model_commit": "308697f2fe2f0143d687107e6157688ea4116157",
            "model_license": "MIT",
            "model_training_data": "GT4HistOCR",
            "model_training_data_license": "CC BY 4.0",
            "engine_license": "GPL-3.0",
            "model_tree_sha256": model_tree_sha256,
            "model_files": model_files,
            "benchmark_source_licenses": manifest["sources"][samples[member_ids[0]]["source"]],
            "preprocessor": "jahtz/nlbin",
            "preprocessor_version": "1.0.0",
            "preprocessor_commit": "940773e0810bb166155c0d5cc47a8027a25d9531",
            "preprocessor_license": "Apache-2.0",
            "preprocessor_provenance_status": "upstream installation identity unverified; executable hash recorded only",
            "preprocessor_executable_name": nlbin_path.name,
            "preprocessor_executable_sha256": nlbin_executable_sha256,
            "preprocessing_wall_seconds": round(preprocessing_wall_seconds, 3),
            "segmentation_wall_seconds": round(segmentation_wall_seconds, 3),
            "calamari_version": package_version("calamari-ocr"),
            "python": sys.version.split()[0],
            "platform": platform.platform(),
            "inference_device": "CPU (CUDA_VISIBLE_DEVICES empty)",
            "inference_wall_seconds": round(wall_seconds, 3),
            "end_to_end_wall_seconds": round(segmentation_wall_seconds + preprocessing_wall_seconds + wall_seconds, 3),
            "inference_line_count": len(line_rows),
            "sample_count": len(outputs),
            "missing_outputs": 0,
            "blank_outputs": sum(not row["text"].strip() for row in outputs),
            "output_sha256": sha256(output_path),
            "segments": [*locks.values()],
            "line_inputs": line_rows,
        },
    )
    print(f"inferred {len(line_rows)} lines from {len(outputs)} samples in {wall_seconds:.3f}s")
    print(f"outputs: {output_path} sha256={sha256(output_path)}")


if __name__ == "__main__":
    run()
