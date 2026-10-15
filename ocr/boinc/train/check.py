#!/usr/bin/env python3
"""Structural acceptance and quarantine, NOT scientific validation or promotion."""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
import shutil
import struct
import tempfile

from worker import HEX, MAX_ADAPTER, load_job, sha

REPORT_KEYS = {
    "v", "job_sha256", "base_revision", "bundle_sha256", "steps", "rank", "seed",
    "learning_rate", "losses", "adapter_sha256", "adapter_bytes", "elapsed_seconds",
    "peak_gpu_bytes", "gpu", "torch", "candidate_status",
}


def bounded(path: Path, limit: int) -> bytes:
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"{path.name}: output exceeds limit")
    return data


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field")
        result[key] = value
    return result


def tensor_shapes(rank: int) -> dict:
    shapes = {}
    for layer in range(28):
        for projection, size in (("q_proj", 2048), ("v_proj", 1024)):
            prefix = f"base_model.model.model.language_model.layers.{layer}.self_attn.{projection}"
            shapes[f"{prefix}.lora_A.weight"] = [rank, 2048]
            shapes[f"{prefix}.lora_B.weight"] = [size, rank]
    return shapes


def check_tensors(data: bytes, rank: int) -> None:
    if len(data) < 8:
        raise ValueError("truncated safetensors")
    length = struct.unpack("<Q", data[:8])[0]
    if not 2 <= length <= 128 * 1024 or 8 + length >= len(data):
        raise ValueError("invalid safetensors header length")
    header = json.loads(data[8:8 + length], object_pairs_hook=unique_object)
    expected = tensor_shapes(rank)
    if not isinstance(header, dict) or set(header) != set(expected):
        raise ValueError("adapter tensor keys differ from frozen-base LoRA contract")
    segments = []
    for name, shape in expected.items():
        entry = header[name]
        if (not isinstance(entry, dict) or set(entry) != {"dtype", "shape", "data_offsets"}
                or entry["dtype"] != "F32" or entry["shape"] != shape):
            raise ValueError("adapter dtype/shape differs from the pinned model")
        offsets = entry["data_offsets"]
        if (not isinstance(offsets, list) or len(offsets) != 2
                or any(type(n) is not int for n in offsets)
                or offsets[0] < 0 or offsets[1] - offsets[0] != math.prod(shape) * 4):
            raise ValueError("invalid tensor offsets")
        segments.append(tuple(offsets))
    cursor = 0
    for start, end in sorted(segments):
        if start != cursor:
            raise ValueError("overlapping or non-contiguous tensors")
        cursor = end
    values = memoryview(data)[8 + length:]
    if cursor != len(values):
        raise ValueError("truncated tensors or trailing data")
    if not all(math.isfinite(value) for (value,) in struct.iter_unpack("<f", values)):
        raise ValueError("non-finite adapter tensor")


def validate(adapter: bytes, raw_report: bytes, unit: Path) -> dict:
    report = json.loads(raw_report, object_pairs_hook=unique_object)
    if not isinstance(report, dict) or set(report) != REPORT_KEYS:
        raise ValueError("invalid training result fields")
    job, _ = load_job(unit)
    identity = sha((unit / "train.json").read_bytes())
    for key in ("base_revision", "bundle_sha256", "steps", "rank", "seed", "learning_rate"):
        if type(report[key]) is not type(job[key]) or report[key] != job[key]:
            raise ValueError(f"training result differs from submitted {key}")
    if (type(report["v"]) is not int or report["v"] != 1 or report["job_sha256"] != identity
            or report["adapter_sha256"] != sha(adapter)
            or type(report["adapter_bytes"]) is not int or report["adapter_bytes"] != len(adapter)
            or report["candidate_status"] != "untrusted-requires-held-out-evaluation"):
        raise ValueError("invalid result identity or candidate status")
    losses = report["losses"]
    if (not isinstance(losses, list) or len(losses) != job["steps"]
            or any(type(n) not in (int, float) or not math.isfinite(n) or n < 0 for n in losses)):
        raise ValueError("invalid training losses")
    for key in ("elapsed_seconds", "peak_gpu_bytes"):
        n = report[key]
        if type(n) not in (int, float) or not math.isfinite(n) or n <= 0:
            raise ValueError(f"invalid {key}")
    for key in ("gpu", "torch"):
        if not isinstance(report[key], str) or not 1 <= len(report[key]) <= 256:
            raise ValueError(f"invalid {key}")
    check_tensors(adapter, job["rank"])
    return report


def quarantine(adapter_path: Path, report_path: Path, campaigns: Path, keep: Path) -> dict:
    adapter = bounded(adapter_path, MAX_ADAPTER)
    raw = bounded(report_path, 16384)
    report = json.loads(raw, object_pairs_hook=unique_object)
    identity = report.get("job_sha256") if isinstance(report, dict) else None
    if not isinstance(identity, str) or not HEX.fullmatch(identity):
        raise ValueError("invalid job identity")
    units = list(campaigns.glob(f"*/units/{identity}"))
    if len(units) != 1 or not (units[0] / "submitted").is_file():
        raise ValueError("result has no unique submitted training job")
    report = validate(adapter, raw, units[0])
    candidate = sha(adapter + raw)
    keep.mkdir(parents=True, exist_ok=True)
    dest = keep / candidate
    if dest.exists():
        if ((dest / "adapter.safetensors").read_bytes() != adapter
                or (dest / "result.json").read_bytes() != raw):
            raise ValueError("quarantined candidate changed")
    else:
        with tempfile.TemporaryDirectory(prefix=".candidate.", dir=keep) as tmp:
            stage = Path(tmp) / candidate
            stage.mkdir()
            (stage / "adapter.safetensors").write_bytes(adapter)
            (stage / "result.json").write_bytes(raw)
            shutil.copyfile(units[0] / "train.json", stage / "train.json")
            stage.rename(dest)
    return {"candidate": candidate, "status": "quarantined", "job_sha256": report["job_sha256"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("adapter", type=Path)
    parser.add_argument("report", type=Path)
    parser.add_argument("--campaigns", required=True, type=Path)
    parser.add_argument("--keep", required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(quarantine(args.adapter, args.report, args.campaigns, args.keep)))
