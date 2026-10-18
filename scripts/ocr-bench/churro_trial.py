#!/usr/bin/env python3
"""Run the pinned CHURRO 3B checkpoint on an immutable OCR-bench subset."""
from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import inspect
import json
import math
import os
import platform
import resource
import statistics
import subprocess
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any

MODEL_ID = "stanford-oval/churro-3B"
MODEL_REVISION = "ca2150ea465d5a3d67818c50e234b9422619c75d"
CODE_REVISION = "23c3ac65781443b18d87c8cdac5f77445c3319e6"
BLOCK_ELEMENTS = {"body", "blockquotation", "column", "dateline", "document", "footer", "heading", "line", "page", "paragraph", "p", "row", "section"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def model_files(root: Path) -> list[dict[str, Any]]:
    result = []
    for path in sorted(root.rglob("*")):
        if path.is_file() and not any(part.startswith(".") for part in path.relative_to(root).parts):
            result.append({"path": path.relative_to(root).as_posix(), "bytes": path.stat().st_size, "sha256": sha256(path)})
    return result


def tree_digest(files: list[dict[str, Any]]) -> str:
    payload = "".join(f"{row['path']} {row['sha256']}\n" for row in files).encode()
    return hashlib.sha256(payload).hexdigest()


def score_text(raw: str) -> tuple[str, str]:
    text = raw.strip()
    if text.startswith("```xml") and text.endswith("```"):
        text = text[6:-3].strip()
    elif text.startswith("```") and text.endswith("```"):
        text = text[3:-3].strip()
    try:
        root = ET.fromstring(text)
    except ET.ParseError:
        return raw.strip(), "unparsed-raw-fallback"
    chunks: list[str] = []

    def append_text(value: str | None, *, preserve_spaces: bool = False) -> None:
        preserve_whitespace = preserve_spaces and value and value.isspace() and "\n" not in value and "\r" not in value
        if value and (value.strip() or preserve_whitespace):
            chunks.append(value)

    def append_newline() -> None:
        if chunks and chunks[-1] != "\n":
            chunks.append("\n")

    def walk(node: ET.Element, *, inside_line: bool = False) -> None:
        local_name = node.tag.rsplit("}", 1)[-1].lower()
        if local_name == "metadata":
            return
        inside_line = inside_line or local_name == "line"
        append_text(node.text, preserve_spaces=inside_line)
        for child in node:
            child_name = child.tag.rsplit("}", 1)[-1].lower()
            if child_name != "metadata":
                walk(child, inside_line=inside_line)
                if child_name in BLOCK_ELEMENTS:
                    append_newline()
            append_text(child.tail, preserve_spaces=inside_line)

    walk(root)
    return "".join(chunks).strip(), "xml-page-text-metadata-excluded-lines-v2"


def command_output(argv: list[str]) -> str | None:
    try:
        return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=15).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter-lock", type=Path, required=True)
    parser.add_argument("--input-manifest", type=Path, required=True)
    parser.add_argument("--image-dir", type=Path, required=True)
    parser.add_argument("--model-cache", type=Path, required=True)
    parser.add_argument("--churro-source-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    return parser.parse_args()


def run() -> None:
    args = parse_args()
    lock = json.loads(args.adapter_lock.read_text(encoding="utf-8"))
    input_manifest = json.loads(args.input_manifest.read_text(encoding="utf-8"))
    out_dir = args.out_dir.resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"refusing nonempty output directory: {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)
    if lock["format"] != "usnm-ocr-bench-churro-adapter-v1":
        raise SystemExit("unknown CHURRO adapter lock format")
    if lock["model_repo"] != MODEL_ID or lock["model_revision"] != MODEL_REVISION:
        raise SystemExit("adapter lock model identity differs from this pinned runner")
    if lock["code_revision"] != CODE_REVISION:
        raise SystemExit("adapter lock CHURRO code revision differs from this runner")
    if input_manifest["format"] != "usnm-ocr-bench-churro-inputs-v1":
        raise SystemExit("unknown input manifest format")
    locked_samples = lock["samples"]
    supplied = input_manifest["samples"]
    if [row["id"] for row in supplied] != [row["id"] for row in locked_samples]:
        raise SystemExit("input order/membership differs from the immutable adapter lock")
    for expected, actual in zip(locked_samples, supplied, strict=True):
        for field in ("id", "image_sha256", "image_width", "image_height", "reference_sha256"):
            if expected[field] != actual[field]:
                raise SystemExit(f"input manifest lock mismatch for {expected['id']} field {field}")
        image_path = args.image_dir / actual["image"]
        if sha256(image_path) != expected["image_sha256"]:
            raise SystemExit(f"input image checksum mismatch: {expected['id']}")

    os.environ.setdefault("HF_HOME", str(args.model_cache.resolve()))
    os.environ.setdefault("HF_HUB_CACHE", str(args.model_cache.resolve() / "hub"))
    os.environ.setdefault("TMPDIR", str(out_dir / "tmp"))
    Path(os.environ["TMPDIR"]).mkdir(parents=True, exist_ok=True)
    from huggingface_hub import snapshot_download

    cache_dir = args.model_cache.resolve() / "hub"
    snapshot = Path(
        snapshot_download(
            repo_id=MODEL_ID,
            revision=MODEL_REVISION,
            cache_dir=cache_dir,
            local_files_only=False,
        )
    )
    files = model_files(snapshot)
    digest = tree_digest(files)
    if not lock.get("model_tree_sha256") or digest != lock["model_tree_sha256"]:
        write_json(out_dir / "model-lock-candidate.json", {"model_tree_sha256": digest, "model_files": files})
        raise SystemExit(f"model tree digest mismatch: lock={lock.get('model_tree_sha256')} actual={digest}")

    import torch
    from churro_ocr.ocr import OCRClient
    from churro_ocr.providers import HuggingFaceOptions, OCRBackendSpec, build_ocr_backend
    from churro_ocr.providers import hf as hf_module

    source_dir = args.churro_source_dir.resolve()
    runtime_source_root = command_output(["git", "-C", str(source_dir), "rev-parse", "--show-toplevel"])
    if runtime_source_root != str(source_dir):
        raise SystemExit(f"CHURRO source directory is not the Git checkout root: {source_dir}")
    runtime_code_revision = command_output(["git", "-C", str(source_dir), "rev-parse", "HEAD"])
    if runtime_code_revision != CODE_REVISION:
        raise SystemExit(
            f"CHURRO source checkout revision mismatch: expected {CODE_REVISION}, got {runtime_code_revision}"
        )
    if command_output(["git", "-C", str(source_dir), "status", "--porcelain"]) != "":
        raise SystemExit("CHURRO source checkout is dirty; use the exact clean pinned revision")
    try:
        Path(inspect.getfile(OCRClient)).resolve().relative_to(source_dir)
    except ValueError as error:
        raise SystemExit("CHURRO runtime was not imported from the pinned source checkout; install it editable") from error
    churro_package_version = importlib.metadata.version("churro-ocr")
    if churro_package_version != lock["runtime_version"]:
        raise SystemExit(
            f"CHURRO package version mismatch: lock={lock['runtime_version']} installed={churro_package_version}"
        )

    max_new_tokens = int(lock["max_new_tokens"])
    options = HuggingFaceOptions(
        processor_kwargs={"revision": MODEL_REVISION, "cache_dir": str(cache_dir)},
        model_kwargs={
            "revision": MODEL_REVISION,
            "cache_dir": str(cache_dir),
            "device_map": "auto",
            "dtype": torch.float16,
            "max_memory": {0: "6GiB", "cpu": "9GiB"},
        },
        generation_kwargs={"max_new_tokens": max_new_tokens, "do_sample": False},
    )
    backend = build_ocr_backend(OCRBackendSpec(provider="hf", model=MODEL_ID, options=options))
    client = OCRClient(backend)

    runtime_started = time.monotonic()
    runtime = backend._load_runtime()
    runtime_import_seconds = time.monotonic() - runtime_started
    processor_started = time.monotonic()
    backend._get_processor(runtime)
    processor_load_seconds = time.monotonic() - processor_started
    gpu_processes_before_trial = command_output(
        ["nvidia-smi", "--query-compute-apps=pid,process_name,used_memory", "--format=csv,noheader"]
    )
    model_started = time.monotonic()
    model = backend._get_model(runtime)
    if hasattr(model, "eval"):
        model.eval()
    model_load_seconds = time.monotonic() - model_started
    if torch.cuda.is_available():
        torch.cuda.synchronize()
        torch.cuda.reset_peak_memory_stats()

    generations: list[dict[str, int | None]] = []
    generate = hf_module._generate_with_model

    def measured_generate(model_obj: object, **kwargs: object) -> object:
        output = generate(model_obj, **kwargs)
        input_ids = kwargs.get("input_ids")
        input_count = int(input_ids.shape[-1]) if hasattr(input_ids, "shape") else None
        output_count = int(output.shape[-1]) if hasattr(output, "shape") else None
        generated_count = output_count - input_count if output_count is not None and input_count is not None else None
        generations.append({"input_tokens": input_count, "output_tokens_total": output_count, "generated_tokens": generated_count})
        return output

    hf_module._generate_with_model = measured_generate
    raw_rows = []
    scored_rows = []
    timings = []
    parse_statuses = []
    wall_started = time.monotonic()
    try:
        for index, sample in enumerate(supplied):
            started = time.monotonic()
            page = client.ocr_image(image_path=args.image_dir / sample["image"], page_index=index)
            if torch.cuda.is_available():
                torch.cuda.synchronize()
            elapsed = time.monotonic() - started
            text = page.text or ""
            scored_text, parse_status = score_text(text)
            timings.append(elapsed)
            parse_statuses.append(parse_status)
            raw_rows.append({"id": sample["id"], "text": text})
            scored_rows.append({"id": sample["id"], "text": scored_text})
            print(f"{index + 1}/{len(supplied)} {sample['id']} {elapsed:.3f}s generated_tokens={generations[-1]['generated_tokens']}", flush=True)
    finally:
        hf_module._generate_with_model = generate
    wall_seconds = time.monotonic() - wall_started

    raw_path = out_dir / "raw-outputs.jsonl"
    scored_path = out_dir / "outputs.jsonl"
    raw_path.write_text("".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in raw_rows), encoding="utf-8")
    scored_path.write_text("".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in scored_rows), encoding="utf-8")
    if torch.cuda.is_available():
        torch.cuda.synchronize()
        peak_gpu_allocated = torch.cuda.max_memory_allocated()
        peak_gpu_reserved = torch.cuda.max_memory_reserved()
    else:
        peak_gpu_allocated = peak_gpu_reserved = 0
    device_map = getattr(model, "hf_device_map", None)
    rss_kib = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if platform.system() == "Darwin":
        rss_kib //= 1024
    receipt = {
        "format": "usnm-ocr-bench-churro-trial-v1",
        "release": lock["release"],
        "release_content_digest": lock["release_content_digest"],
        "subset": lock["subset"],
        "subset_digest": lock["subset_digest"],
        "adapter_lock_sha256": sha256(args.adapter_lock),
        "model_repo": MODEL_ID,
        "model_revision": MODEL_REVISION,
        "model_license": lock["model_license"],
        "model_tree_sha256": digest,
        "model_files": files,
        "code_repo": "stanford-oval/Churro",
        "code_revision": runtime_code_revision,
        "runtime_code_revision_verified": True,
        "runtime_source_repo_root_verified": True,
        "runtime_import_path_verified": True,
        "runtime_source_tree_clean": True,
        "churro_package_version": churro_package_version,
        "python": platform.python_version(),
        "torch": torch.__version__,
        "torch_cuda_runtime": torch.version.cuda,
        "transformers": importlib.metadata.version("transformers"),
        "accelerate": importlib.metadata.version("accelerate"),
        "qwen_vl_utils": importlib.metadata.version("qwen-vl-utils"),
        "platform": platform.platform(),
        "gpu": command_output(["nvidia-smi", "--query-gpu=name,driver_version,memory.total", "--format=csv,noheader"]),
        "gpu_processes_before_trial": gpu_processes_before_trial,
        "backend": "HuggingFace Transformers via official CHURRO 0.3.0 HF profile",
        "prompt_profile": "CHURRO_3B_XML_TEMPLATE",
        "torch_dtype": "float16 (checkpoint loaded without quantization)",
        "device_map": device_map,
        "device_map_setting": "auto; max_memory GPU 6GiB, CPU 9GiB",
        "max_new_tokens": max_new_tokens,
        "do_sample": False,
        "warmup_samples": 0,
        "runtime_import_seconds": round(runtime_import_seconds, 3),
        "processor_load_seconds": round(processor_load_seconds, 3),
        "model_load_seconds": round(model_load_seconds, 3),
        "cold_first_image_seconds": round(timings[0], 3) if timings else None,
        "warm_image_seconds": [round(value, 3) for value in timings[1:]],
        "warm_p50_seconds": round(statistics.median(timings[1:]), 3) if len(timings) > 1 else None,
        "warm_p95_seconds": round(sorted(timings[1:])[math.ceil(0.95 * len(timings[1:])) - 1], 3) if len(timings) > 1 else None,
        "ocr_loop_wall_seconds": round(wall_seconds, 3),
        "samples": len(scored_rows),
        "missing_outputs": len(supplied) - len(scored_rows),
        "blank_outputs": sum(not row["text"].strip() for row in scored_rows),
        "xml_extraction_status_counts": {status: parse_statuses.count(status) for status in sorted(set(parse_statuses))},
        "generation_tokens": generations,
        "possible_max_token_truncations": sum(row["generated_tokens"] == max_new_tokens for row in generations),
        "output_sha256": sha256(scored_path),
        "raw_output_sha256": sha256(raw_path),
        "peak_gpu_allocated_bytes": peak_gpu_allocated,
        "peak_gpu_reserved_bytes": peak_gpu_reserved,
        "process_peak_rss_kib": rss_kib,
        "resource_note": "No cgroup limit was set for this isolated trial; PyTorch peak is this process' CUDA allocation. CPU peak RSS is process max RSS.",
        "scores_text_transform": "Parse model XML; extract text nodes in reading order; insert line breaks after block elements. Keep exact generated text in raw-outputs.jsonl. Invalid XML uses the unmodified trimmed response. No OCR post-correction.",
        "benchmark_source_licenses": "As recorded in the verified v1.0.0 manifest.",
    }
    write_json(out_dir / "run.json", receipt)
    print(f"completed {len(scored_rows)} samples in {wall_seconds:.3f}s; scores={scored_path}", flush=True)


if __name__ == "__main__":
    run()
