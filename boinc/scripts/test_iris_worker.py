"""usnm_iris contract and worker, without OpenVINO, a GPU, Pillow or the network.

OpenVINO and openvino_genai are replaced by fakes; image decoding is patched. The worker's real
engine path runs only in the image (see .github/workflows/boinc-iris.yml) and on the assigned host.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
IRIS = ROOT / "boinc/apps/iris"
sys.path.insert(0, str(IRIS))
import contract as c  # noqa: E402
import worker as w  # noqa: E402

IMAGE = c.WORKER_IMAGE_REPOSITORY + "@sha256:" + "a" * 64


def jpeg(width: int, height: int, pad: int = 0) -> bytes:
    sof = b"\xff\xc0\x00\x11\x08" + height.to_bytes(2, "big") + width.to_bytes(2, "big") + b"\x03" + b"\x01\x11\x00" * 3
    app0 = b"\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
    return b"\xff\xd8" + app0 + sof + b"\xff\xda\x00\x02" + b"\x00" * pad + b"\xff\xd9"


def job_for(page: bytes, **kw) -> dict:
    args = dict(crop_id="overproof-ca-0001", doc_id="sn84024828/1887-06-18/ed-1/seq-1",
                worker_image=IMAGE, page=page, width=825, height=429)
    args.update(kw)
    return c.make_job(**args)


class FakeTensor:
    def __init__(self, array):
        self.array = array


BASE_PROPS = {
        "FULL_DEVICE_NAME": "Intel(R) Iris(R) Xe Graphics (iGPU)",
        "DEVICE_TYPE": "Type.INTEGRATED",
        "DEVICE_ARCHITECTURE": "GPU: vendor=0x8086 arch=v12.0.0",
        "GPU_DEVICE_TOTAL_MEM_SIZE": 6 * 2**30,
}


class FakeCore:
    devices = ["CPU", "GPU"]
    props = dict(BASE_PROPS)

    @property
    def available_devices(self):
        return list(self.devices)

    def get_property(self, device, name):
        assert device == "GPU"
        if name not in self.props:
            raise RuntimeError("unsupported")
        return self.props[name]


class FakeOutput:
    def __init__(self, text, tokens):
        self.texts = [text]
        self.perf_metrics = types.SimpleNamespace(get_num_generated_tokens=lambda: tokens)
        self.finish_reasons = [types.SimpleNamespace(name=FakePipeline.finish)]


class FakePipeline:
    calls: list = []
    text = "Domingo mines. Some metallic refuse is"
    tokens = 12
    finish = "STOP"
    load_error: Exception | None = None

    def __init__(self, model, device, **props):
        FakePipeline.calls.append(("load", model, device, props))
        if FakePipeline.load_error:
            raise FakePipeline.load_error

    def generate(self, prompt, **kw):
        FakePipeline.calls.append(("generate", prompt, kw))
        return FakeOutput(FakePipeline.text, FakePipeline.tokens)


def fake_modules(version="2026.4.1-22982-abc"):
    ov = types.ModuleType("openvino")
    ov.__version__ = version
    ov.Core = FakeCore
    ov.Tensor = FakeTensor
    genai = types.ModuleType("openvino_genai")
    genai.__version__ = "2026.4.1.0-2636-abc"
    genai.VLMPipeline = FakePipeline
    return {"openvino": ov, "openvino_genai": genai}


class ContractTest(unittest.TestCase):
    def test_model_lock_matches_pinned_manifest(self):
        text = (IRIS / "model.lock").read_text()
        self.assertEqual(c.manifest_digest(text), c.MODEL_MANIFEST_SHA256)
        files = c.parse_model_lock(text)
        self.assertEqual(len(files), 24)
        self.assertIn("openvino_language_model.bin", files)

    def test_image_pins_everything_it_installs(self):
        containerfile = (IRIS / "Containerfile").read_text()
        self.assertIn("USNM_IRIS_MODEL_MANIFEST=sha256:" + c.MODEL_MANIFEST_SHA256, containerfile)
        self.assertIn("MODEL_REVISION=" + c.MODEL_REVISION, containerfile)
        self.assertRegex(containerfile, r"debian:trixie-slim@sha256:[0-9a-f]{64}")
        self.assertIn("--require-hashes --no-deps", containerfile)
        for line in (IRIS / "requirements.lock").read_text().splitlines():
            if line and not line.startswith("#"):
                self.assertRegex(line, r"^[a-z0-9-]+==[0-9.]+ --hash=sha256:[0-9a-f]{64}$")
        names = []
        for line in (IRIS / "runtime.lock").read_text().splitlines():
            if line and not line.startswith("#"):
                digest, url, name = line.split(" ")
                self.assertRegex(digest, "^[0-9a-f]{64}$")
                self.assertTrue(url.startswith("https://"))
                names.append(name)
        self.assertIn("intel-opencl-icd_25.18.33578.6-0_amd64.deb", names)
        self.assertIn("libigdgmm12_22.7.0_amd64.deb", names)
        self.assertIn("intel-igc-opencl-2_2.11.7+19146_amd64.deb", names)

    def test_job_identity_is_canonical_and_covers_every_field(self):
        page = jpeg(825, 429)
        job = job_for(page)
        self.assertEqual(job["id"], hashlib.sha256(json.dumps(
            {k: v for k, v in job.items() if k != "id"}, sort_keys=True, separators=(",", ":"), ensure_ascii=True
        ).encode()).hexdigest())
        self.assertEqual(c.canonical_job_id(job), job["id"])
        data = c.encode_job(job)
        self.assertEqual(c.parse_job_bytes(data), job)
        for key, value in (("doc_id", "other"), ("max_new_tokens", 511), ("page_bytes", job["page_bytes"] + 1)):
            with self.subTest(key=key):
                with self.assertRaisesRegex(ValueError, "identity"):
                    c.validate_job({**job, key: value})

    def test_job_rejects_out_of_contract_values(self):
        job = job_for(jpeg(825, 429))
        bad = {
            "host_id": 8, "max_new_tokens": 513, "model_revision": "main", "prompt_version": "v2",
            "worker_image": "ghcr.io/evil/x@sha256:" + "a" * 64, "crop_id": "../x", "doc_id": "",
            "box": [1, 0, 825, 429], "v": 2, "page_bytes": True,
        }
        for key, value in bad.items():
            with self.subTest(key=key):
                candidate = {**job, key: value}
                candidate["id"] = c.canonical_job_id(candidate)
                with self.assertRaises(c.ContractError):
                    c.validate_job(candidate)
        with self.assertRaises(c.ContractError):
            c.validate_job({**job, "extra": 1})
        self.assertEqual(c.parse_job_bytes((json.dumps(job, sort_keys=True, indent=2) + "\n").encode()), job)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            c.parse_job_bytes(b'{"v":1,"v":1}')
        with self.assertRaises(ValueError):
            c.parse_job_bytes(b" " * (c.MAX_JOB_BYTES + 1))

    def test_page_checks_bytes_hash_and_whole_box(self):
        page = jpeg(825, 429)
        job = job_for(page)
        self.assertEqual(c.check_page(job, page), (825, 429))
        with self.assertRaises(ValueError):
            c.check_page(job, page + b"x")
        with self.assertRaises(ValueError):
            job_for(page, width=800)
        for size in ((8, 100), (5000, 100), (2100, 2100)):
            with self.subTest(size=size), self.assertRaises(ValueError):
                c.jpeg_dimensions(jpeg(*size))
        with self.assertRaises(ValueError):
            c.jpeg_dimensions(b"\x89PNG\r\n\x1a\n")


class WorkerTest(unittest.TestCase):
    def setUp(self):
        (ROOT / "target").mkdir(exist_ok=True)
        tmp = tempfile.TemporaryDirectory(prefix="iris-test-", dir=ROOT / "target")
        self.addCleanup(tmp.cleanup)
        base = Path(tmp.name)
        self.paths = w.Paths(work=base / "work", model_dir=base / "model", model_lock=IRIS / "model.lock",
                             dev_dri=base / "dev/dri", sys_drm=base / "sys/class/drm", scratch=base / "scratch")
        self.paths.work.mkdir()
        self.paths.model_dir.mkdir()
        for name, (_, size) in c.parse_model_lock((IRIS / "model.lock").read_text()).items():
            with open(self.paths.model_dir / name, "wb") as fh:
                fh.truncate(size)  # sparse: names and sizes only
        self.paths.dev_dri.mkdir(parents=True)
        os.symlink("/dev/null", self.paths.dev_dri / "renderD128")  # a character device stand-in
        pci = base / "sys/devices/pci0000:00/0000:00:02.0"
        pci.mkdir(parents=True)
        (pci / "vendor").write_text("0x8086\n")
        (pci / "device").write_text("0x9a49\n")
        (base / "sys/bus/pci/drivers/i915").mkdir(parents=True)
        os.symlink(base / "sys/bus/pci/drivers/i915", pci / "driver")
        (base / "sys/class/drm/renderD128").mkdir(parents=True)
        os.symlink(pci, base / "sys/class/drm/renderD128/device")
        self.page = jpeg(825, 429, pad=64)
        self.job = job_for(self.page)
        (self.paths.work / "iris.json").write_bytes(c.encode_job(self.job))
        (self.paths.work / "page.jpg").write_bytes(self.page)
        (self.paths.work / "init_data.xml").write_text("<app_init_data>\n<hostid>9</hostid>\n</app_init_data>\n")
        FakePipeline.calls = []
        FakePipeline.text = "Domingo mines. Some metallic refuse is"
        FakePipeline.tokens = 12
        FakePipeline.load_error = None
        FakePipeline.finish = "STOP"
        FakeCore.devices = ["CPU", "GPU"]
        FakeCore.props = dict(BASE_PROPS)
        self.modules = patch.dict(sys.modules, fake_modules())
        self.modules.start()
        self.addCleanup(self.modules.stop)
        decode = patch.object(w, "decode_rgb", return_value="pixels")
        decode.start()
        self.addCleanup(decode.stop)

    def run_worker(self):
        """The inference child in-process (the fakes do not cross a process boundary)."""
        with patch("sys.stderr"):
            self.assertEqual(w.main(["--infer"], self.paths), 0)
        data = (self.paths.scratch / c.RESULT_FILE).read_bytes()
        self.assertLessEqual(len(data), c.MAX_RESULT_BYTES)
        result = c.parse_result_bytes(data, self.job)
        return (0 if result["status"] == "ok" else 1), result

    def assert_error(self, stage, code):
        status, result = self.run_worker()
        self.assertEqual(status, 1)
        self.assertEqual(result["status"], "error")
        self.assertIsNone(result["text"])
        self.assertEqual((result["error"]["stage"], result["error"]["code"]), (stage, code), result["error"])
        self.assertEqual(c.validate_result(result, self.job), result)
        with self.assertRaises(ValueError):
            c.validate_result(result, self.job, require_ok=True)
        return result

    def test_success_runs_on_gpu_only_and_validates_on_the_server(self):
        status, result = self.run_worker()
        self.assertEqual(status, 0, result["error"])
        self.assertEqual(c.validate_result(result, self.job), result)
        self.assertEqual(result["text"], FakePipeline.text)
        self.assertEqual((result["id"], result["worker_image"], result["host_id"]), (self.job["id"], IMAGE, 9))
        self.assertEqual(result["model_manifest_sha256"], c.MODEL_MANIFEST_SHA256)
        self.assertEqual(result["diagnostics"]["sysfs_pci_slot"], "0000:00:02.0")
        self.assertEqual(result["diagnostics"]["full_device_name"], "Intel(R) Iris(R) Xe Graphics (iGPU)")
        self.assertEqual((result["finish_reason"], result["generated_tokens"]), ("stop", 12))
        load, gen = FakePipeline.calls
        self.assertEqual(load[1:3], (str(self.paths.model_dir), "GPU"))
        self.assertEqual(load[3], {"INFERENCE_PRECISION_HINT": "f16"})
        self.assertEqual(result["diagnostics"]["finish_reason_source"], "native")
        self.assertEqual(result["diagnostics"]["pci_verification"], "unsupported_property")
        self.assertEqual(gen[1], c.PROMPTS["iris-ocr-v1"])
        self.assertEqual((gen[2]["max_new_tokens"], gen[2]["do_sample"]), (512, False))
        self.assertFalse((self.paths.scratch / (c.RESULT_FILE + ".partial")).exists())

    def test_server_rejects_results_not_bound_to_the_job(self):
        _, result = self.run_worker()
        other = job_for(self.page, crop_id="overproof-ca-0002")
        with self.assertRaisesRegex(ValueError, "does not match"):
            c.validate_result(result, other)
        for key, value in (("model_manifest_sha256", "0" * 64), ("device", "CPU"), ("text", " "),
                           ("generated_tokens", 512), ("finish_reason", "length"), ("finish_reason", None),
                           ("host_id", 8)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                c.validate_result({**result, key: value}, self.job)
        with self.assertRaises(ValueError):
            c.parse_result_bytes(b"{" + b" " * c.MAX_RESULT_BYTES + b"}", self.job)

    def test_no_cpu_fallback_when_gpu_is_missing_or_not_iris_xe(self):
        FakeCore.devices = ["CPU"]
        self.assert_error("device", "gpu_not_single")
        FakeCore.devices = ["CPU", "GPU.0", "GPU.1"]
        self.assert_error("device", "gpu_not_single")
        FakeCore.devices = ["CPU", "GPU"]
        FakeCore.props["FULL_DEVICE_NAME"] = "Intel(R) UHD Graphics 620"
        self.assert_error("device", "gpu_not_iris_xe")
        self.assertEqual(FakePipeline.calls, [])

    def test_render_node_must_be_exclusive_intel_iris_xe(self):
        os.symlink("/dev/null", self.paths.dev_dri / "card0")
        self.assert_error("device", "render_node_not_exclusive")
        os.unlink(self.paths.dev_dri / "card0")
        (self.paths.sys_drm / "renderD128/device/device").write_text("0x56a0\n")
        self.assert_error("device", "render_node_not_iris_xe")

    def test_openvino_pci_slot_must_match_render_node(self):
        FakeCore.props["DEVICE_PCI_INFO"] = types.SimpleNamespace(domain=0, bus=3, device=0, function=0)
        self.assert_error("device", "gpu_pci_mismatch")
        FakeCore.props["DEVICE_PCI_INFO"] = types.SimpleNamespace(domain=0, bus=0, device=2, function=0)
        status, result = self.run_worker()
        self.assertEqual(status, 0)
        self.assertEqual(result["diagnostics"]["openvino_pci_slot"], "0000:00:02.0")
        self.assertEqual(result["diagnostics"]["pci_verification"], "matched")
        FakeCore.props["DEVICE_PCI_INFO"] = object()
        _, result = self.run_worker()
        self.assertEqual(result["diagnostics"]["pci_verification"], "unsupported_fields")

    def test_native_finish_reason_prevents_incomplete_success(self):
        FakePipeline.finish = "LENGTH"
        self.assert_error("generate", "token_limit_reached")
        FakePipeline.finish = "NONE"
        self.assert_error("generate", "unsupported_finish_reason")

    def test_inputs_are_bounded_regular_files(self):
        (self.paths.work / "page.jpg").unlink()
        os.symlink("/etc/passwd", self.paths.work / "page.jpg")
        self.assert_error("image", "page_unreadable")
        (self.paths.work / "page.jpg").unlink()
        (self.paths.work / "page.jpg").write_bytes(self.page + b"x")
        self.assert_error("image", "page_invalid")
        (self.paths.work / "iris.json").write_bytes(b"{}")
        result = self.assert_error("input", "job_invalid")
        self.assertIsNone(result["id"])

    def test_host_must_be_the_assigned_host(self):
        (self.paths.work / "init_data.xml").write_text("<app_init_data><hostid>8</hostid></app_init_data>")
        result = self.assert_error("host", "host_mismatch")
        self.assertEqual(result["host_id"], 8)
        (self.paths.work / "init_data.xml").unlink()
        self.assert_error("host", "init_data_unreadable")

    def test_model_must_match_manifest(self):
        with open(self.paths.model_dir / "openvino_language_model.bin", "ab") as fh:
            fh.write(b"x")
        self.assert_error("model", "model_file_size")
        (self.paths.model_dir / "extra.bin").write_bytes(b"")
        self.assert_error("model", "model_files_differ")

    def test_engine_failures_are_explicit(self):
        self.modules.stop()
        with patch.dict(sys.modules, fake_modules(version="2025.1.0")):
            self.assert_error("load", "openvino_version")
        self.modules.start()
        FakePipeline.load_error = RuntimeError("CL_OUT_OF_RESOURCES")
        result = self.assert_error("load", "pipeline_load")
        self.assertIn("CL_OUT_OF_RESOURCES", result["error"]["message"])
        FakePipeline.load_error = None
        FakePipeline.text = "  "
        self.assert_error("generate", "empty_transcription")

    def test_token_limit_is_never_ok(self):
        FakePipeline.tokens = 512
        result = self.assert_error("generate", "token_limit_reached")
        self.assertEqual((result["finish_reason"], result["generated_tokens"]), ("length", 512))

    def supervise(self, script, timeout=30.0):
        with patch("sys.stderr"):
            code = w.main([], self.paths, command=[sys.executable, "-c", script], timeout=timeout)
        data = (self.paths.work / c.RESULT_FILE).read_bytes()
        return code, c.parse_result_bytes(data, self.job)

    def test_supervisor_returns_the_child_result(self):
        _, expected = self.run_worker()
        target = self.paths.scratch / c.RESULT_FILE
        data = target.read_bytes()
        target.write_text("stale")  # the supervisor must not read a previous unit's result
        script = f"open({str(target)!r}, 'wb').write({data!r})"
        code, result = self.supervise(script)
        self.assertEqual((code, result), (0, expected))
        self.assertEqual(c.validate_result(result, self.job, require_ok=True), expected)

    def test_supervisor_watchdog_kills_and_reports(self):
        start = __import__("time").monotonic()
        code, result = self.supervise("import time; time.sleep(60)", timeout=1)
        self.assertLess(__import__("time").monotonic() - start, 30)
        self.assertEqual(code, 1)
        self.assertEqual((result["error"]["stage"], result["error"]["code"]), ("timeout", "watchdog_timeout"))
        self.assertEqual(result["id"], self.job["id"])

    def test_supervisor_ignores_stale_child_result(self):
        self.run_worker()
        code, result = self.supervise("pass")
        self.assertEqual((code, result["error"]["code"]), (1, "child_result_invalid"))

    def test_supervisor_reports_crash_and_missing_result(self):
        code, result = self.supervise("import os, signal; os.kill(os.getpid(), signal.SIGKILL)")
        self.assertEqual(code, 1)
        self.assertEqual(result["error"]["code"], "worker_crashed")
        self.assertIn("configured memory limit", result["error"]["message"])
        self.assertIn("confirm cgroup OOM counters", result["error"]["message"])
        self.assertIsNone(result["host_id"])
        self.assertEqual(c.validate_result(result, self.job), result)
        code, result = self.supervise("raise SystemExit(0)")
        self.assertEqual((code, result["error"]["code"]), (1, "child_result_invalid"))

    def test_unexpected_exception_is_reported_not_success(self):
        with patch.object(w, "check_model", side_effect=KeyError("boom")), patch("sys.stderr"):
            result = self.assert_error("internal", "unexpected_exception")
        self.assertIn("boom", result["error"]["message"])

    def test_takes_no_arguments(self):
        with patch("sys.stderr"):
            self.assertEqual(w.main(["--dir", "/etc"], self.paths), 2)
            self.assertEqual(w.main(["--infer", "x"], self.paths), 2)
        self.assertFalse((self.paths.work / c.RESULT_FILE).exists())


if __name__ == "__main__":
    unittest.main()
