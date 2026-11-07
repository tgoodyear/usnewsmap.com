"""usnm_iris v2 bundle contract (#736): several pre-cropped JPEGs per workunit, one model load.

Pure Python standard library, like contract.py, and built on it: a bundle's crops are v1 jobs that
share the worker image, model, prompt, host and token cap, so every per-crop result is an ordinary v1
result bound to its own derived v1 job (crop_job). A bundle can therefore never hide a failed crop or
miscount pages: results holds exactly one entry per crop, in order, each bound to that crop's
identity, page checksum and bytes.

Bundle job (iris.json, "v": 2) and pack (pages.pack):
  v 2 | id sha256 of the canonical JSON of the job without "id" | host_id, worker_image, model_id,
  model_revision, prompt_version, max_new_tokens as in v1 | pack_bytes, pack_sha256 of pages.pack
  crops: 1..MAX_CROPS of {crop_id, doc_id, page_sha256, page_bytes, box [0,0,w,h]}
  pages.pack is the crops' JPEGs concatenated in order; offsets follow from page_bytes. crop_id and
  page_sha256 are unique within a bundle.

Bundle result (iris-result.json):
  v 2, id, status "ok" (every crop ok) | "partial" (some ok) | "error" (none ok), host_id,
  worker_image, elapsed_ms, load_ms, results (one v1 result per crop), error (null, or the shared
  cause when no crop ran).
"""
from __future__ import annotations

import hashlib
import json
from typing import Any, Iterator, Mapping, Sequence

import contract as c

BUNDLE_VERSION = 2
PACK_FILE = "pages.pack"
MAX_CROPS = 32
MAX_PACK_BYTES = 128 * 1024 * 1024
MAX_BUNDLE_JOB_BYTES = 32768
MAX_BUNDLE_RESULT_BYTES = MAX_CROPS * c.MAX_RESULT_BYTES + c.MAX_RESULT_BYTES
# Fixed overhead plus a per-crop allowance: measured 4-9 s per crop after a 24 s cold load.
TIMEOUT_BASE_SECONDS = 900
TIMEOUT_PER_CROP_SECONDS = 90

BUNDLE_JOB_KEYS = frozenset({
    "v", "id", "host_id", "worker_image", "model_id", "model_revision", "prompt_version",
    "max_new_tokens", "pack_bytes", "pack_sha256", "crops",
})
CROP_KEYS = frozenset({"crop_id", "doc_id", "page_sha256", "page_bytes", "box"})
BUNDLE_RESULT_KEYS = frozenset({"v", "id", "status", "host_id", "worker_image", "elapsed_ms", "load_ms", "results", "error"})
STATUSES = frozenset({"ok", "partial", "error"})


def timeout_seconds(job: Mapping[str, Any]) -> int:
    return TIMEOUT_BASE_SECONDS + TIMEOUT_PER_CROP_SECONDS * len(job["crops"])


def crop_job(job: Mapping[str, Any], index: int) -> dict[str, Any]:
    """The v1 job for one crop of a bundle: this is the identity its result is bound to."""
    crop = job["crops"][index]
    single = {
        "v": c.VERSION, "crop_id": crop["crop_id"], "doc_id": crop["doc_id"], "host_id": job["host_id"],
        "worker_image": job["worker_image"], "model_id": job["model_id"], "model_revision": job["model_revision"],
        "prompt_version": job["prompt_version"], "max_new_tokens": job["max_new_tokens"],
        "page_sha256": crop["page_sha256"], "page_bytes": crop["page_bytes"], "box": list(crop["box"]),
    }
    single["id"] = c.canonical_job_id(single)
    return c.validate_job(single)


def crop_slices(job: Mapping[str, Any]) -> Iterator[tuple[int, int]]:
    """(offset, length) of each crop's JPEG in pages.pack."""
    offset = 0
    for crop in job["crops"]:
        yield offset, crop["page_bytes"]
        offset += crop["page_bytes"]


def make_bundle(*, crops: Sequence[Mapping[str, Any]], worker_image: str,
                max_new_tokens: int = c.MAX_NEW_TOKENS, host_id: int = c.HOST_ID) -> tuple[dict[str, Any], bytes]:
    """crops: dicts with crop_id, doc_id, page (JPEG bytes), width, height. Returns (job, pack)."""
    entries, pack = [], bytearray()
    for item in crops:
        page = item["page"]
        if c.jpeg_dimensions(page) != (item["width"], item["height"]):
            c._fail("width/height differ from the JPEG")
        entries.append({
            "crop_id": item["crop_id"], "doc_id": item["doc_id"], "page_sha256": hashlib.sha256(page).hexdigest(),
            "page_bytes": len(page), "box": [0, 0, item["width"], item["height"]],
        })
        pack += page
    job = {
        "v": BUNDLE_VERSION, "host_id": host_id, "worker_image": worker_image, "model_id": c.MODEL_ID,
        "model_revision": c.MODEL_REVISION, "prompt_version": c.PROMPT_VERSION, "max_new_tokens": max_new_tokens,
        "pack_bytes": len(pack), "pack_sha256": hashlib.sha256(pack).hexdigest(), "crops": entries,
    }
    job["id"] = c.canonical_job_id(job)
    return validate_bundle_job(job), bytes(pack)


def validate_bundle_job(job: Any) -> dict[str, Any]:
    job = c._keys(job, BUNDLE_JOB_KEYS, "bundle job")
    c._int(job["v"], "v", BUNDLE_VERSION, BUNDLE_VERSION)
    c._str(job["id"], "id", c._HEX64)
    c._int(job["host_id"], "host_id", c.HOST_ID, c.HOST_ID)
    c._str(job["worker_image"], "worker_image", c._WORKER_IMAGE)
    if job["model_id"] != c.MODEL_ID or job["model_revision"] != c.MODEL_REVISION:
        c._fail("model_id/model_revision are not the pinned model")
    if job["prompt_version"] != c.PROMPT_VERSION:
        c._fail("prompt_version is not supported")
    c._int(job["max_new_tokens"], "max_new_tokens", 1, c.MAX_NEW_TOKENS)
    c._int(job["pack_bytes"], "pack_bytes", 1, MAX_PACK_BYTES)
    c._str(job["pack_sha256"], "pack_sha256", c._HEX64)
    crops = job["crops"]
    if not isinstance(crops, list) or not 1 <= len(crops) <= MAX_CROPS:
        c._fail(f"crops must hold 1..{MAX_CROPS} entries")
    total, ids, pages = 0, set(), set()
    for entry in crops:
        entry = c._keys(entry, CROP_KEYS, "crop")
        c._int(entry["page_bytes"], "page_bytes", 1, c.MAX_PAGE_BYTES)
        total += entry["page_bytes"]
        if entry["crop_id"] in ids or entry["page_sha256"] in pages:
            c._fail("duplicate crop_id or page_sha256 in bundle")
        ids.add(entry["crop_id"])
        pages.add(entry["page_sha256"])
    if total != job["pack_bytes"]:
        c._fail("crop page_bytes do not add up to pack_bytes")
    # Each crop must be a valid v1 job (shared fields, box, sizes), and its identity unique.
    singles = {crop_job(job, index)["id"] for index in range(len(crops))}
    if len(singles) != len(crops):
        c._fail("duplicate derived crop identity")
    if c.canonical_job_id(job) != job["id"]:
        c._fail("id is not the canonical bundle identity")
    return json.loads(c.canonical_bytes(job))


def encode_bundle_job(job: Mapping[str, Any]) -> bytes:
    data = c.canonical_bytes(validate_bundle_job(job))
    if len(data) > MAX_BUNDLE_JOB_BYTES:
        c._fail("bundle job too large")
    return data


def parse_bundle_job_bytes(data: bytes) -> dict[str, Any]:
    return validate_bundle_job(c._loads(data, MAX_BUNDLE_JOB_BYTES, "iris.json"))


def check_pack(job: Mapping[str, Any], pack: bytes) -> None:
    """pages.pack matches the job's size and checksum, and every crop's own page checksum and box."""
    if len(pack) != job["pack_bytes"] or hashlib.sha256(pack).hexdigest() != job["pack_sha256"]:
        c._fail("pages.pack does not match pack_bytes/pack_sha256")
    for index, (offset, length) in enumerate(crop_slices(job)):
        c.check_page(crop_job(job, index), pack[offset:offset + length])


def status_of(results: Sequence[Mapping[str, Any]]) -> str:
    ok = sum(1 for r in results if r["status"] == "ok")
    return "ok" if ok == len(results) else "partial" if ok else "error"


def encode_bundle_result(result: Mapping[str, Any]) -> bytes:
    data = json.dumps(result, sort_keys=True, ensure_ascii=False, allow_nan=False, indent=1).encode("utf-8") + b"\n"
    if len(data) > MAX_BUNDLE_RESULT_BYTES:
        c._fail(f"bundle result exceeds {MAX_BUNDLE_RESULT_BYTES} bytes")
    return data


def parse_bundle_result_bytes(data: bytes, job: Mapping[str, Any] | None = None) -> dict[str, Any]:
    return validate_bundle_result(c._loads(data, MAX_BUNDLE_RESULT_BYTES, "iris-result.json"), job)


def validate_bundle_result(result: Any, job: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Validates a bundle result; with the job, every crop is bound to its own derived identity."""
    if job is not None:
        job = validate_bundle_job(job)
    result = c._keys(result, BUNDLE_RESULT_KEYS, "bundle result")
    c._int(result["v"], "v", BUNDLE_VERSION, BUNDLE_VERSION)
    if result["status"] not in STATUSES:
        c._fail("status must be ok, partial or error")
    c._str(result["id"], "id", c._HEX64)
    c._str(result["worker_image"], "worker_image", c._WORKER_IMAGE)
    if result["host_id"] is not None:
        c._int(result["host_id"], "host_id", 0, 2**63 - 1)
    c._int(result["elapsed_ms"], "elapsed_ms", 0, 2**31 - 1)
    if result["load_ms"] is not None:
        c._int(result["load_ms"], "load_ms", 0, 2**31 - 1)
    rows = result["results"]
    if not isinstance(rows, list) or not 1 <= len(rows) <= MAX_CROPS:
        c._fail(f"results must hold 1..{MAX_CROPS} entries")
    checked = []
    for index, row in enumerate(rows):
        single = crop_job(job, index) if job is not None else None
        checked.append(c.validate_result(row, single))
        if single is not None:
            for key in ("id", "crop_id", "page_sha256"):
                if checked[-1][key] != single[key]:
                    c._fail(f"results[{index}] {key} is not bound to its crop")
    if result["status"] != status_of(checked):
        c._fail("bundle status does not match its per-crop results")
    error = result["error"]
    if error is not None:
        error = c._keys(error, c.ERROR_KEYS, "error")
        if error["stage"] not in c.ERROR_STAGES:
            c._fail("error.stage is unknown")
        c._str(error["code"], "error.code", c._CODE)
        c._str(error["message"], "error.message", None, c.MAX_MESSAGE_CHARS)
        if result["status"] != "error":
            c._fail("only an error bundle carries a shared error")
    if job is not None:
        if len(rows) != len(job["crops"]):
            c._fail("results do not cover exactly the job's crops")
        if result["id"] != job["id"] or result["worker_image"] != job["worker_image"]:
            c._fail("bundle result does not match the job")
        if result["status"] != "error" and result["host_id"] != job["host_id"]:
            c._fail("host_id is not the assigned host")
    encode_bundle_result(result)
    return json.loads(json.dumps(result))
