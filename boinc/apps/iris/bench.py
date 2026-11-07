"""Experimental usnm_iris benchmark (#736): one VLMPipeline for N sequential crops. Not shipped in
the worker image and not part of the BOINC contract.

Runs in the pinned image with the same GPU-only device checks, prompt, precision hint and token cap
as worker.py. Crops are processed sequentially in groups of --n; each group constructs one pipeline,
runs generate() once per crop with no chat history, then drops the pipeline. Output is JSON with
per-crop timings and the memory counters visible inside the container.
"""
from __future__ import annotations

import argparse
import gc
import hashlib
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract as c  # noqa: E402
import worker as w  # noqa: E402

CGROUP = Path("/sys/fs/cgroup")


def cgroup_counter(name: str) -> int | None:
    try:
        return int((CGROUP / name).read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return None


def rss_bytes() -> int | None:
    try:
        for line in Path("/proc/self/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) * 1024
    except (OSError, ValueError):
        pass
    return None


def memory_sample() -> dict[str, Any]:
    return {"cgroup_current": cgroup_counter("memory.current"), "cgroup_peak": cgroup_counter("memory.peak"),
            "rss": rss_bytes()}


def load_units(root: Path, limit: int, offset: int = 0) -> list[dict[str, Any]]:
    """Distinct, checksum-verified crops: <root>/<id>/iris.json + page.jpg."""
    units, seen = [], set()
    for unit in sorted(p for p in root.iterdir() if p.is_dir()):
        job = c.parse_job_bytes(w.read_bounded(unit / c.JOB_FILE, c.MAX_JOB_BYTES, "input", "job_unreadable"))
        page = w.read_bounded(unit / c.PAGE_FILE, c.MAX_PAGE_BYTES, "image", "page_unreadable")
        width, height = c.check_page(job, page)
        if job["page_sha256"] in seen:
            raise SystemExit(f"duplicate crop image {job['crop_id']}: distinct crops required")
        seen.add(job["page_sha256"])
        units.append({"job": job, "page": page, "width": width, "height": height})
    if len(units) < offset + limit:
        raise SystemExit(f"{len(units)} crops available, {offset + limit} requested")
    return units[offset:offset + limit]


def metric(perf: Any, name: str) -> float | None:
    try:
        value = getattr(perf, name)()
        return float(getattr(value, "mean", value))
    except Exception:  # noqa: BLE001 - metric absent in this API version
        return None


def run_crop(pipe: Any, ov: Any, unit: dict[str, Any]) -> dict[str, Any]:
    job = unit["job"]
    start = time.monotonic()
    pixels = w.decode_rgb(unit["page"], unit["width"], unit["height"])
    decode_ms = (time.monotonic() - start) * 1000
    start = time.monotonic()
    output = pipe.generate(c.PROMPTS[job["prompt_version"]], image=ov.Tensor(pixels),
                           max_new_tokens=job["max_new_tokens"], do_sample=False)
    generate_ms = (time.monotonic() - start) * 1000
    texts = getattr(output, "texts", None)
    text = str(texts[0]) if texts else str(output)
    perf = output.perf_metrics
    reasons = getattr(output, "finish_reasons", None)
    reason = None
    if reasons:
        reason = getattr(reasons[0], "name", str(reasons[0]).split(".")[-1])
    tokens = int(perf.get_num_generated_tokens())
    return {
        "crop_id": job["crop_id"], "page_sha256": job["page_sha256"], "width": unit["width"],
        "height": unit["height"], "decode_ms": round(decode_ms, 1), "generate_ms": round(generate_ms, 1),
        "tokens": tokens, "finish_reason": reason,
        "truncated": reason == "LENGTH" or tokens >= job["max_new_tokens"],
        "empty": not text.strip(), "text": text, "text_sha256": hashlib.sha256(text.encode()).hexdigest(),
        "ttft_ms": metric(perf, "get_ttft"), "tpot_ms": metric(perf, "get_tpot"),
        "tokens_per_s": metric(perf, "get_throughput"),
        "tokenization_ms": metric(perf, "get_tokenization_duration"),
        "prepare_embeddings_ms": metric(perf, "get_prepare_embeddings_duration"),
        "inference_ms": metric(perf, "get_inference_duration"),
        "memory": memory_sample(),
    }


def main() -> int:
    parser = argparse.ArgumentParser(allow_abbrev=False)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--count", type=int, required=True, help="distinct crops to process")
    parser.add_argument("--offset", type=int, default=0, help="skip this many crops (sorted by job id)")
    parser.add_argument("--n", type=int, required=True, help="crops per pipeline construction")
    parser.add_argument("--cache-dir", type=Path, help="OpenVINO CACHE_DIR (omit for the current cold behavior)")
    parser.add_argument("--label", default="")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.n < 1 or args.count < 1:
        raise SystemExit("--n and --count must be positive")
    wall = time.monotonic()
    units = load_units(args.inputs, args.count, args.offset)
    paths = w.Paths()
    diag: dict[str, Any] = {}
    engine: dict[str, Any] = {}
    w.check_model(paths)
    w.check_render_node(paths, diag)
    ov, genai = w._versions(engine)
    w.check_openvino_gpu(ov, diag)
    config: dict[str, Any] = {"INFERENCE_PRECISION_HINT": "f16"}
    if args.cache_dir:
        config["CACHE_DIR"] = str(args.cache_dir)
    groups, crops = [], []
    for first in range(0, len(units), args.n):
        batch = units[first:first + args.n]
        start = time.monotonic()
        pipe = genai.VLMPipeline(str(paths.model_dir), c.DEVICE, **config)
        load_ms = (time.monotonic() - start) * 1000
        group_start = time.monotonic()
        rows = []
        for index, unit in enumerate(batch):
            row = run_crop(pipe, ov, unit)
            row["position_in_group"] = index
            rows.append(row)
        crops.extend(rows)
        del pipe
        gc.collect()
        groups.append({"crops": len(batch), "load_ms": round(load_ms, 1),
                       "group_ms": round((time.monotonic() - group_start) * 1000 + load_ms, 1),
                       "memory_after_release": memory_sample()})
    cache_bytes = None
    if args.cache_dir and args.cache_dir.is_dir():
        cache_bytes = sum(p.stat().st_size for p in args.cache_dir.rglob("*") if p.is_file())
    report = {
        "label": args.label, "n": args.n, "count": len(units), "config": config, "engine": engine,
        "device": {k: diag.get(k) for k in ("full_device_name", "device_architecture", "gpu_total_mem_bytes")},
        "wall_ms": round((time.monotonic() - wall) * 1000, 1), "cache_bytes": cache_bytes,
        "groups": groups, "crops": crops, "memory_events": (CGROUP / "memory.events").read_text()
        if (CGROUP / "memory.events").exists() else None,
    }
    args.out.write_text(json.dumps(report, indent=1))
    print(f"{args.label}: {len(units)} crops, wall {report['wall_ms'] / 1000:.1f} s", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
