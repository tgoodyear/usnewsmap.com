"""usnm_iris v1 job/result contract: pure Python standard library, no OpenVINO.

Shared by the worker inside the image (ocr/boinc/iris/worker.py) and by the server's staging and
validation, which import it without the engine. Every check raises ContractError (a ValueError).

Job (iris.json, exactly these keys, any JSON encoding up to MAX_JOB_BYTES; identity is semantic):
  v 1 | id sha256 hex of canonical JSON of the job without "id" (sort_keys, compact, ensure_ascii)
  crop_id, doc_id identifiers | host_id 9 | worker_image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<64>
  model_id, model_revision, prompt_version fixed constants | max_new_tokens 1..512
  page_sha256, page_bytes of page.jpg (a pre-cropped JPEG) | box [0, 0, width, height] (the whole image)

Result (iris-result.json, at most MAX_RESULT_BYTES of UTF-8 JSON, exactly RESULT_KEYS):
  v, id, crop_id, status "ok"|"error", text, device "GPU", model_id, model_revision,
  model_manifest_sha256, prompt_version, worker_image, host_id (from init_data.xml), page_sha256,
  max_new_tokens, generated_tokens, finish_reason "stop"|"length"|null, elapsed_ms, load_ms,
  generate_ms, engine {...}, diagnostics {...}, error null | {stage, code, message}
An output that reaches max_new_tokens is never "ok": it is an error with code token_limit_reached.
validate_result accepts well-formed error results unless require_ok=True; callers check status.
"""
from __future__ import annotations

import hashlib
import json
import math
import re
from typing import Any, Mapping

VERSION = 1
APP = "usnm_iris"
PLAN_CLASS = "docker_intel_opencl_4b"
HOST_ID = 9
WORKER_IMAGE_REPOSITORY = "ghcr.io/tgoodyear/usnm-boinc-worker"
MODEL_ID = "OpenVINO/Qwen3-VL-4B-Instruct-int4-ov"
MODEL_REVISION = "3ffb805d532a250e5010c5aa5ce6579e60b31d71"
# sha256 of model.lock's lines that are not comments (each ending in "\n"); see manifest_digest.
MODEL_MANIFEST_SHA256 = "f12e4fc8498cbe5454212ae843805a9afbd64028472a2c6778cffd0b1967ac8e"
OPENVINO_VERSION = "2026.4.1"
PROMPT_VERSION = "iris-ocr-v1"
PROMPTS = {"iris-ocr-v1": "Transcribe all text in this image. Return only the transcription."}
DEVICE = "GPU"

JOB_FILE = "iris.json"
PAGE_FILE = "page.jpg"
RESULT_FILE = "iris-result.json"

MAX_JOB_BYTES = 4096
MAX_RESULT_BYTES = 65536
MAX_NEW_TOKENS = 512
MAX_PAGE_BYTES = 8 * 1024 * 1024
MIN_EDGE = 16
MAX_EDGE = 4096
MAX_PIXELS = 4 * 1024 * 1024
MAX_TEXT_CHARS = 16384
MAX_MESSAGE_CHARS = 2000
# The worker kills its inference process and reports error stage "timeout" after this many seconds.
TIMEOUT_SECONDS = 900

JOB_KEYS = frozenset({
    "v", "id", "crop_id", "doc_id", "host_id", "worker_image", "model_id", "model_revision",
    "prompt_version", "max_new_tokens", "page_sha256", "page_bytes", "box",
})
RESULT_KEYS = frozenset({
    "v", "id", "crop_id", "status", "text", "device", "model_id", "model_revision",
    "model_manifest_sha256", "prompt_version", "worker_image", "host_id", "page_sha256",
    "max_new_tokens", "generated_tokens", "finish_reason", "elapsed_ms", "load_ms", "generate_ms", "engine",
    "diagnostics", "error",
})
ENGINE_KEYS = frozenset({"worker", "build_revision", "python", "openvino", "openvino_genai"})
ERROR_KEYS = frozenset({"stage", "code", "message"})
ERROR_STAGES = frozenset({"input", "host", "image", "model", "device", "load", "generate", "output", "timeout", "internal"})
FINISH_REASONS = frozenset({"stop", "length"})
WORKER = "usnm-iris-worker/1"

_HEX64 = re.compile(r"[0-9a-f]{64}")
_CROP_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}")
_DOC_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,199}")
_WORKER_IMAGE = re.compile(re.escape(WORKER_IMAGE_REPOSITORY) + r"@sha256:[0-9a-f]{64}")
_CODE = re.compile(r"[a-z][a-z0-9_]{0,63}")
_DIAG_KEY = re.compile(r"[a-z][a-z0-9_]{0,63}")


class ContractError(ValueError):
    pass


def _fail(message: str) -> None:
    raise ContractError(message)


def _int(value: Any, name: str, lo: int, hi: int) -> int:
    if type(value) is not int or not lo <= value <= hi:
        _fail(f"{name} must be an integer in [{lo}, {hi}]")
    return value


def _str(value: Any, name: str, pattern: re.Pattern[str] | None = None, max_len: int = 256) -> str:
    if type(value) is not str or len(value) > max_len or (pattern is not None and not pattern.fullmatch(value)):
        _fail(f"{name} is malformed")
    return value


def _keys(obj: Any, keys: frozenset[str], name: str) -> Mapping[str, Any]:
    if not isinstance(obj, Mapping):
        _fail(f"{name} must be an object")
    if set(obj) != keys:
        missing, extra = sorted(keys - set(obj)), sorted(set(obj) - keys)
        _fail(f"{name} keys differ: missing {missing}, unexpected {extra}")
    return obj


def canonical_bytes(obj: Any) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False).encode("ascii")


def canonical_job_id(job: Mapping[str, Any]) -> str:
    """sha256 hex of the canonical JSON of the job without its "id" key."""
    return hashlib.sha256(canonical_bytes({k: v for k, v in job.items() if k != "id"})).hexdigest()


def make_job(*, crop_id: str, doc_id: str, worker_image: str, page: bytes, width: int, height: int,
             max_new_tokens: int = MAX_NEW_TOKENS, host_id: int = HOST_ID) -> dict[str, Any]:
    """Builds and validates a job for a pre-cropped JPEG (width/height are checked against the JPEG)."""
    if jpeg_dimensions(page) != (width, height):
        _fail("width/height differ from the JPEG")
    job = {
        "v": VERSION, "crop_id": crop_id, "doc_id": doc_id, "host_id": host_id,
        "worker_image": worker_image, "model_id": MODEL_ID, "model_revision": MODEL_REVISION,
        "prompt_version": PROMPT_VERSION, "max_new_tokens": max_new_tokens,
        "page_sha256": hashlib.sha256(page).hexdigest(), "page_bytes": len(page), "box": [0, 0, width, height],
    }
    job["id"] = canonical_job_id(job)
    return validate_job(job)


def encode_job(job: Mapping[str, Any]) -> bytes:
    """Canonical bytes for iris.json (the worker accepts any JSON encoding of a valid job)."""
    data = canonical_bytes(validate_job(job))
    if len(data) > MAX_JOB_BYTES:
        _fail("job too large")
    return data


def validate_job(job: Any) -> dict[str, Any]:
    """Validates a parsed job and its identity; returns a plain copy, raises ContractError."""
    job = _keys(job, JOB_KEYS, "job")
    _int(job["v"], "v", VERSION, VERSION)
    _str(job["id"], "id", _HEX64)
    _str(job["crop_id"], "crop_id", _CROP_ID)
    _str(job["doc_id"], "doc_id", _DOC_ID)
    _int(job["host_id"], "host_id", HOST_ID, HOST_ID)
    _str(job["worker_image"], "worker_image", _WORKER_IMAGE)
    if job["model_id"] != MODEL_ID or job["model_revision"] != MODEL_REVISION:
        _fail("model_id/model_revision are not the pinned model")
    if job["prompt_version"] != PROMPT_VERSION:
        _fail("prompt_version is not supported")
    _int(job["max_new_tokens"], "max_new_tokens", 1, MAX_NEW_TOKENS)
    _str(job["page_sha256"], "page_sha256", _HEX64)
    _int(job["page_bytes"], "page_bytes", 1, MAX_PAGE_BYTES)
    box = job["box"]
    if not isinstance(box, list) or len(box) != 4:
        _fail("box must be [0, 0, width, height]")
    _int(box[0], "box[0]", 0, 0)
    _int(box[1], "box[1]", 0, 0)
    _int(box[2], "box width", MIN_EDGE, MAX_EDGE)
    _int(box[3], "box height", MIN_EDGE, MAX_EDGE)
    if box[2] * box[3] > MAX_PIXELS:
        _fail(f"box exceeds {MAX_PIXELS} pixels")
    if canonical_job_id(job) != job["id"]:
        _fail("id is not the canonical job identity")
    return json.loads(canonical_bytes(job))


def _reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for key, value in pairs:
        if key in out:
            _fail(f"duplicate key {key!r}")
        out[key] = value
    return out


def _reject_constant(value: str) -> None:
    _fail(f"non-finite number {value}")


def _loads(data: bytes, limit: int, name: str) -> Any:
    if not isinstance(data, (bytes, bytearray)):
        _fail(f"{name} must be bytes")
    if len(data) > limit:
        _fail(f"{name} exceeds {limit} bytes")
    try:
        text = bytes(data).decode("utf-8")
        return json.loads(text, object_pairs_hook=_reject_duplicates, parse_constant=_reject_constant)
    except ContractError:
        raise
    except (UnicodeDecodeError, ValueError) as exc:
        _fail(f"{name} is not JSON: {exc}")


def parse_job_bytes(data: bytes) -> dict[str, Any]:
    """Parses iris.json: bounded, no duplicate keys or non-finite numbers, identity checked."""
    return validate_job(_loads(data, MAX_JOB_BYTES, "iris.json"))


def check_page(job: Mapping[str, Any], page: bytes) -> tuple[int, int]:
    """page.jpg matches the job's size, sha256 and box; returns (width, height)."""
    if len(page) != job["page_bytes"] or hashlib.sha256(page).hexdigest() != job["page_sha256"]:
        _fail("page.jpg does not match page_bytes/page_sha256")
    size = jpeg_dimensions(page)
    if list(job["box"]) != [0, 0, *size]:
        _fail(f"box {job['box']} is not the whole {size[0]}x{size[1]} image")
    return size


_SOF = {0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF}


def jpeg_dimensions(data: bytes) -> tuple[int, int]:
    """(width, height) from the first JPEG SOF segment; raises ContractError on anything else."""
    if len(data) > MAX_PAGE_BYTES or data[:3] != b"\xff\xd8\xff":
        _fail("page is not a JPEG within the size bound")
    i = 2
    while i + 4 <= len(data):
        if data[i] != 0xFF:
            _fail("malformed JPEG marker")
        marker = data[i + 1]
        if marker == 0xFF:
            i += 1
            continue
        if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7:
            i += 2
            continue
        if marker in (0xD9, 0xDA):
            break
        length = int.from_bytes(data[i + 2:i + 4], "big")
        if length < 2 or i + 2 + length > len(data):
            _fail("truncated JPEG segment")
        if marker in _SOF:
            if length < 8:
                _fail("truncated JPEG frame header")
            height = int.from_bytes(data[i + 5:i + 7], "big")
            width = int.from_bytes(data[i + 7:i + 9], "big")
            if not (MIN_EDGE <= width <= MAX_EDGE and MIN_EDGE <= height <= MAX_EDGE) or width * height > MAX_PIXELS:
                _fail(f"JPEG {width}x{height} is outside the bounds")
            return width, height
        i += 2 + length
    _fail("JPEG has no frame header")
    raise AssertionError


def manifest_digest(text: str) -> str:
    """sha256 hex of a lock file's non-comment lines, each terminated by a newline."""
    lines = [line for line in text.splitlines() if line and not line.startswith("#")]
    return hashlib.sha256("".join(line + "\n" for line in lines).encode("utf-8")).hexdigest()


def parse_model_lock(text: str) -> dict[str, tuple[str, int]]:
    """model.lock: '<sha256> <bytes> <path>' per line -> {path: (sha256, bytes)}."""
    files: dict[str, tuple[str, int]] = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        parts = line.split(" ")
        if len(parts) != 3 or not _HEX64.fullmatch(parts[0]) or not parts[1].isdigit():
            _fail(f"bad model.lock line: {line!r}")
        path = parts[2]
        if not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", path) or path in (".", "..") or path in files:
            _fail(f"bad model.lock path: {path!r}")
        files[path] = (parts[0], int(parts[1]))
    if not files:
        _fail("empty model.lock")
    return files


def _optional_str(value: Any, name: str, max_len: int = 256) -> None:
    if value is not None:
        _str(value, name, None, max_len)


def _diag_value(value: Any, depth: int = 0) -> None:
    if value is None or type(value) in (bool, int):
        return
    if type(value) is float:
        if not math.isfinite(value):
            _fail("non-finite diagnostic")
        return
    if type(value) is str:
        if len(value) > 1024:
            _fail("diagnostic string too long")
        return
    if isinstance(value, list) and depth < 2 and len(value) <= 32:
        for item in value:
            _diag_value(item, depth + 1)
        return
    if isinstance(value, Mapping) and depth < 2 and len(value) <= 64:
        for key, item in value.items():
            _str(key, "diagnostic key", _DIAG_KEY)
            _diag_value(item, depth + 1)
        return
    _fail("diagnostics value is not bounded JSON")


def encode_result(result: Mapping[str, Any]) -> bytes:
    data = json.dumps(result, sort_keys=True, ensure_ascii=False, allow_nan=False, indent=1).encode("utf-8") + b"\n"
    if len(data) > MAX_RESULT_BYTES:
        _fail(f"result exceeds {MAX_RESULT_BYTES} bytes")
    return data


def parse_result_bytes(data: bytes, job: Mapping[str, Any] | None = None, *, require_ok: bool = False) -> dict[str, Any]:
    """Parses iris-result.json bytes (bounded) and validates it with validate_result."""
    return validate_result(_loads(data, MAX_RESULT_BYTES, "iris-result.json"), job, require_ok=require_ok)


def validate_result(result: Any, job: Mapping[str, Any] | None = None, *, require_ok: bool = False) -> dict[str, Any]:
    """Validates a parsed result against its job (when given); raises ContractError (ValueError).

    A well-formed error result bound to the job passes unless require_ok=True; check status before
    crediting or scoring. An "ok" result always has text, finish_reason "stop" and fewer
    generated_tokens than max_new_tokens.
    """
    result = _keys(result, RESULT_KEYS, "result")
    _int(result["v"], "v", VERSION, VERSION)
    status = result["status"]
    if status not in ("ok", "error"):
        _fail("status must be ok or error")
    if result["device"] != DEVICE:
        _fail("device must be GPU")
    if result["model_id"] != MODEL_ID or result["model_revision"] != MODEL_REVISION:
        _fail("result model is not the pinned model")
    if result["model_manifest_sha256"] is not None and result["model_manifest_sha256"] != MODEL_MANIFEST_SHA256:
        _fail("model_manifest_sha256 is not the pinned manifest")
    engine = _keys(result["engine"], ENGINE_KEYS, "engine")
    if engine["worker"] != WORKER:
        _fail("engine.worker is not this worker")
    for key in ("build_revision", "python", "openvino", "openvino_genai"):
        _optional_str(engine[key], f"engine.{key}", 128)
    if not isinstance(result["diagnostics"], Mapping):
        _fail("diagnostics must be an object")
    _diag_value(result["diagnostics"])
    for key in ("id", "page_sha256"):
        if result[key] is not None:
            _str(result[key], key, _HEX64)
    if result["crop_id"] is not None:
        _str(result["crop_id"], "crop_id", _CROP_ID)
    if result["worker_image"] is not None:
        _str(result["worker_image"], "worker_image", _WORKER_IMAGE)
    if result["prompt_version"] is not None and result["prompt_version"] != PROMPT_VERSION:
        _fail("prompt_version is not supported")
    if result["host_id"] is not None:
        _int(result["host_id"], "host_id", 0, 2**63 - 1)
    if result["max_new_tokens"] is not None:
        _int(result["max_new_tokens"], "max_new_tokens", 1, MAX_NEW_TOKENS)
    if result["generated_tokens"] is not None:
        _int(result["generated_tokens"], "generated_tokens", 0, MAX_NEW_TOKENS)
    if result["finish_reason"] is not None and result["finish_reason"] not in FINISH_REASONS:
        _fail("finish_reason is unknown")
    _int(result["elapsed_ms"], "elapsed_ms", 0, 2**31 - 1)
    for key in ("load_ms", "generate_ms"):
        if result[key] is not None:
            _int(result[key], key, 0, 2**31 - 1)
    text, error = result["text"], result["error"]
    if status == "ok":
        if error is not None:
            _fail("ok result carries an error")
        _str(text, "text", None, MAX_TEXT_CHARS)
        if not text.strip():
            _fail("ok result has empty text")
        for key in ("id", "crop_id", "page_sha256", "worker_image", "prompt_version", "host_id",
                    "max_new_tokens", "generated_tokens", "load_ms", "generate_ms", "model_manifest_sha256"):
            if result[key] is None:
                _fail(f"ok result lacks {key}")
        for key in ("openvino", "openvino_genai"):
            if engine[key] is None:
                _fail(f"ok result lacks engine.{key}")
        if not engine["openvino"].startswith(OPENVINO_VERSION):
            _fail("engine.openvino is not the pinned OpenVINO")
        if result["generated_tokens"] >= result["max_new_tokens"] or result["finish_reason"] != "stop":
            _fail("ok result reached the token limit")
        if result["host_id"] != HOST_ID:
            _fail("host_id is not the assigned host")
    else:
        if require_ok:
            _fail("result status is error: " + (str(error.get("code")) if isinstance(error, Mapping) else "?"))
        if text is not None:
            _fail("error result carries text")
        error = _keys(error, ERROR_KEYS, "error")
        if error["stage"] not in ERROR_STAGES:
            _fail("error.stage is unknown")
        _str(error["code"], "error.code", _CODE)
        _str(error["message"], "error.message", None, MAX_MESSAGE_CHARS)
    if job is not None:
        job = validate_job(job)
        expected = {
            "id": job["id"], "crop_id": job["crop_id"], "worker_image": job["worker_image"],
            "page_sha256": job["page_sha256"], "prompt_version": job["prompt_version"],
            "max_new_tokens": job["max_new_tokens"], "host_id": job["host_id"],
        }
        if status != "ok":
            del expected["host_id"]
        for key, value in expected.items():
            if result[key] is not None or status == "ok":
                if result[key] != value:
                    _fail(f"result {key} does not match the job")
    encode_result(result)
    return json.loads(json.dumps(result))
