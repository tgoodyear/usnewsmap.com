#!/usr/bin/env python3
"""Run the pinned EffOCR English checkpoint on an immutable OCR-bench subset."""
from __future__ import annotations

import argparse
import copy
import hashlib
import importlib.metadata
import inspect
import json
import os
import platform
import re
import resource
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any, Callable

MODEL_KEY = "effocr_en"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tree_digest(files: list[dict[str, Any]]) -> str:
    payload = "".join(f"{row['path']} {row['sha256']}\n" for row in sorted(files, key=lambda row: row["path"]))
    return hashlib.sha256(payload.encode()).hexdigest()


def command_output(argv: list[str]) -> str | None:
    try:
        return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=15).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def stage_model_files(model_files: list[dict[str, Any]], model_dir: Path, runtime_model_dir: Path) -> None:
    expected = {"line.onnx", "localizer.onnx"}
    expected.update(
        f"{kind}_recognizer/{filename}"
        for kind in ("word", "char")
        for filename in ("enc_best.onnx", "ref.index", "ref.txt")
    )
    if {item["path"] for item in model_files} != expected:
        raise SystemExit("EffOCR model lock does not contain exactly the expected pinned runtime files")
    destinations = {"line.onnx": "line/line.onnx", "localizer.onnx": "localizer/localizer.onnx"}
    for item in model_files:
        relative = destinations.get(item["path"], item["path"])
        source = model_dir / item["path"]
        target = runtime_model_dir / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.symlink_to(source)


def safe_error_message(error: Exception) -> str:
    message = re.sub(r"(?:/Users|/home)/[^\s:'\"`]+|[A-Za-z]:\\[^\s:'\"`]+", "<local-path>", str(error))
    return message[:500]


def collect_samples(
    samples: list[dict[str, Any]], infer_one: Callable[[dict[str, Any]], str]
) -> tuple[list[dict[str, str]], list[float], list[dict[str, str | float]], float]:
    outputs: list[dict[str, str]] = []
    timings: list[float] = []
    failures: list[dict[str, str | float]] = []
    started_wall = time.monotonic()
    for index, sample in enumerate(samples):
        started = time.monotonic()
        try:
            text = infer_one(sample)
            if not isinstance(text, str):
                raise RuntimeError("EffOCR returned non-text output")
        except Exception as error:
            elapsed = time.monotonic() - started
            failures.append({
                "id": sample["id"],
                "error_type": type(error).__name__,
                "error_message": safe_error_message(error),
                "elapsed_seconds": round(elapsed, 3),
            })
            print(
                f"{index + 1}/{len(samples)} {sample['id']} failed ({type(error).__name__}); preserving completed outputs",
                flush=True,
            )
            break
        elapsed = time.monotonic() - started
        outputs.append({"id": sample["id"], "text": text})
        timings.append(elapsed)
        print(f"{index + 1}/{len(samples)} {sample['id']} {elapsed:.3f}s", flush=True)
    return outputs, timings, failures, time.monotonic() - started_wall


def save_trial_outputs(
    out_dir: Path,
    outputs: list[dict[str, str]],
    receipt: dict[str, Any],
    expected_samples: int,
    failures: list[dict[str, str | float]],
) -> Path:
    output_path = out_dir / "outputs.jsonl"
    output_path.write_text(
        "".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in outputs),
        encoding="utf-8",
    )
    receipt.update({
        "samples": len(outputs),
        "missing_outputs": expected_samples - len(outputs),
        "complete": not failures,
        "sample_failures": failures,
        "output_sha256": sha256(output_path),
    })
    write_json(out_dir / "run.json", receipt)
    if failures:
        raise SystemExit(f"partial run saved for {len(outputs)} completed samples; see run.json for the failed sample")
    return output_path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter-lock", type=Path, required=True)
    parser.add_argument("--release", type=Path, required=True)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--runtime-model-dir", type=Path, required=True)
    parser.add_argument("--reference-vectors-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--threads", type=int, default=4)
    return parser.parse_args()


def verify_source(source_dir: Path) -> tuple[str, Path]:
    root = command_output(["git", "-C", str(source_dir), "rev-parse", "--show-toplevel"])
    revision = command_output(["git", "-C", str(source_dir), "rev-parse", "HEAD"])
    if root != str(source_dir) or revision is None:
        raise SystemExit("EffOCR source must be a Git checkout root")
    if command_output(["git", "-C", str(source_dir), "status", "--porcelain"]) != "":
        raise SystemExit("EffOCR source checkout must be clean")
    return revision, source_dir


def run() -> None:
    args = parse_args()
    if args.threads < 1:
        raise SystemExit("--threads must be positive")
    lock_path = args.adapter_lock.resolve()
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    runtime_lock = Path(__file__).resolve().parents[2] / lock["runtime"]["requirements_lock"]
    if sha256(runtime_lock) != lock["runtime"]["requirements_lock_sha256"]:
        raise SystemExit("EffOCR runtime requirements lock checksum mismatch")
    if lock.get("format") != "usnm-ocr-bench-effocr-adapter-v1" or lock.get("model_used") != MODEL_KEY:
        raise SystemExit("unknown or mismatched EffOCR adapter lock")
    release = args.release.resolve()
    model_dir = args.model_dir.resolve()
    source_dir = args.source_dir.resolve()
    runtime_model_dir = args.runtime_model_dir.resolve()
    reference_vectors_dir = args.reference_vectors_dir.resolve()
    out_dir = args.out_dir.resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"refusing nonempty output directory: {out_dir}")
    if runtime_model_dir.exists() and any(runtime_model_dir.iterdir()):
        raise SystemExit(f"refusing nonempty runtime model directory: {runtime_model_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)
    runtime_model_dir.mkdir(parents=True, exist_ok=True)

    manifest_path = release / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("release") != lock["release"]:
        raise SystemExit("benchmark release differs from adapter lock")
    if sha256(release / "SHA256SUMS") != lock["release_content_digest"]:
        raise SystemExit("benchmark release content digest differs from adapter lock")
    subset = manifest.get("subsets", {}).get(lock["subset"])
    if subset is None or subset.get("digest") != lock["subset_digest"]:
        raise SystemExit("benchmark subset differs from adapter lock")
    samples = {row["id"]: row for row in manifest["samples"]}
    locked = lock["samples"]
    if len(locked) != subset["count"] or [row["id"] for row in locked] != subset["members"]:
        raise SystemExit("adapter lock membership/order differs from the immutable subset")
    subset_lines = "".join(
        f"{sample_id} {samples[sample_id]['image']['sha256']} {samples[sample_id]['reference']['sha256']}\n"
        for sample_id in sorted(subset["members"])
    )
    if hashlib.sha256(subset_lines.encode()).hexdigest() != subset["digest"]:
        raise SystemExit("benchmark manifest subset digest is invalid")
    for expected in locked:
        actual = samples[expected["id"]]
        for key in ("path", "sha256"):
            if expected["image" if key == "path" else "image_sha256"] != actual["image"][key]:
                raise SystemExit(f"locked image metadata mismatch: {expected['id']}")
            if expected["reference" if key == "path" else "reference_sha256"] != actual["reference"][key]:
                raise SystemExit(f"locked reference metadata mismatch: {expected['id']}")
        if [expected["image_width"], expected["image_height"]] != [actual["image_width"], actual["image_height"]]:
            raise SystemExit(f"locked image dimensions mismatch: {expected['id']}")
        for relative, field in ((expected["image"], "image_sha256"), (expected["reference"], "reference_sha256")):
            if sha256(release / relative) != expected[field]:
                raise SystemExit(f"benchmark input checksum mismatch: {expected['id']} {relative}")

    repo = lock["model_repositories"][MODEL_KEY]
    model_files = repo["files"]
    for item in model_files:
        path = model_dir / item["path"]
        if not path.is_file() or path.stat().st_size != item["bytes"] or sha256(path) != item["sha256"]:
            raise SystemExit(f"EffOCR checkpoint file mismatch: {item['path']}")
    if tree_digest(model_files) != repo["file_tree_sha256"]:
        raise SystemExit("EffOCR checkpoint tree digest differs from adapter lock")
    if platform.python_version() != lock["runtime"]["python"]:
        raise SystemExit(f"Python version mismatch: expected {lock['runtime']['python']}, got {platform.python_version()}")
    for package, expected in lock["runtime"]["packages"].items():
        actual = importlib.metadata.version(package)
        if actual != expected:
            raise SystemExit(f"runtime package mismatch for {package}: expected {expected}, got {actual}")
    source_revision, source_root = verify_source(source_dir)
    if source_revision != lock["effocr_source"]["revision"]:
        raise SystemExit("EffOCR source revision differs from adapter lock")
    license_path = source_dir / "LICENSE"
    if sha256(license_path) != lock["effocr_source"]["license_sha256"]:
        raise SystemExit("EffOCR source license file hash differs from adapter lock")

    stage_model_files(model_files, model_dir, runtime_model_dir)
    line_dir = runtime_model_dir / "line"
    localizer_dir = runtime_model_dir / "localizer"

    os.environ["CUDA_VISIBLE_DEVICES"] = ""
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["WANDB_MODE"] = "offline"
    os.environ["OMP_NUM_THREADS"] = str(args.threads)
    os.environ["MKL_NUM_THREADS"] = str(args.threads)
    os.environ["OPENBLAS_NUM_THREADS"] = str(args.threads)
    os.environ["VECLIB_MAXIMUM_THREADS"] = str(args.threads)
    import onnxruntime as ort

    available_providers = ort.get_available_providers()
    if "CPUExecutionProvider" not in available_providers:
        raise SystemExit("ONNX Runtime lacks CPUExecutionProvider")
    ort.get_available_providers = lambda: ["CPUExecutionProvider"]
    import torch
    from efficient_ocr.model.effocr import EffOCR
    from efficient_ocr.utils.default_config import DEFAULT_CONFIG
    from efficient_ocr.recognition import recognizermodel as recognition_module
    import numpy as np

    vector_manifest = json.loads((reference_vectors_dir / "manifest.json").read_text(encoding="utf-8"))
    vector_rows = {row["kind"]: row for row in vector_manifest}
    reference_vectors = {}
    for kind in ("word", "char"):
        vector_path = reference_vectors_dir / f"{kind}.npy"
        row = vector_rows.get(kind)
        expected_index = next(
            item for item in model_files if item["path"] == f"{kind}_recognizer/ref.index"
        )
        if (
            row is None
            or row["source_index_sha256"] != expected_index["sha256"]
            or row["sha256"] != sha256(vector_path)
        ):
            raise SystemExit(f"{kind} FAISS vector extraction does not match the pinned checkpoint")
        vectors = np.load(vector_path, mmap_mode="r", allow_pickle=False)
        if vectors.dtype != np.float32 or vectors.ndim != 2 or list(vectors.shape) != row["shape"]:
            raise SystemExit(f"invalid {kind} FAISS reference-vector array")
        reference_vectors[kind] = vectors

    def faiss_cpu_run(recognizer: Any, images: list[Any], cutoff: float | None = None) -> list[str | None]:
        total_images = len(images)
        embeddings = recognition_module.get_crop_embeddings(
            recognition_module.RecognizerEngine(
                recognizer.model,
                recognizer.config["Recognizer"][recognizer.type]["model_backend"],
                recognizer.transform,
                recognizer.input_name,
            ),
            images,
        )
        if not embeddings:
            return []
        vectors = torch.cat(embeddings, dim=0).detach().cpu().contiguous().numpy().astype(np.float32, copy=False)
        if vectors.ndim != 2 or vectors.shape[1] != recognizer.index.d:
            raise RuntimeError("EffOCR embedding shape does not match its pinned FAISS index")
        references = reference_vectors[recognizer.type]
        if vectors.shape[1] != references.shape[1] or not np.isfinite(vectors).all():
            raise RuntimeError("EffOCR query vectors differ from the pinned FAISS reference-vector shape")
        distances = np.empty((len(vectors), 1), dtype=np.float32)
        indices = np.empty((len(vectors), 1), dtype=np.int64)
        for start in range(0, len(vectors), 64):
            chunk = np.ascontiguousarray(vectors[start : start + 64], dtype=np.float32)
            scores = np.matmul(chunk, references.T)
            best = scores.argmax(axis=1)
            indices[start : start + len(chunk), 0] = best
            distances[start : start + len(chunk), 0] = scores[np.arange(len(chunk)), best]
        if cutoff:
            return [
                recognizer.candidates[int(index[0])] if distance[0] > cutoff else None
                for distance, index in zip(distances[:total_images], indices[:total_images], strict=True)
            ]
        return [recognizer.candidates[int(index[0])] for index in indices[:total_images]]

    recognition_module.Recognizer.run = faiss_cpu_run
    torch.set_num_threads(args.threads)
    config = copy.deepcopy(DEFAULT_CONFIG)
    config["Global"]["skip_line_detection"] = False
    config["Global"]["char_only"] = False
    for section, model_path in (("Line", line_dir), ("Localizer", localizer_dir)):
        config[section]["model_backend"] = "onnx"
        config[section]["model_dir"] = str(model_path)
        config[section]["hf_repo_id"] = None
        config[section]["providers"] = None
        config[section]["num_cores"] = args.threads
    for kind in ("word", "char"):
        config["Recognizer"][kind]["model_backend"] = "onnx"
        config["Recognizer"][kind]["model_dir"] = str(runtime_model_dir / f"{kind}_recognizer")
        config["Recognizer"][kind]["hf_repo_id"] = None
        config["Recognizer"][kind]["providers"] = None
        config["Recognizer"][kind]["num_cores"] = args.threads
    init_started = time.monotonic()
    engine = EffOCR(config=config)
    initialization_seconds = time.monotonic() - init_started
    for kind, vectors in reference_vectors.items():
        recognizer = getattr(engine, f"{kind}_model")
        if vectors.shape != (recognizer.index.ntotal, recognizer.index.d) or len(recognizer.candidates) != vectors.shape[0]:
            raise SystemExit(f"{kind} FAISS vectors, index, and candidate labels differ")
    imported_file = Path(inspect.getfile(EffOCR)).resolve()
    try:
        imported_relative = imported_file.relative_to(source_root).as_posix()
    except ValueError as error:
        raise SystemExit("EffOCR was not imported from its pinned source checkout") from error
    tracked_path = command_output(["git", "-C", str(source_dir), "ls-files", "--error-unmatch", "--", imported_relative])
    if tracked_path != imported_relative:
        raise SystemExit("EffOCR runtime import is not a tracked file in the pinned checkout")

    def infer_sample(sample: dict[str, Any]) -> str:
        image_path = release / sample["image"]
        pages = engine.infer(str(image_path))
        if len(pages) > 1:
            raise RuntimeError(f"EffOCR returned multiple outputs for one input: {sample['id']}")
        return pages[0].text if pages and pages[0] is not None else ""

    outputs, timings, sample_failures, wall_seconds = collect_samples(locked, infer_sample)

    rss_kib = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if platform.system() == "Darwin":
        rss_kib //= 1024
    package_version = importlib.metadata.version("efficient-ocr")
    receipt = {
        "format": "usnm-ocr-bench-effocr-trial-v1",
        "release": lock["release"],
        "release_content_digest": lock["release_content_digest"],
        "subset": lock["subset"],
        "subset_digest": lock["subset_digest"],
        "adapter_lock_sha256": sha256(lock_path),
        "model_repo": repo["repo"],
        "model_revision": repo["revision"],
        "model_license_tag": repo["license_tag"],
        "model_card_license": repo["model_card_license"],
        "model_tree_sha256": repo["file_tree_sha256"],
        "model_files": model_files,
        "model_card_gap": lock["rights"]["checkpoint_license_tag"],
        "license_basis": lock["rights"]["checkpoint_license_basis"],
        "effocr_source_repo": lock["effocr_source"]["repo"],
        "effocr_source_revision": source_revision,
        "effocr_source_license": lock["effocr_source"]["license"],
        "effocr_source_license_sha256": lock["effocr_source"]["license_sha256"],
        "effocr_package_version": package_version,
        "python": platform.python_version(),
        "torch": torch.__version__,
        "onnxruntime": importlib.metadata.version("onnxruntime"),
        "numpy": importlib.metadata.version("numpy"),
        "faiss_cpu": importlib.metadata.version("faiss-cpu"),
        "runtime_packages": {
            name: importlib.metadata.version(name)
            for name in (
                "efficient-ocr", "onnx", "onnxruntime", "opencv-python", "Pillow",
                "pytorch-metric-learning", "setuptools", "timm", "torch", "torchvision", "yolov5",
            )
        },
        "platform": platform.platform(),
        "machine": platform.machine(),
        "backend": lock["inference"]["backend"],
        "device": "CPU",
        "onnx_runtime_available_providers": available_providers,
        "onnx_runtime_used_providers": ["CPUExecutionProvider"],
        "threads": args.threads,
        "faiss_query_backend": "numpy float32 inner product against vectors reconstructed from pinned IndexFlatIP files; exact metric/index data",
        "faiss_reference_vectors": vector_manifest,
        "faiss_vector_manifest_sha256": sha256(reference_vectors_dir / "manifest.json"),
        "word_similarity_threshold": lock["inference"]["word_similarity_threshold"],
        "faiss_compatibility_adapter": lock["inference"]["faiss_compatibility_adapter"],
        "post_correction": "none",
        "model_initialization_seconds": round(initialization_seconds, 3),
        "cold_first_image_seconds": round(timings[0], 3) if timings else None,
        "warm_image_seconds": [round(value, 3) for value in timings[1:]],
        "warm_p50_seconds": round(statistics.median(timings[1:]), 3) if len(timings) > 1 else None,
        "warm_p95_seconds": round(sorted(timings[1:])[int(len(timings[1:]) * 0.95)], 3) if len(timings) > 1 else None,
        "ocr_loop_wall_seconds": round(wall_seconds, 3),
        "blank_outputs": sum(not row["text"].strip() for row in outputs),
        "process_peak_rss_kib": rss_kib,
        "resource_note": "Isolated local process without a cgroup cap; process peak RSS from getrusage.",
        "benchmark_source_licenses": "As recorded in the verified v1.0.0 manifest.",
        "output_redistribution_note": lock["rights"]["output_redistribution"],
    }
    output_path = save_trial_outputs(out_dir, outputs, receipt, len(locked), sample_failures)
    print(f"completed {len(outputs)} samples in {wall_seconds:.3f}s; outputs={output_path}", flush=True)


if __name__ == "__main__":
    run()
