"""usnm_iris worker: pre-cropped newspaper JPEGs, Qwen3-VL-4B INT4 on an Intel Iris Xe GPU.

A unit is either one crop (iris.json v1 + page.jpg, app version 1.0) or a bundle (iris.json v2 +
pages.pack, bundle.py): one model load, then each crop in order, every crop with its own bound result.

Runs in the image built from ocr/boinc/iris/Containerfile under the stock BOINC docker_wrapper, which
mounts the slot at /work. Reads only /work/iris.json, /work/page.jpg and /work/init_data.xml (regular
files, no symlinks, bounded), and writes only /work/iris-result.json. Takes no arguments, makes no
network calls, and loads the model bundled in the image with openvino_genai.VLMPipeline on device
"GPU" only: no CPU, AUTO or HETERO fallback.

The process BOINC starts is a supervisor: it runs the inference in a child process (its own session)
that writes its result to /tmp, kills the child's process group after contract.TIMEOUT_SECONDS, and
writes the result itself, so a timeout, crash or OOM kill still returns a diagnostic error result.
Exit status 0 means an "ok" result; otherwise the result is a well-formed "error" and the exit
status is 1 (3 if even the result could not be written).

Compilation is cold (no persistent CACHE_DIR), with an FP16 precision hint and plugin defaults.
A bundle amortizes that load over its crops; measurements are in docs/reports/2026-10-iris-model-reuse.
Process RSS excludes i915/shmem allocations charged to the container: it is not a memory-cap proof.
"""
from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import platform
import re
import signal
import stat
import subprocess
import sys
import time
import traceback
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bundle as b  # noqa: E402
import contract as c  # noqa: E402

# Tiger Lake GT2 Iris Xe (Computer9's i7-1185G7 reports 0x9a49).
INTEL_VENDOR = "0x8086"
IRIS_XE_DEVICE_IDS = frozenset({"0x9a40", "0x9a49"})
RENDER_NODE = "renderD128"
MAX_INIT_DATA_BYTES = 1024 * 1024


@dataclasses.dataclass(frozen=True)
class Paths:
    work: Path = Path("/work")
    model_dir: Path = Path("/opt/iris/model")
    model_lock: Path = Path("/opt/iris/model.lock")
    dev_dri: Path = Path("/dev/dri")
    sys_drm: Path = Path("/sys/class/drm")
    scratch: Path = Path("/tmp/usnm-iris")


class Failure(Exception):
    def __init__(self, stage: str, code: str, message: str):
        super().__init__(message)
        self.stage, self.code, self.message = stage, code, message


def read_bounded(path: Path, limit: int, stage: str, code: str) -> bytes:
    """Reads a regular, non-symlink file of at most limit bytes."""
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        raise Failure(stage, code, f"{path.name} is missing") from None
    if not stat.S_ISREG(st.st_mode):
        raise Failure(stage, code, f"{path.name} is not a regular file")
    if st.st_size > limit:
        raise Failure(stage, code, f"{path.name} exceeds {limit} bytes")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(fd, "rb") as fh:
        data = fh.read(limit + 1)
    if len(data) > limit:
        raise Failure(stage, code, f"{path.name} exceeds {limit} bytes")
    return data


def boinc_hostid(work: Path) -> int:
    data = read_bounded(work / "init_data.xml", MAX_INIT_DATA_BYTES, "host", "init_data_unreadable")
    found = re.findall(rb"<hostid>\s*(\d{1,18})\s*</hostid>", data)
    if len(found) != 1:
        raise Failure("host", "hostid_missing", "init_data.xml has no single <hostid>")
    return int(found[0])


def decode_rgb(page: bytes, width: int, height: int) -> Any:
    """Decodes the JPEG with Pillow into a 1xHxWx3 uint8 array."""
    import io

    import numpy as np
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = c.MAX_PIXELS
    with Image.open(io.BytesIO(page)) as image:
        if image.format != "JPEG" or image.size != (width, height):
            raise Failure("image", "image_mismatch", f"decoded {image.format} {image.size} != JPEG {width}x{height}")
        rgb = image.convert("RGB")
        rgb.load()
    return np.ascontiguousarray(np.asarray(rgb, dtype=np.uint8)[None])


def check_model(paths: Paths) -> str:
    """The bundled model matches the pinned manifest (names and sizes; content was hashed at build)."""
    text = read_bounded(paths.model_lock, 65536, "model", "model_lock_unreadable").decode("utf-8")
    digest = c.manifest_digest(text)
    if digest != c.MODEL_MANIFEST_SHA256:
        raise Failure("model", "model_manifest_mismatch", f"model.lock digest {digest}")
    files = c.parse_model_lock(text)
    try:
        present = {p.name: p for p in paths.model_dir.iterdir()}
    except OSError as exc:
        raise Failure("model", "model_missing", str(exc)) from None
    if set(present) != set(files):
        raise Failure("model", "model_files_differ", f"unexpected {sorted(set(present) ^ set(files))[:8]}")
    for name, (_, size) in files.items():
        st = os.lstat(present[name])
        if not stat.S_ISREG(st.st_mode) or st.st_size != size:
            raise Failure("model", "model_file_size", f"{name} is not the pinned {size}-byte file")
    return digest


def _read_sys(path: Path) -> str | None:
    try:
        return path.read_text(encoding="ascii", errors="replace").strip()[:128]
    except OSError:
        return None


def check_render_node(paths: Paths, diag: dict[str, Any]) -> None:
    """Exactly /dev/dri/renderD128 is passed, and sysfs says it is an Intel Iris Xe."""
    try:
        entries = sorted(os.listdir(paths.dev_dri))
    except OSError:
        entries = []
    diag["dev_dri"] = entries
    if entries != [RENDER_NODE]:
        raise Failure("device", "render_node_not_exclusive", f"/dev/dri has {entries}, expected only {RENDER_NODE}")
    if not stat.S_ISCHR(os.stat(paths.dev_dri / RENDER_NODE).st_mode):
        raise Failure("device", "render_node_not_char", f"{RENDER_NODE} is not a character device")
    dev = paths.sys_drm / RENDER_NODE / "device"
    vendor, device = _read_sys(dev / "vendor"), _read_sys(dev / "device")
    diag["sysfs_vendor"], diag["sysfs_device"] = vendor, device
    try:
        diag["sysfs_pci_slot"] = os.path.basename(os.path.realpath(dev))[:64]
        diag["sysfs_driver"] = os.path.basename(os.path.realpath(dev / "driver"))[:64]
    except OSError:
        diag["sysfs_pci_slot"] = diag["sysfs_driver"] = None
    if vendor != INTEL_VENDOR or device not in IRIS_XE_DEVICE_IDS:
        raise Failure("device", "render_node_not_iris_xe",
                      f"{RENDER_NODE} sysfs vendor={vendor} device={device}, expected {INTEL_VENDOR} {sorted(IRIS_XE_DEVICE_IDS)}")


def _prop(core: Any, name: str) -> Any:
    try:
        return core.get_property(c.DEVICE, name)
    except Exception:  # noqa: BLE001 - optional property on this plugin
        return None


def check_openvino_gpu(ov: Any, diag: dict[str, Any]) -> None:
    """OpenVINO sees exactly one GPU and it is the integrated Intel Iris Xe on the render node's slot."""
    core = ov.Core()
    devices = [str(d) for d in core.available_devices]
    diag["openvino_devices"] = devices[:16]
    gpus = [d for d in devices if d.split(".")[0] == "GPU"]
    if gpus not in (["GPU"], ["GPU.0"]):
        raise Failure("device", "gpu_not_single", f"OpenVINO GPU devices {gpus}")
    name = str(_prop(core, "FULL_DEVICE_NAME") or "")
    kind = str(_prop(core, "DEVICE_TYPE") or "")
    arch = str(_prop(core, "DEVICE_ARCHITECTURE") or "")
    pci = _prop(core, "DEVICE_PCI_INFO")
    memory = _prop(core, "GPU_DEVICE_TOTAL_MEM_SIZE")
    diag.update({
        "full_device_name": name[:256], "device_type": kind[:64], "device_architecture": arch[:128],
        "gpu_total_mem_bytes": memory if type(memory) is int else None,
    })
    if "Intel" not in name or "Iris(R) Xe" not in name:
        raise Failure("device", "gpu_not_iris_xe", f"OpenVINO GPU is {name!r}")
    if "INTEGRATED" not in kind.upper():
        raise Failure("device", "gpu_not_integrated", f"OpenVINO GPU type {kind!r}")
    if arch and "vendor=0x8086" not in arch:
        raise Failure("device", "gpu_not_intel", f"OpenVINO GPU architecture {arch!r}")
    slot = diag.get("sysfs_pci_slot")
    diag["pci_verification"] = "unsupported_property" if pci is None else "unsupported_fields"
    if pci is not None and slot:
        fields = [getattr(pci, k, None) for k in ("domain", "bus", "device", "function")]
        if all(type(f) is int for f in fields):
            ov_slot = "{:04x}:{:02x}:{:02x}.{:x}".format(*fields)
            diag["openvino_pci_slot"] = ov_slot
            if ov_slot != slot:
                raise Failure("device", "gpu_pci_mismatch", f"OpenVINO GPU at {ov_slot}, {RENDER_NODE} at {slot}")
            diag["pci_verification"] = "matched"


def _versions(engine: dict[str, Any]) -> tuple[Any, Any]:
    try:
        import openvino as ov
        import openvino_genai as genai
    except Exception as exc:  # noqa: BLE001
        raise Failure("load", "openvino_import", f"{type(exc).__name__}: {exc}") from None
    engine["openvino"] = str(getattr(ov, "__version__", None) or ov.get_version())[:128]
    engine["openvino_genai"] = str(getattr(genai, "__version__", None) or genai.get_version())[:128]
    if not engine["openvino"].startswith(c.OPENVINO_VERSION):
        raise Failure("load", "openvino_version", f"OpenVINO {engine['openvino']} is not {c.OPENVINO_VERSION}")
    return ov, genai


def new_result() -> dict[str, Any]:
    engine = {"worker": c.WORKER, "build_revision": (os.environ.get("USNM_IRIS_BUILD_REVISION") or None),
              "python": platform.python_version(), "openvino": None, "openvino_genai": None}
    if engine["build_revision"] is not None:
        engine["build_revision"] = engine["build_revision"][:128]
    diag: dict[str, Any] = {"render_node": RENDER_NODE}
    return {
        "v": c.VERSION, "id": None, "crop_id": None, "status": "error", "text": None, "device": c.DEVICE,
        "model_id": c.MODEL_ID, "model_revision": c.MODEL_REVISION, "model_manifest_sha256": None,
        "prompt_version": None, "worker_image": None, "host_id": None, "page_sha256": None,
        "max_new_tokens": None, "generated_tokens": None, "finish_reason": None, "elapsed_ms": 0, "load_ms": None,
        "generate_ms": None, "engine": engine, "diagnostics": diag, "error": None,
    }


def generate_one(pipe: Any, ov: Any, result: dict[str, Any], prompt_version: str, max_new_tokens: int,
                 pixels: Any) -> None:
    """One generation on a loaded pipeline (no chat history: each call is independent); fills result."""
    diag = result["diagnostics"]
    generate_start = time.monotonic()
    try:
        output = pipe.generate(c.PROMPTS[prompt_version], image=ov.Tensor(pixels),
                               max_new_tokens=max_new_tokens, do_sample=False)
    except Exception as exc:  # noqa: BLE001
        raise Failure("generate", "generate_failed", f"{type(exc).__name__}: {exc}") from None
    result["generate_ms"] = int((time.monotonic() - generate_start) * 1000)
    texts = getattr(output, "texts", None)
    text = str(texts[0]) if texts else str(output)
    try:
        result["generated_tokens"] = int(output.perf_metrics.get_num_generated_tokens())
    except Exception:  # noqa: BLE001
        raise Failure("generate", "token_count_unavailable", "perf_metrics has no generated token count") from None
    reasons = getattr(output, "finish_reasons", None)
    if reasons:
        reason = getattr(reasons[0], "name", str(reasons[0]).split(".")[-1])
        diag["native_finish_reason"] = reason[:64]
        if reason not in ("STOP", "LENGTH"):
            raise Failure("generate", "unsupported_finish_reason", f"generation ended with {reason}")
        result["finish_reason"] = "length" if reason == "LENGTH" else "stop"
        diag["finish_reason_source"] = "native"
    else:
        result["finish_reason"] = "stop"
        diag["finish_reason_source"] = "generated_token_count"
    if result["generated_tokens"] >= max_new_tokens:
        result["finish_reason"] = "length"
    diag["text_chars"] = len(text)
    if result["finish_reason"] == "length":
        raise Failure("generate", "token_limit_reached",
                      f"{result['generated_tokens']} tokens reached max_new_tokens; transcription incomplete")
    if not text.strip():
        raise Failure("generate", "empty_transcription", "the model returned no text")
    if len(text) > c.MAX_TEXT_CHARS:
        raise Failure("output", "text_too_long", f"{len(text)} characters")
    result["text"] = text
    result["status"] = "ok"


def run(paths: Paths = Paths()) -> dict[str, Any]:
    """The inference: validates everything, then one generation on the GPU."""
    start = time.monotonic()
    result = new_result()
    engine, diag = result["engine"], result["diagnostics"]
    try:
        try:
            job = c.parse_job_bytes(read_bounded(paths.work / c.JOB_FILE, c.MAX_JOB_BYTES, "input", "job_unreadable"))
        except c.ContractError as exc:
            raise Failure("input", "job_invalid", str(exc)) from None
        for key in ("id", "crop_id", "worker_image", "prompt_version", "max_new_tokens"):
            result[key] = job[key]
        hostid = boinc_hostid(paths.work)
        result["host_id"] = hostid
        if hostid != job["host_id"]:
            raise Failure("host", "host_mismatch", f"BOINC hostid {hostid} is not the job's {job['host_id']}")
        page = read_bounded(paths.work / c.PAGE_FILE, c.MAX_PAGE_BYTES, "image", "page_unreadable")
        try:
            width, height = c.check_page(job, page)
        except c.ContractError as exc:
            raise Failure("image", "page_invalid", str(exc)) from None
        result["page_sha256"] = job["page_sha256"]
        pixels = decode_rgb(page, width, height)
        result["model_manifest_sha256"] = check_model(paths)
        check_render_node(paths, diag)
        ov, genai = _versions(engine)
        check_openvino_gpu(ov, diag)
        load_start = time.monotonic()
        try:
            config = {"INFERENCE_PRECISION_HINT": "f16"}
            diag["compile_config"] = json.dumps(config, sort_keys=True)
            pipe = genai.VLMPipeline(str(paths.model_dir), c.DEVICE, **config)
        except Exception as exc:  # noqa: BLE001
            raise Failure("load", "pipeline_load", f"{type(exc).__name__}: {exc}") from None
        result["load_ms"] = int((time.monotonic() - load_start) * 1000)
        generate_one(pipe, ov, result, job["prompt_version"], job["max_new_tokens"], pixels)
    except Failure as exc:
        result["error"] = {"stage": exc.stage, "code": exc.code, "message": exc.message[:c.MAX_MESSAGE_CHARS]}
    except Exception as exc:  # noqa: BLE001 - reported, never success-shaped
        traceback.print_exc(file=sys.stderr)
        message = f"{type(exc).__name__}: {exc}"[:c.MAX_MESSAGE_CHARS]
        result["error"] = {"stage": "internal", "code": "unexpected_exception", "message": message}
    result["elapsed_ms"] = int((time.monotonic() - start) * 1000)
    return result


def crop_error(job: dict[str, Any], index: int, base: dict[str, Any], stage: str, code: str, message: str,
               elapsed_ms: int = 0) -> dict[str, Any]:
    """A per-crop error result bound to the crop's own identity."""
    single = b.crop_job(job, index)
    result = new_result()
    result["engine"], result["diagnostics"] = dict(base["engine"]), {"render_node": RENDER_NODE, "bundle_index": index}
    for key in ("id", "crop_id", "worker_image", "prompt_version", "max_new_tokens", "page_sha256"):
        result[key] = single[key]
    result["host_id"] = base.get("host_id")
    result["model_manifest_sha256"] = base.get("model_manifest_sha256")
    result["elapsed_ms"] = elapsed_ms
    result["error"] = {"stage": stage, "code": code, "message": message[:c.MAX_MESSAGE_CHARS]}
    return result


def complete_bundle(job: dict[str, Any], rows: list[dict[str, Any]], base: dict[str, Any], stage: str, code: str,
                    message: str, load_ms: int | None, elapsed_ms: int) -> dict[str, Any]:
    """Every crop not yet reported gets an explicit error; the bundle status follows from the crops."""
    rows = list(rows)
    shared = None
    if not rows:
        shared = {"stage": stage, "code": code, "message": message[:c.MAX_MESSAGE_CHARS]}
    for index in range(len(rows), len(job["crops"])):
        rows.append(crop_error(job, index, base, stage, code, message))
    return {"v": b.BUNDLE_VERSION, "id": job["id"], "status": b.status_of(rows), "host_id": base.get("host_id"),
            "worker_image": job["worker_image"], "elapsed_ms": elapsed_ms, "load_ms": load_ms, "results": rows,
            "error": shared if b.status_of(rows) == "error" and shared else None}


def run_bundle(paths: Paths = Paths(), progress: Any = None) -> dict[str, Any]:
    """One model load, then each crop in order on the GPU; a crop's failure never hides another's result.

    progress(rows) is called after every crop so the supervisor can salvage finished crops after a
    timeout or crash.
    """
    start = time.monotonic()
    base = new_result()
    engine, diag = base["engine"], base["diagnostics"]
    job = b.parse_bundle_job_bytes(read_bounded(paths.work / c.JOB_FILE, b.MAX_BUNDLE_JOB_BYTES, "input", "job_unreadable"))
    rows: list[dict[str, Any]] = []
    load_ms: int | None = None

    def elapsed() -> int:
        return int((time.monotonic() - start) * 1000)

    try:
        hostid = boinc_hostid(paths.work)
        base["host_id"] = hostid
        if hostid != job["host_id"]:
            raise Failure("host", "host_mismatch", f"BOINC hostid {hostid} is not the job's {job['host_id']}")
        pack_path = paths.work / b.PACK_FILE
        try:
            st = os.lstat(pack_path)
        except FileNotFoundError:
            raise Failure("input", "pack_missing", f"{b.PACK_FILE} is missing") from None
        if not stat.S_ISREG(st.st_mode) or st.st_size != job["pack_bytes"]:
            raise Failure("input", "pack_invalid", f"{b.PACK_FILE} is not the job's {job['pack_bytes']}-byte regular file")
        digest = hashlib.sha256()
        with open(pack_path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                digest.update(chunk)
        if digest.hexdigest() != job["pack_sha256"]:
            raise Failure("input", "pack_invalid", f"{b.PACK_FILE} does not match pack_sha256")
        base["model_manifest_sha256"] = check_model(paths)
        check_render_node(paths, diag)
        ov, genai = _versions(engine)
        check_openvino_gpu(ov, diag)
        load_start = time.monotonic()
        try:
            pipe = genai.VLMPipeline(str(paths.model_dir), c.DEVICE, INFERENCE_PRECISION_HINT="f16")
        except Exception as exc:  # noqa: BLE001
            raise Failure("load", "pipeline_load", f"{type(exc).__name__}: {exc}") from None
        load_ms = int((time.monotonic() - load_start) * 1000)
        with open(pack_path, "rb") as fh:
            for index, (offset, length) in enumerate(b.crop_slices(job)):
                single = b.crop_job(job, index)
                crop_start = time.monotonic()
                result = crop_error(job, index, base, "internal", "unset", "unset")
                result["diagnostics"].update({k: v for k, v in diag.items() if k != "render_node"})
                result["diagnostics"]["compile_config"] = '{"INFERENCE_PRECISION_HINT": "f16"}'
                result["load_ms"] = load_ms if index == 0 else 0
                try:
                    fh.seek(offset)
                    page = fh.read(length)
                    width, height = c.check_page(single, page)
                    pixels = decode_rgb(page, width, height)
                    result["error"] = None
                    generate_one(pipe, ov, result, job["prompt_version"], job["max_new_tokens"], pixels)
                except (Failure, c.ContractError) as exc:
                    stage, code = (exc.stage, exc.code) if isinstance(exc, Failure) else ("image", "page_invalid")
                    message = exc.message if isinstance(exc, Failure) else str(exc)
                    result["status"], result["text"] = "error", None
                    result["error"] = {"stage": stage, "code": code, "message": message[:c.MAX_MESSAGE_CHARS]}
                    if code == "generate_failed":
                        rows.append(result)
                        result["elapsed_ms"] = int((time.monotonic() - crop_start) * 1000)
                        return complete_bundle(job, rows, base, "generate", "bundle_aborted",
                                               "an earlier crop's generate() raised; pipeline state is untrusted",
                                               load_ms, elapsed())
                finally:
                    pixels = None
                result["elapsed_ms"] = int((time.monotonic() - crop_start) * 1000) + (load_ms if index == 0 else 0)
                rows.append(result)
                if progress:
                    progress(rows)
        return complete_bundle(job, rows, base, "internal", "incomplete", "no result for this crop", load_ms, elapsed())
    except Failure as exc:
        return complete_bundle(job, rows, base, exc.stage, exc.code, exc.message, load_ms, elapsed())
    except Exception as exc:  # noqa: BLE001 - reported, never success-shaped
        traceback.print_exc(file=sys.stderr)
        return complete_bundle(job, rows, base, "internal", "unexpected_exception",
                               f"{type(exc).__name__}: {exc}", load_ms, elapsed())


def _write_file(directory: Path, name: str, data: bytes) -> None:
    target, partial = directory / name, directory / (name + ".partial")
    for path in (target, partial):
        if os.path.lexists(path) and not stat.S_ISREG(os.lstat(path).st_mode):
            raise OSError(f"{path.name} exists and is not a regular file")
    fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0), 0o644)
    with os.fdopen(fd, "wb") as fh:
        fh.write(data)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(partial, target)


def finalize(result: dict[str, Any]) -> bytes:
    """A valid encoded result; an invalid one becomes an explicit output error."""
    try:
        c.validate_result(result, None, require_ok=False)
        data = c.encode_result(result)
    except c.ContractError as exc:
        result = {**result, "status": "error", "text": None, "generated_tokens": None,
                  "diagnostics": {"render_node": RENDER_NODE},
                  "error": {"stage": "output", "code": "result_invalid", "message": str(exc)[:c.MAX_MESSAGE_CHARS]}}
        data = c.encode_result(c.validate_result(result, None, require_ok=False))
    return data


def finalize_bundle(result: dict[str, Any], job: dict[str, Any]) -> bytes:
    """A valid bundle result bound to its job; anything else is reported as an explicit bundle error."""
    try:
        return b.encode_bundle_result(b.validate_bundle_result(result, job))
    except c.ContractError as exc:
        base = new_result()
        base["host_id"] = result.get("host_id")
        message = f"bundle result invalid: {exc}"
        rebuilt = complete_bundle(job, [], base, "output", "result_invalid", message, None, int(result.get("elapsed_ms") or 0))
        return b.encode_bundle_result(b.validate_bundle_result(rebuilt, job))


PARTIAL_FILE = "iris-partial.json"


def child(paths: Paths) -> int:
    """Inference process: writes its result to the scratch directory only."""
    paths.scratch.mkdir(mode=0o700, parents=True, exist_ok=True)
    if os.path.lexists(paths.work / b.PACK_FILE):
        job = b.parse_bundle_job_bytes(read_bounded(paths.work / c.JOB_FILE, b.MAX_BUNDLE_JOB_BYTES, "input", "job_unreadable"))
        progress = lambda rows: _write_file(paths.scratch, PARTIAL_FILE, json.dumps(rows).encode("utf-8"))  # noqa: E731
        _write_file(paths.scratch, c.RESULT_FILE, finalize_bundle(run_bundle(paths, progress), job))
        return 0
    result = run(paths)
    _write_file(paths.scratch, c.RESULT_FILE, finalize(result))
    return 0


def _supervisor_error(paths: Paths, stage: str, code: str, message: str, elapsed_ms: int) -> dict[str, Any]:
    result = new_result()
    try:
        job = c.parse_job_bytes(read_bounded(paths.work / c.JOB_FILE, c.MAX_JOB_BYTES, "input", "job_unreadable"))
        for key in ("id", "crop_id", "worker_image", "prompt_version", "max_new_tokens"):
            result[key] = job[key]
    except (Failure, c.ContractError, OSError):
        pass
    result["elapsed_ms"] = elapsed_ms
    result["error"] = {"stage": stage, "code": code, "message": message[:c.MAX_MESSAGE_CHARS]}
    return result


def _salvage(paths: Paths, job: dict[str, Any]) -> list[dict[str, Any]]:
    """Per-crop results the child finished before it died, only the valid, correctly bound prefix."""
    try:
        rows = json.loads(read_bounded(paths.scratch / PARTIAL_FILE, b.MAX_BUNDLE_RESULT_BYTES, "output", "partial_missing"))
    except (Failure, ValueError, OSError):
        return []
    kept: list[dict[str, Any]] = []
    for index, row in enumerate(rows if isinstance(rows, list) else []):
        try:
            single = b.crop_job(job, index)
            row = c.validate_result(row, single)
            if any(row[k] != single[k] for k in ("id", "crop_id", "page_sha256")):
                break
        except (c.ContractError, IndexError):
            break
        kept.append(row)
    return kept


def _bundle_failure(paths: Paths, job: dict[str, Any], stage: str, code: str, message: str, elapsed_ms: int) -> dict[str, Any]:
    base = new_result()
    try:
        base["host_id"] = boinc_hostid(paths.work)
    except Failure:
        pass
    return complete_bundle(job, _salvage(paths, job), base, stage, code, message, None, elapsed_ms)


def supervise(paths: Paths, command: list[str], timeout: float) -> dict[str, Any]:
    """Runs the inference child with a hard deadline and returns its result or an explicit error."""
    start = time.monotonic()
    paths.scratch.mkdir(mode=0o700, parents=True, exist_ok=True)
    for name in (c.RESULT_FILE, PARTIAL_FILE):
        if os.path.lexists(paths.scratch / name):
            os.unlink(paths.scratch / name)
    child_result = paths.scratch / c.RESULT_FILE
    bundle_job = None
    if os.path.lexists(paths.work / b.PACK_FILE):
        try:
            bundle_job = b.parse_bundle_job_bytes(read_bounded(paths.work / c.JOB_FILE, b.MAX_BUNDLE_JOB_BYTES, "input", "job_unreadable"))
            timeout = float(b.timeout_seconds(bundle_job))
        except (Failure, c.ContractError):
            bundle_job = None
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=sys.stderr.fileno(),
                               start_new_session=True, close_fds=True)
    try:
        code = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        elapsed = int((time.monotonic() - start) * 1000)
        message = f"inference exceeded {timeout:g} s and was killed"
        if bundle_job:
            return _bundle_failure(paths, bundle_job, "timeout", "watchdog_timeout", message, elapsed)
        return _supervisor_error(paths, "timeout", "watchdog_timeout", message, elapsed)
    elapsed = int((time.monotonic() - start) * 1000)
    if code != 0:
        how = f"signal {-code}" if code < 0 else f"status {code}"
        hint = " (SIGKILL: possible configured memory limit; confirm cgroup OOM counters)" if code == -signal.SIGKILL else ""
        message = f"inference process ended by {how}{hint}"
        if bundle_job:
            return _bundle_failure(paths, bundle_job, "internal", "worker_crashed", message, elapsed)
        return _supervisor_error(paths, "internal", "worker_crashed", message, elapsed)
    try:
        if bundle_job:
            data = read_bounded(child_result, b.MAX_BUNDLE_RESULT_BYTES, "output", "child_result_missing")
            return b.parse_bundle_result_bytes(data, bundle_job)
        data = read_bounded(child_result, c.MAX_RESULT_BYTES, "output", "child_result_missing")
        return c.parse_result_bytes(data, None)
    except (Failure, c.ContractError) as exc:
        if bundle_job:
            return _bundle_failure(paths, bundle_job, "output", "child_result_invalid", str(exc), elapsed)
        return _supervisor_error(paths, "output", "child_result_invalid", str(exc), elapsed)


def print_summary(result: dict[str, Any]) -> None:
    """One bounded line on stderr: BOINC may not upload iris-result.json after a nonzero exit, but
    the stderr tail reaches the server's result.stderr_out."""
    if "results" in result:
        crops = [{"crop_id": r.get("crop_id"), "status": r.get("status"), "generate_ms": r.get("generate_ms"),
                  "generated_tokens": r.get("generated_tokens"), "code": (r.get("error") or {}).get("code")}
                 for r in result["results"]]
        line = json.dumps({"id": result.get("id"), "status": result.get("status"), "elapsed_ms": result.get("elapsed_ms"),
                           "load_ms": result.get("load_ms"), "crops": len(crops),
                           "ok": sum(r["status"] == "ok" for r in crops),
                           "failed": [r for r in crops if r["status"] != "ok"][:8]}, sort_keys=True, ensure_ascii=True)
        print("usnm_iris_bundle_result " + line[:4000], file=sys.stderr, flush=True)
        return
    keys = ("id", "crop_id", "status", "elapsed_ms", "load_ms", "generate_ms", "generated_tokens",
            "finish_reason", "host_id", "error")
    line = json.dumps({k: result.get(k) for k in keys}, sort_keys=True, ensure_ascii=True)
    print("usnm_iris_result " + line[:4000], file=sys.stderr, flush=True)


def main(argv: list[str] | None = None, paths: Paths = Paths(), command: list[str] | None = None,
         timeout: float = c.TIMEOUT_SECONDS) -> int:
    parser = argparse.ArgumentParser(description="Run one usnm_iris BOINC crop", allow_abbrev=False)
    parser.add_argument("--infer", action="store_true", help=argparse.SUPPRESS)
    try:
        args = parser.parse_args(argv)
    except SystemExit as exc:
        return int(exc.code)
    if args.infer:
        return child(paths)
    command = command or [sys.executable, "-I", str(Path(__file__).resolve()), "--infer"]
    try:
        result = supervise(paths, command, timeout)
    except Exception as exc:  # noqa: BLE001
        traceback.print_exc(file=sys.stderr)
        result = _supervisor_error(paths, "internal", "supervisor_failed", f"{type(exc).__name__}: {exc}", 0)
    try:
        data = b.encode_bundle_result(result) if "results" in result else finalize(result)
        _write_file(paths.work, c.RESULT_FILE, data)
    except Exception as exc:  # noqa: BLE001
        print_summary({**result, "status": "error", "error": {
            "stage": "output", "code": "result_write_failed", "message": f"{type(exc).__name__}: {exc}"[:c.MAX_MESSAGE_CHARS]}})
        return 3
    print_summary(result)
    # A partial bundle exits 0 so BOINC uploads the per-crop results the finished crops earned.
    return 0 if result["status"] in ("ok", "partial") else 1


if __name__ == "__main__":
    sys.exit(main())
